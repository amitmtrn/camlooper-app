import Foundation
import CoreMediaIO

// Entry point for the CMIO Camera Extension. The provider source publishes the virtual
// camera device; startService blocks and runs the extension's run loop.
let providerSource = CameraExtensionProviderSource(clientQueue: nil)
CMIOExtensionProvider.startService(provider: providerSource.provider)

CFRunLoopRun()
