import AppKit
import CoreGraphics
import Foundation

@MainActor
protocol ScreenRecordingPermissionServicing: AnyObject {
    func isGranted() -> Bool
    @discardableResult func request() -> Bool
    func openSystemSettings()
}

@MainActor
final class ScreenRecordingPermissionService: ScreenRecordingPermissionServicing {
    func isGranted() -> Bool {
        CGPreflightScreenCaptureAccess()
    }

    @discardableResult
    func request() -> Bool {
        CGRequestScreenCaptureAccess()
    }

    func openSystemSettings() {
        guard let url = URL(
            string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        ) else { return }
        NSWorkspace.shared.open(url)
    }
}
