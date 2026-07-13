import AppKit
import CoreGraphics
import Foundation
import ScreenCaptureKit
import WindowResolutionCore

enum SpikeError: LocalizedError {
    case noFrontmostApplication
    case noEligibleWindow(pid_t)
    case screenRecordingPermissionMissing
    case shareableWindowMismatch(CGWindowID, Int)

    var errorDescription: String? {
        switch self {
        case .noFrontmostApplication:
            return "No frontmost application is available."
        case let .noEligibleWindow(pid):
            return "No eligible normal foreground window was found for PID \(pid)."
        case .screenRecordingPermissionMissing:
            return "Screen Recording permission is not granted to this executable host."
        case let .shareableWindowMismatch(windowID, count):
            return "Window \(windowID) mapped to \(count) ScreenCaptureKit windows; expected exactly one."
        }
    }
}

@main
struct WindowResolutionSpike {
    static func main() async {
        do {
            try await run()
        } catch {
            FileHandle.standardError.write(Data("window-resolution-spike: \(error.localizedDescription)\n".utf8))
            Foundation.exit(EXIT_FAILURE)
        }
    }

    @available(macOS 14.0, *)
    private static func run() async throws {
        guard let app = NSWorkspace.shared.frontmostApplication else {
            throw SpikeError.noFrontmostApplication
        }

        let candidates = windowCandidates()
        guard let selected = WindowSelection.firstEligible(
            for: app.processIdentifier,
            from: candidates
        ) else {
            throw SpikeError.noEligibleWindow(app.processIdentifier)
        }

        guard CGPreflightScreenCaptureAccess() else {
            throw SpikeError.screenRecordingPermissionMissing
        }

        let content = try await SCShareableContent.excludingDesktopWindows(
            true,
            onScreenWindowsOnly: true
        )
        let matches = content.windows.filter { $0.windowID == selected.windowID }
        guard matches.count == 1, let window = matches.first else {
            throw SpikeError.shareableWindowMismatch(selected.windowID, matches.count)
        }

        let filter = SCContentFilter(desktopIndependentWindow: window)
        let configuration = SCStreamConfiguration()
        configuration.showsCursor = false
        configuration.width = min(Int(window.frame.width * 2), 2_560)
        configuration.height = min(Int(window.frame.height * 2), 2_560)

        let image = try await SCScreenshotManager.captureImage(
            contentFilter: filter,
            configuration: configuration
        )

        let result: [String: Any] = [
            "frontmost_bundle_id": app.bundleIdentifier ?? "unknown",
            "frontmost_pid": app.processIdentifier,
            "selected_window_id": selected.windowID,
            "eligible_owner_window_count": candidates.filter {
                $0.ownerPID == app.processIdentifier && $0.isEligibleNormalWindow
            }.count,
            "shareable_match_count": matches.count,
            "captured_pixel_width": image.width,
            "captured_pixel_height": image.height,
            "persisted_image": false,
        ]
        let data = try JSONSerialization.data(
            withJSONObject: result,
            options: [.prettyPrinted, .sortedKeys]
        )
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }

    private static func windowCandidates() -> [WindowCandidate] {
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let rows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
            as? [[CFString: Any]] else {
            return []
        }

        return rows.compactMap { row in
            guard
                let id = row[kCGWindowNumber] as? CGWindowID,
                let ownerPID = row[kCGWindowOwnerPID] as? pid_t,
                let layer = row[kCGWindowLayer] as? Int,
                let alpha = row[kCGWindowAlpha] as? Double,
                let boundsDictionary = row[kCGWindowBounds] as? NSDictionary,
                let bounds = CGRect(
                    dictionaryRepresentation: boundsDictionary as CFDictionary
                )
            else {
                return nil
            }

            return WindowCandidate(
                windowID: id,
                ownerPID: ownerPID,
                layer: layer,
                alpha: alpha,
                bounds: bounds,
                isOnScreen: (row[kCGWindowIsOnscreen] as? Bool) ?? false
            )
        }
    }
}
