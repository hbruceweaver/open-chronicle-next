import Foundation
import ServiceManagement

enum LaunchAtLoginState: Equatable, Sendable {
    case notRegistered
    case enabled
    case requiresApproval
    case notFound
}

@MainActor
protocol LaunchAtLoginBackend: AnyObject {
    var state: LaunchAtLoginState { get }
    func register() throws
    func unregister() async throws
    func openSystemSettings()
}

@MainActor
final class SystemLaunchAtLoginBackend: LaunchAtLoginBackend {
    private let service: SMAppService

    init(service: SMAppService = .mainApp) {
        self.service = service
    }

    var state: LaunchAtLoginState {
        switch service.status {
        case .notRegistered: .notRegistered
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .notFound
        @unknown default: .notFound
        }
    }

    func register() throws {
        try service.register()
    }

    func unregister() async throws {
        try await service.unregister()
    }

    func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}

@MainActor
final class LaunchAtLoginService: ObservableObject {
    @Published private(set) var state: LaunchAtLoginState
    @Published private(set) var lastError: String?
    private let backend: any LaunchAtLoginBackend

    init(backend: (any LaunchAtLoginBackend)? = nil) {
        let resolvedBackend = backend ?? SystemLaunchAtLoginBackend()
        self.backend = resolvedBackend
        state = resolvedBackend.state
    }

    func refresh() {
        state = backend.state
    }

    func setEnabled(_ enabled: Bool) async {
        lastError = nil
        do {
            if enabled {
                try backend.register()
            } else {
                try await backend.unregister()
            }
        } catch {
            lastError = error.localizedDescription
        }
        refresh()
    }

    func openApprovalSettings() {
        backend.openSystemSettings()
    }
}
