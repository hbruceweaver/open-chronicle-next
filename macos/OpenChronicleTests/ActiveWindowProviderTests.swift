import CoreGraphics
import XCTest
@testable import OpenChronicle

final class ActiveWindowProviderTests: XCTestCase {
    func testSelectsFirstEligibleFrontToBackWindowForExactPID() {
        let result = ActiveWindowResolver.resolve(
            foreground: app(pid: 42),
            frontToBackWindows: [
                window(id: 1, pid: 99),
                window(id: 2, pid: 42, layer: 1),
                window(id: 3, pid: 42),
                window(id: 4, pid: 42),
            ],
            shareableWindows: [(3, 42), (4, 42)]
        )
        XCTAssertEqual(result, .exact(testIdentity(windowID: 3)))
    }

    func testRejectsInvisibleTransparentAndDegenerateWindows() {
        let result = ActiveWindowResolver.resolve(
            foreground: app(pid: 42),
            frontToBackWindows: [
                window(id: 1, pid: 42, alpha: 0),
                window(id: 2, pid: 42, width: 1),
                window(id: 3, pid: 42, onScreen: false),
            ],
            shareableWindows: [(1, 42), (2, 42), (3, 42)]
        )
        XCTAssertEqual(result, .noExactWindow)
    }

    func testRequiresUniqueScreenCaptureKitMatchForWindowAndPID() {
        let result = ActiveWindowResolver.resolve(
            foreground: app(pid: 42),
            frontToBackWindows: [window(id: 3, pid: 42)],
            shareableWindows: [(3, 42), (3, 42)]
        )
        XCTAssertEqual(result, .ambiguousWindow)
    }

    func testSameWindowIDFromDifferentPIDIsNotAnExactMatch() {
        let result = ActiveWindowResolver.resolve(
            foreground: app(pid: 42),
            frontToBackWindows: [window(id: 3, pid: 42)],
            shareableWindows: [(3, 99)]
        )
        XCTAssertEqual(result, .noExactWindow)
    }

    private func app(pid: pid_t) -> ForegroundApplication {
        ForegroundApplication(
            processID: pid,
            bundleIdentifier: "com.example.editor",
            processName: "Editor"
        )
    }

    private func window(
        id: UInt32,
        pid: pid_t,
        layer: Int = 0,
        alpha: Double = 1,
        width: CGFloat = 800,
        onScreen: Bool = true
    ) -> WindowMetadata {
        WindowMetadata(
            windowID: id,
            ownerPID: pid,
            layer: layer,
            alpha: alpha,
            bounds: CGRect(x: 0, y: 0, width: width, height: 600),
            isOnScreen: onScreen,
            title: "Notes"
        )
    }
}
