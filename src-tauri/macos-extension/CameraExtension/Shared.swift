import Foundation

// Identifiers and geometry shared across the extension. The app side
// (src-tauri/macos/camera_bridge.m) finds the device by `cameraName`, so keep the two in
// step.
enum Shared {
    // Bundle id of this extension — must nest under the app's bundle id.
    static let extensionBundleID = "com.camlooper.app.CameraExtension"

    // The friendly name shown in camera pickers (Zoom/Teams/OBS).
    static let cameraName = "CamLooper Virtual Camera"

    // Stable identities. Random ids would make every launch look like a new camera, and
    // conferencing apps would forget that the user had picked this one.
    static let deviceID = UUID(uuidString: "5F0B6C1E-9B7A-4D53-8E11-2C61F0A7C001")!
    static let sourceStreamID = UUID(uuidString: "5F0B6C1E-9B7A-4D53-8E11-2C61F0A7C002")!
    static let sinkStreamID = UUID(uuidString: "5F0B6C1E-9B7A-4D53-8E11-2C61F0A7C003")!

    // Stream geometry / rate. The device advertises exactly this format, and the app
    // renders to it (see `MACOS_CAMERA_GEOMETRY` in virtual_camera.rs).
    static let width = 1280
    static let height = 720
    static let frameRate = 30
}
