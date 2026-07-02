import Foundation
import CoreMediaIO
import IOKit.audio
import os.log

// A CMIO Camera Extension that publishes one virtual camera device with one stream.
// Until real frames arrive over XPC (see FrameReceiver / Stage C2), it emits a moving
// test pattern so the CMIO plumbing can be validated first (Stage C1).
//
// Structure mirrors Apple's "Creating a camera extension" sample: a ProviderSource owns
// one DeviceSource, which owns one StreamSource. The DeviceSource drives a timer that
// fills CVPixelBuffers from a pool and hands them to the stream as CMSampleBuffers.

let kFrameRate = Shared.frameRate
let kWidth = Shared.width
let kHeight = Shared.height
private let logger = Logger(subsystem: Shared.extensionBundleID, category: "Extension")

// MARK: - Stream Source

class CameraExtensionStreamSource: NSObject, CMIOExtensionStreamSource {

    private(set) var stream: CMIOExtensionStream!
    let device: CMIOExtensionDevice
    private let _streamFormat: CMIOExtensionStreamFormat

    init(localizedName: String, streamID: UUID, streamFormat: CMIOExtensionStreamFormat, device: CMIOExtensionDevice) {
        self.device = device
        self._streamFormat = streamFormat
        super.init()
        self.stream = CMIOExtensionStream(localizedName: localizedName, streamID: streamID, direction: .source, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [_streamFormat] }

    var activeFormatIndex: Int = 0 {
        didSet { if activeFormatIndex >= 1 { logger.error("Invalid format index") } }
    }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration]
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let p = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            p.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            p.frameDuration = CMTime(value: 1, timescale: Int32(kFrameRate))
        }
        return p
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {
        if let index = streamProperties.activeFormatIndex {
            self.activeFormatIndex = index
        }
    }

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool { true }

    func startStream() throws {
        guard let deviceSource = device.source as? CameraExtensionDeviceSource else {
            fatalError("Unexpected device source type")
        }
        deviceSource.startStreaming()
    }

    func stopStream() throws {
        guard let deviceSource = device.source as? CameraExtensionDeviceSource else {
            fatalError("Unexpected device source type")
        }
        deviceSource.stopStreaming()
    }
}

// MARK: - Device Source

class CameraExtensionDeviceSource: NSObject, CMIOExtensionDeviceSource {

    private(set) var device: CMIOExtensionDevice!
    private var _streamSource: CameraExtensionStreamSource!
    private var _streamingCounter: UInt32 = 0
    private var _timer: DispatchSourceTimer?
    private let _timerQueue = DispatchQueue(label: "timerQueue", qos: .userInteractive)
    private var _videoDescription: CMFormatDescription!
    private var _bufferPool: CVPixelBufferPool!
    private var _bufferAuxAttributes: NSDictionary!
    private var _frameCounter: UInt64 = 0

    init(localizedName: String) {
        super.init()
        let deviceID = UUID()
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

        let streamID = UUID()
        _streamSource = CameraExtensionStreamSource(localizedName: "\(localizedName).stream", streamID: streamID, streamFormat: videoStreamFormat, device: device)
        do {
            try device.addStream(_streamSource.stream)
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
            p.model = "CamLooper Model"
        }
        return p
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {
        // No writable device properties.
    }

    func startStreaming() {
        guard let _ = _bufferPool else { return }
        _streamingCounter += 1
        _timer = DispatchSource.makeTimerSource(flags: .strict, queue: _timerQueue)
        _timer!.schedule(deadline: .now(), repeating: 1.0 / Double(kFrameRate), leeway: .seconds(0))
        _timer!.setEventHandler { [weak self] in
            self?.emitFrame()
        }
        _timer!.resume()
    }

    func stopStreaming() {
        if _streamingCounter > 1 {
            _streamingCounter -= 1
        } else {
            _streamingCounter = 0
            _timer?.cancel()
            _timer = nil
        }
    }

    // Produces one frame. If FrameReceiver has a fresh app frame, it is used; otherwise a
    // moving test pattern is drawn so the device is verifiable without the app (Stage C1).
    private func emitFrame() {
        var pixelBuffer: CVPixelBuffer?
        let err = CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(kCFAllocatorDefault, _bufferPool, _bufferAuxAttributes, &pixelBuffer)
        guard err == kCVReturnSuccess, let pb = pixelBuffer else {
            logger.error("Failed to allocate pixel buffer: \(err)")
            return
        }

        CVPixelBufferLockBaseAddress(pb, [])
        if let latest = FrameReceiver.shared.copyLatestBGRA(width: kWidth, height: kHeight),
           let base = CVPixelBufferGetBaseAddress(pb) {
            let stride = CVPixelBufferGetBytesPerRow(pb)
            latest.withUnsafeBytes { src in
                for row in 0..<kHeight {
                    memcpy(base.advanced(by: row * stride), src.baseAddress!.advanced(by: row * kWidth * 4), kWidth * 4)
                }
            }
        } else {
            drawTestPattern(into: pb)
        }
        CVPixelBufferUnlockBaseAddress(pb, [])

        var sbuf: CMSampleBuffer?
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: Int32(kFrameRate)),
            presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid)
        var formatDesc: CMFormatDescription?
        CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: pb, formatDescriptionOut: &formatDesc)
        CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pb, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: formatDesc!,
            sampleTiming: &timing, sampleBufferOut: &sbuf)

        if let sb = sbuf {
            _streamSource.stream.send(sb, discontinuity: [], hostTimeInNanoseconds: UInt64(timing.presentationTimeStamp.seconds * Double(NSEC_PER_SEC)))
        }
        _frameCounter += 1
    }

    private func drawTestPattern(into pb: CVPixelBuffer) {
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let t = Int(_frameCounter)
        let ptr = base.assumingMemoryBound(to: UInt8.self)
        for y in 0..<kHeight {
            for x in 0..<kWidth {
                let o = y * stride + x * 4
                ptr[o + 0] = UInt8((x + t) & 0xFF)          // B
                ptr[o + 1] = UInt8((y + t) & 0xFF)          // G
                ptr[o + 2] = UInt8((x ^ y) & 0xFF)          // R
                ptr[o + 3] = 255                            // A
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
        FrameReceiver.shared.start()
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
