// Not shipped. This target exists so Xcode's automatic signing creates a provisioning
// profile for `com.camlooper.app` with the System Extension capability. dev-install.sh
// copies that profile into the Tauri-built app, which cargo — not Xcode — compiles.
import Foundation
print("CamLooper signing host: nothing to run.")
