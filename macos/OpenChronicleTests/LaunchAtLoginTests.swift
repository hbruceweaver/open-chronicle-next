import XCTest
@testable import OpenChronicle

@MainActor
final class LaunchAtLoginTests: XCTestCase {
    func testEnableDisableAndApprovalNavigationFollowBackendState() async {
        let backend = LaunchBackendProbe()
        let service = LaunchAtLoginService(backend: backend)
        XCTAssertEqual(service.state, .notRegistered)

        await service.setEnabled(true)
        XCTAssertEqual(service.state, .enabled)
        XCTAssertEqual(backend.registerCalls, 1)

        backend.state = .requiresApproval
        service.refresh()
        XCTAssertEqual(service.state, .requiresApproval)
        service.openApprovalSettings()
        XCTAssertEqual(backend.settingsCalls, 1)

        await service.setEnabled(false)
        XCTAssertEqual(service.state, .notRegistered)
        XCTAssertEqual(backend.unregisterCalls, 1)
    }

    func testBackendErrorIsVisibleAndDoesNotInventEnabledState() async {
        let backend = LaunchBackendProbe()
        backend.registerError = LaunchBackendError.denied
        let service = LaunchAtLoginService(backend: backend)

        await service.setEnabled(true)

        XCTAssertEqual(service.state, .notRegistered)
        XCTAssertNotNil(service.lastError)
        XCTAssertEqual(backend.registerCalls, 1)
    }
}

@MainActor
private final class LaunchBackendProbe: LaunchAtLoginBackend {
    var state: LaunchAtLoginState = .notRegistered
    var registerError: Error?
    private(set) var registerCalls = 0
    private(set) var unregisterCalls = 0
    private(set) var settingsCalls = 0

    func register() throws {
        registerCalls += 1
        if let registerError { throw registerError }
        state = .enabled
    }

    func unregister() async throws {
        unregisterCalls += 1
        state = .notRegistered
    }

    func openSystemSettings() {
        settingsCalls += 1
    }
}

private enum LaunchBackendError: Error {
    case denied
}
