import Foundation
import os.log

// Receives frames from the CamLooper app and hands the latest one to the stream.
//
// Stage C1: `start()` is a no-op and `copyLatestBGRA` returns nil, so the device shows the
// test pattern — this proves the CMIO device works before any IPC exists.
//
// Stage C2: implement the XPC listener below. The recommended transport is an XPC Mach
// service (registered under the App Group `Shared.machServiceName`) that receives an
// IOSurface handle per frame (via IOSurfaceCreateXPCObject on the app side /
// IOSurfaceLookupFromXPCObject here), locks it, and copies BGRA into `_latest`. IOSurface
// gives zero-copy shared memory across the app/extension sandbox boundary.
final class FrameReceiver: NSObject {

    static let shared = FrameReceiver()
    private let logger = Logger(subsystem: Shared.extensionBundleID, category: "FrameReceiver")

    private let lock = NSLock()
    private var _latest: Data?          // BGRA, width*height*4, top-down
    private var _latestWidth = 0
    private var _latestHeight = 0

    func start() {
        // TODO (Stage C2): create an XPC Mach service listener on Shared.machServiceName
        // and, on each message, look up the IOSurface, lock it, and call store(...).
        //
        //   let listener = xpc_connection_create_mach_service(
        //       Shared.machServiceName, nil, UInt64(XPC_CONNECTION_MACH_SERVICE_LISTENER))
        //   ... set event handler, resume, per-frame: IOSurfaceLookupFromXPCObject -> copy
        //
        logger.log("FrameReceiver.start() — XPC not yet wired (Stage C2); using test pattern.")
    }

    // Called by the app-side transport once C2 is wired.
    func store(bgra: Data, width: Int, height: Int) {
        lock.lock(); defer { lock.unlock() }
        _latest = bgra
        _latestWidth = width
        _latestHeight = height
    }

    // Returns the latest frame as BGRA if it matches the requested geometry, else nil.
    func copyLatestBGRA(width: Int, height: Int) -> Data? {
        lock.lock(); defer { lock.unlock() }
        guard let data = _latest, _latestWidth == width, _latestHeight == height else { return nil }
        return data
    }
}
