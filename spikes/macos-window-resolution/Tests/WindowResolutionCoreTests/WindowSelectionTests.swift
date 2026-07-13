import CoreGraphics
import XCTest
@testable import WindowResolutionCore

final class WindowSelectionTests: XCTestCase {
    func testSelectsFirstEligibleWindowForFrontmostPID() {
        let candidates = [
            candidate(id: 1, pid: 20),
            candidate(id: 2, pid: 10, layer: 1),
            candidate(id: 3, pid: 10),
            candidate(id: 4, pid: 10),
        ]

        XCTAssertEqual(
            WindowSelection.firstEligible(for: 10, from: candidates)?.windowID,
            3
        )
    }

    func testRejectsInvisibleTransparentAndDegenerateWindows() {
        let candidates = [
            candidate(id: 1, pid: 10, isOnScreen: false),
            candidate(id: 2, pid: 10, alpha: 0),
            candidate(id: 3, pid: 10, width: 1),
            candidate(id: 4, pid: 10, height: 1),
        ]

        XCTAssertNil(WindowSelection.firstEligible(for: 10, from: candidates))
    }

    private func candidate(
        id: CGWindowID,
        pid: pid_t,
        layer: Int = 0,
        alpha: Double = 1,
        width: CGFloat = 800,
        height: CGFloat = 600,
        isOnScreen: Bool = true
    ) -> WindowCandidate {
        WindowCandidate(
            windowID: id,
            ownerPID: pid,
            layer: layer,
            alpha: alpha,
            bounds: CGRect(x: 0, y: 0, width: width, height: height),
            isOnScreen: isOnScreen
        )
    }
}
