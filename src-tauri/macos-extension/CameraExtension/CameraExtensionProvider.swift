import Foundation
import CoreMediaIO
import IOKit.audio
import os.log

// A CMIO Camera Extension that publishes one virtual camera device with two streams:
//
//   source — what Zoom / Meet / Photo Booth read.
//   sink   — what the CamLooper app writes into (CoreMediaIO's C API, see
//            src-tauri/macos/camera_bridge.m).
//
// Frames the app enqueues on the sink are forwarded straight to the source. A sink stream
// is the transport on purpose: CoreMediaIO already carries IOSurface-backed sample buffers
// across the sandbox boundary, so the app needs no XPC service, no shared App Group and no
// second copy of each frame.
//
// While the app is not feeding the sink, the source emits a quiet placeholder so the camera
// is still a valid, visibly live device rather than a frozen or failing one.

let kFrameRate = Shared.frameRate
let kWidth = Shared.width
let kHeight = Shared.height
private let logger = Logger(subsystem: Shared.extensionBundleID, category: "Extension")

/// How long after the last app frame the placeholder takes over again.
private let kSinkStaleAfterNanos: UInt64 = 500_000_000

private func hostTimeNanos() -> UInt64 {
    UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
}

// MARK: - Stream Source (source and sink share everything but direction)

class CameraExtensionStreamSource: NSObject, CMIOExtensionStreamSource {

    private(set) var stream: CMIOExtensionStream!
    let device: CMIOExtensionDevice
    let direction: CMIOExtensionStream.Direction
    private let _streamFormat: CMIOExtensionStreamFormat
    /// The app process currently writing to the sink. Only meaningful for the sink stream.
    private(set) var client: CMIOExtensionClient?

    init(localizedName: String, streamID: UUID, direction: CMIOExtensionStream.Direction,
         streamFormat: CMIOExtensionStreamFormat, device: CMIOExtensionDevice) {
        self.device = device
        self.direction = direction
        self._streamFormat = streamFormat
        super.init()
        self.stream = CMIOExtensionStream(localizedName: localizedName, streamID: streamID,
                                          direction: direction, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [_streamFormat] }

    var activeFormatIndex: Int = 0 {
        didSet { if activeFormatIndex >= 1 { logger.error("Invalid format index") } }
    }

    var availableProperties: Set<CMIOExtensionProperty> {
        var p: Set<CMIOExtensionProperty> = [.streamActiveFormatIndex, .streamFrameDuration]
        if direction == .sink {
            p.formUnion([.streamSinkBufferQueueSize, .streamSinkBuffersRequiredForStartup,
                         .streamSinkBufferUnderrunCount, .streamSinkEndOfData])
        }
        return p
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let p = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            p.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            p.frameDuration = CMTime(value: 1, timescale: Int32(kFrameRate))
        }
        if direction == .sink {
            if properties.contains(.streamSinkBufferQueueSize) {
                p.sinkBufferQueueSize = 1
            }
            if properties.contains(.streamSinkBuffersRequiredForStartup) {
                p.sinkBuffersRequiredForStartup = 1
            }
            if properties.contains(.streamSinkBufferUnderrunCount) {
                p.sinkBufferUnderrunCount = 0
            }
            if properties.contains(.streamSinkEndOfData) {
                p.sinkEndOfData = 0
            }
        }
        return p
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {
        if let index = streamProperties.activeFormatIndex {
            self.activeFormatIndex = index
        }
    }

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        if direction == .sink {
            self.client = client
        }
        return true
    }

    func startStream() throws {
        guard let deviceSource = device.source as? CameraExtensionDeviceSource else {
            fatalError("Unexpected device source type")
        }
        if direction == .sink {
            guard let client = client else { return }
            deviceSource.startSink(client: client)
        } else {
            deviceSource.startStreaming()
        }
    }

    func stopStream() throws {
        guard let deviceSource = device.source as? CameraExtensionDeviceSource else {
            fatalError("Unexpected device source type")
        }
        if direction == .sink {
            deviceSource.stopSink()
        } else {
            deviceSource.stopStreaming()
        }
    }
}

// MARK: - Device Source

class CameraExtensionDeviceSource: NSObject, CMIOExtensionDeviceSource {

    private(set) var device: CMIOExtensionDevice!
    private var _streamSource: CameraExtensionStreamSource!
    private var _streamSink: CameraExtensionStreamSource!
    private var _timer: DispatchSourceTimer?
    private let _timerQueue = DispatchQueue(label: "timerQueue", qos: .userInteractive)
    private var _videoDescription: CMFormatDescription!
    private var _bufferPool: CVPixelBufferPool!
    private var _bufferAuxAttributes: NSDictionary!
    private var _frameCounter: UInt64 = 0

    // Shared between the timer queue and CoreMediaIO's callback queues.
    private let _lock = NSLock()
    private var _streamingCounter: UInt32 = 0
    private var _sinkStarted = false
    private var _lastSinkFrameNanos: UInt64 = 0

    init(localizedName: String) {
        super.init()
        let deviceID = Shared.deviceID
        self.device = CMIOExtensionDevice(localizedName: localizedName, deviceID: deviceID, legacyDeviceID: deviceID.uuidString, source: self)

        let dims = CMVideoDimensions(width: Int32(kWidth), height: Int32(kHeight))
        CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCVPixelFormatType_32BGRA,
            width: dims.width, height: dims.height,
            extensions: nil, formatDescriptionOut: &_videoDescription)

        let pixelBufferAttributes: NSDictionary = [
            kCVPixelBufferWidthKey: dims.width,
            kCVPixelBufferHeightKey: dims.height,
            kCVPixelBufferPixelFormatTypeKey: _videoDescription.mediaSubType,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as NSDictionary
        ]
        CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, pixelBufferAttributes, &_bufferPool)

        let videoStreamFormat = CMIOExtensionStreamFormat(
            formatDescription: _videoDescription,
            maxFrameDuration: CMTime(value: 1, timescale: Int32(kFrameRate)),
            minFrameDuration: CMTime(value: 1, timescale: Int32(kFrameRate)),
            validFrameDurations: nil)
        _bufferAuxAttributes = [kCVPixelBufferPoolAllocationThresholdKey: 5]

        _streamSource = CameraExtensionStreamSource(
            localizedName: "\(localizedName).source", streamID: Shared.sourceStreamID,
            direction: .source, streamFormat: videoStreamFormat, device: device)
        _streamSink = CameraExtensionStreamSource(
            localizedName: "\(localizedName).sink", streamID: Shared.sinkStreamID,
            direction: .sink, streamFormat: videoStreamFormat, device: device)
        do {
            try device.addStream(_streamSource.stream)
            try device.addStream(_streamSink.stream)
        } catch {
            fatalError("Failed to add stream: \(error.localizedDescription)")
        }
    }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.deviceTransportType, .deviceModel]
    }

    func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionDeviceProperties {
        let p = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) {
            p.transportType = kIOAudioDeviceTransportTypeVirtual
        }
        if properties.contains(.deviceModel) {
            p.model = "CamLooper Virtual Camera"
        }
        return p
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {
        // No writable device properties.
    }

    // MARK: Source side (a conferencing app is reading)

    func startStreaming() {
        guard let _ = _bufferPool else { return }
        _lock.lock()
        _streamingCounter += 1
        let alreadyRunning = _timer != nil
        _lock.unlock()
        if alreadyRunning { return }

        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: _timerQueue)
        timer.schedule(deadline: .now(), repeating: 1.0 / Double(kFrameRate), leeway: .seconds(0))
        timer.setEventHandler { [weak self] in
            self?.emitPlaceholderIfIdle()
        }
        _lock.lock()
        _timer = timer
        _lock.unlock()
        timer.resume()
    }

    func stopStreaming() {
        _lock.lock()
        defer { _lock.unlock() }
        if _streamingCounter > 1 {
            _streamingCounter -= 1
        } else {
            _streamingCounter = 0
            _timer?.cancel()
            _timer = nil
        }
    }

    // MARK: Sink side (the CamLooper app is writing)

    func startSink(client: CMIOExtensionClient) {
        _lock.lock()
        _sinkStarted = true
        _lock.unlock()
        logger.log("Sink started: the app is feeding frames")
        consumeBuffer(client)
    }

    func stopSink() {
        _lock.lock()
        _sinkStarted = false
        _lastSinkFrameNanos = 0
        _lock.unlock()
        logger.log("Sink stopped: back to the placeholder")
    }

    private func consumeBuffer(_ client: CMIOExtensionClient) {
        _lock.lock()
        let started = _sinkStarted
        _lock.unlock()
        guard started else { return }

        _streamSink.stream.consumeSampleBuffer(from: client) { [weak self] sbuf, seq, _, _, _ in
            guard let self = self else { return }
            guard let sbuf = sbuf else {
                // Nothing queued yet. Ask again shortly rather than spinning.
                self._timerQueue.asyncAfter(deadline: .now() + .milliseconds(4)) {
                    self.consumeBuffer(client)
                }
                return
            }
            let now = hostTimeNanos()
            self._lock.lock()
            self._lastSinkFrameNanos = now
            let hasReaders = self._streamingCounter > 0
            self._lock.unlock()

            if hasReaders {
                self._streamSource.stream.send(sbuf, discontinuity: [], hostTimeInNanoseconds: now)
            }
            let output = CMIOExtensionScheduledOutput(sequenceNumber: seq, hostTimeInNanoseconds: now)
            self._streamSink.stream.notifyScheduledOutputChanged(output)
            self.consumeBuffer(client)
        }
    }

    // MARK: Placeholder

    /// Runs at the frame rate while someone is reading. Does nothing while the app is
    /// feeding the sink, so the two never interleave.
    private func emitPlaceholderIfIdle() {
        let now = hostTimeNanos()
        _lock.lock()
        let last = _lastSinkFrameNanos
        _lock.unlock()
        if last != 0 && now &- last < kSinkStaleAfterNanos { return }

        var pixelBuffer: CVPixelBuffer?
        let err = CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(kCFAllocatorDefault, _bufferPool, _bufferAuxAttributes, &pixelBuffer)
        guard err == kCVReturnSuccess, let pb = pixelBuffer else {
            logger.error("Failed to allocate pixel buffer: \(err)")
            return
        }

        CVPixelBufferLockBaseAddress(pb, [])
        drawPlaceholder(into: pb)
        CVPixelBufferUnlockBaseAddress(pb, [])

        var sbuf: CMSampleBuffer?
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: Int32(kFrameRate)),
            presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid)
        var formatDesc: CMFormatDescription?
        CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: pb, formatDescriptionOut: &formatDesc)
        guard let fd = formatDesc else { return }
        CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pb, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: fd,
            sampleTiming: &timing, sampleBufferOut: &sbuf)

        if let sb = sbuf {
            _streamSource.stream.send(sb, discontinuity: [], hostTimeInNanoseconds: now)
        }
        _frameCounter += 1
    }

    /// A dark slate with a slow purple bar: obviously "CamLooper is idle", obviously live,
    /// and not something that embarrasses anyone if it flashes up in a meeting.
    private func drawPlaceholder(into pb: CVPixelBuffer) {
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let ptr = base.assumingMemoryBound(to: UInt8.self)

        // BGRA 0x15, 0x10, 0x10 — the app's near-black background.
        var pixel: UInt32 = 0xFF10_1015
        for y in 0..<kHeight {
            memset_pattern4(ptr + y * stride, &pixel, kWidth * 4)
        }

        let barWidth = 160
        let barHeight = 6
        let travel = kWidth + barWidth
        let x0 = Int(_frameCounter * 4 % UInt64(travel)) - barWidth
        let y0 = kHeight / 2 - barHeight / 2
        var bar: UInt32 = 0xFFA8_55F7 // purple, as BGRA bytes F7 55 A8 FF
        let from = max(0, x0)
        let to = min(kWidth, x0 + barWidth)
        if to > from {
            for y in y0..<(y0 + barHeight) {
                memset_pattern4(ptr + y * stride + from * 4, &bar, (to - from) * 4)
            }
        }
    }
}

// MARK: - Provider Source

class CameraExtensionProviderSource: NSObject, CMIOExtensionProviderSource {

    private(set) var provider: CMIOExtensionProvider!
    private var deviceSource: CameraExtensionDeviceSource!

    init(clientQueue: DispatchQueue?) {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: clientQueue)
        deviceSource = CameraExtensionDeviceSource(localizedName: Shared.cameraName)
        do {
            try provider.addDevice(deviceSource.device)
        } catch {
            fatalError("Failed to add device: \(error.localizedDescription)")
        }
    }

    func connect(to client: CMIOExtensionClient) throws {}
    func disconnect(from client: CMIOExtensionClient) {}

    var availableProperties: Set<CMIOExtensionProperty> {
        [.providerManufacturer]
    }

    func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionProviderProperties {
        let p = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) {
            p.manufacturer = "CamLooper"
        }
        return p
    }

    func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}
}
