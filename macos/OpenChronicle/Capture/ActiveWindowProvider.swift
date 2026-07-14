import AppKit
import CoreGraphics
import Foundation
import ScreenCaptureKit

struct ActiveWindowIdentity: Equatable, Hashable, Sendable {
    let processID: pid_t
    let windowID: CGWindowID
    let bundleIdentifier: String
    let processName: String
    let windowTitle: String?

    func hasSameCaptureIdentity(as other: ActiveWindowIdentity) -> Bool {
        processID == other.processID &&
            windowID == other.windowID &&
            bundleIdentifier == other.bundleIdentifier
    }
}

struct WindowMetadata: Equatable, Sendable {
    let windowID: CGWindowID
    let ownerPID: pid_t
    let layer: Int
    let alpha: Double
    let bounds: CGRect
    let isOnScreen: Bool
    let title: String?

    var isEligibleNormalWindow: Bool {
        isOnScreen &&
            layer == 0 &&
            alpha > 0 &&
            bounds.width > 1 &&
            bounds.height > 1
    }
}

struct ForegroundApplication: Equatable, Sendable {
    let processID: pid_t
    let bundleIdentifier: String
    let processName: String
}

enum ActiveWindowResolution: Equatable, Sendable {
    case exact(ActiveWindowIdentity)
    case noExactWindow
    case ambiguousWindow
}

enum ActiveWindowResolver {
    static func resolve(
        foreground: ForegroundApplication?,
        frontToBackWindows: [WindowMetadata],
        shareableWindows: [(windowID: CGWindowID, ownerPID: pid_t)]
    ) -> ActiveWindowResolution {
        guard let foreground else { return .noExactWindow }
        guard let candidate = frontToBackWindows.first(where: {
            $0.ownerPID == foreground.processID && $0.isEligibleNormalWindow
        }) else {
            return .noExactWindow
        }

        let exactMatches = shareableWindows.filter {
            $0.windowID == candidate.windowID && $0.ownerPID == foreground.processID
        }
        guard exactMatches.count == 1 else {
            return exactMatches.isEmpty ? .noExactWindow : .ambiguousWindow
        }
        return .exact(
            ActiveWindowIdentity(
                processID: foreground.processID,
                windowID: candidate.windowID,
                bundleIdentifier: foreground.bundleIdentifier,
                processName: foreground.processName,
                windowTitle: candidate.title
            )
        )
    }
}

/// The `SCWindow` never crosses a persistence or disclosure boundary. The identity
/// beside it is the exact metadata revalidated after capture.
struct ResolvedActiveWindow: @unchecked Sendable {
    let identity: ActiveWindowIdentity
    let handle: ActiveWindowHandle

    init(identity: ActiveWindowIdentity, screenCaptureWindow: SCWindow) {
        self.identity = identity
        handle = .screenCaptureKit(screenCaptureWindow)
    }

    static func testFixture(identity: ActiveWindowIdentity) -> ResolvedActiveWindow {
        ResolvedActiveWindow(identity: identity, handle: .testFixture)
    }

    private init(identity: ActiveWindowIdentity, handle: ActiveWindowHandle) {
        self.identity = identity
        self.handle = handle
    }
}

enum ActiveWindowHandle: @unchecked Sendable {
    case screenCaptureKit(SCWindow)
    case testFixture
}

enum ActiveWindowLookup: Sendable {
    case exact(ResolvedActiveWindow)
    case noExactWindow
    case ambiguousWindow

    var privacyResolution: ActiveWindowResolution {
        switch self {
        case let .exact(window): .exact(window.identity)
        case .noExactWindow: .noExactWindow
        case .ambiguousWindow: .ambiguousWindow
        }
    }
}

protocol ActiveWindowProviding: Sendable {
    func resolveActiveWindow() async throws -> ActiveWindowLookup
}

actor SystemActiveWindowProvider: ActiveWindowProviding {
    func resolveActiveWindow() async throws -> ActiveWindowLookup {
        let foreground = await MainActor.run { () -> ForegroundApplication? in
            guard let app = NSWorkspace.shared.frontmostApplication,
                  let bundleIdentifier = app.bundleIdentifier,
                  let processName = app.localizedName,
                  !bundleIdentifier.isEmpty,
                  !processName.isEmpty
            else {
                return nil
            }
            return ForegroundApplication(
                processID: app.processIdentifier,
                bundleIdentifier: bundleIdentifier,
                processName: processName
            )
        }
        guard let foreground else { return .noExactWindow }

        let windows = Self.windowMetadata()
        guard let candidate = windows.first(where: {
            $0.ownerPID == foreground.processID && $0.isEligibleNormalWindow
        }) else {
            return .noExactWindow
        }

        let content = try await SCShareableContent.excludingDesktopWindows(
            true,
            onScreenWindowsOnly: true
        )
        let matches = content.windows.filter {
            $0.windowID == candidate.windowID &&
                $0.owningApplication?.processID == foreground.processID
        }
        guard matches.count == 1, let match = matches.first else {
            return matches.isEmpty ? .noExactWindow : .ambiguousWindow
        }

        let identity = ActiveWindowIdentity(
            processID: foreground.processID,
            windowID: candidate.windowID,
            bundleIdentifier: foreground.bundleIdentifier,
            processName: foreground.processName,
            windowTitle: candidate.title
        )
        return .exact(
            ResolvedActiveWindow(identity: identity, screenCaptureWindow: match)
        )
    }

    private static func windowMetadata() -> [WindowMetadata] {
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let rows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
            as? [[CFString: Any]] else {
            return []
        }
        return rows.compactMap { row in
            guard
                let windowID = row[kCGWindowNumber] as? CGWindowID,
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
            return WindowMetadata(
                windowID: windowID,
                ownerPID: ownerPID,
                layer: layer,
                alpha: alpha,
                bounds: bounds,
                isOnScreen: (row[kCGWindowIsOnscreen] as? Bool) ?? false,
                title: (row[kCGWindowName] as? String).flatMap { $0.isEmpty ? nil : $0 }
            )
        }
    }
}
