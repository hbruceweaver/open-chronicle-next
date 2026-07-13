import CoreGraphics
import Foundation

public struct WindowCandidate: Equatable, Sendable {
    public let windowID: CGWindowID
    public let ownerPID: pid_t
    public let layer: Int
    public let alpha: Double
    public let bounds: CGRect
    public let isOnScreen: Bool

    public init(
        windowID: CGWindowID,
        ownerPID: pid_t,
        layer: Int,
        alpha: Double,
        bounds: CGRect,
        isOnScreen: Bool
    ) {
        self.windowID = windowID
        self.ownerPID = ownerPID
        self.layer = layer
        self.alpha = alpha
        self.bounds = bounds
        self.isOnScreen = isOnScreen
    }

    public var isEligibleNormalWindow: Bool {
        isOnScreen &&
            layer == 0 &&
            alpha > 0 &&
            bounds.width > 1 &&
            bounds.height > 1
    }
}

public enum WindowSelection {
    /// `CGWindowListCopyWindowInfo` returns windows in front-to-back order. Preserve
    /// that order and select the first eligible normal window owned by the frontmost
    /// application rather than sorting by geometry or window number.
    public static func firstEligible(
        for ownerPID: pid_t,
        from frontToBackCandidates: [WindowCandidate]
    ) -> WindowCandidate? {
        frontToBackCandidates.first {
            $0.ownerPID == ownerPID && $0.isEligibleNormalWindow
        }
    }
}
