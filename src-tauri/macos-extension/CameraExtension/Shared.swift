import Foundation

// Central place for identifiers shared by the extension and (conceptually) the app.
// EDIT `teamID` to your Apple Developer Team ID; the App Group must match the entitlements
// on BOTH the app and this extension, and must be created in the Apple Developer portal.
enum Shared {
    // TODO: replace with your 10-character Apple Developer Team ID.
    static let teamID = "TEAMID"

    // Bundle id of this extension — must nest under the app's bundle id.
    static let extensionBundleID = "com.camlooper.app.CameraExtension"

    // Shared App Group used for the XPC Mach service and IOSurface handoff.
    static let appGroup = "group.com.camlooper.shared"

    // Mach service name the extension advertises and the app connects to.
    // Must live under the App Group namespace to be reachable across the sandbox.
    static let machServiceName = "\(appGroup).camera"

    // The friendly name shown in camera pickers (Zoom/Teams/OBS).
    static let cameraName = "CamLooper Virtual Camera"

    // Default stream geometry / rate. The device advertises this format.
    static let width = 1280
    static let height = 720
    static let frameRate = 30
}
