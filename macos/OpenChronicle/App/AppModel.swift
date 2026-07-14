import Foundation

@MainActor
final class AppModel: ObservableObject {
    typealias CoreFactory = @Sendable (URL) async throws -> any CoreService

    @Published private(set) var health = ChronicleHealthState(status: .connecting)
    private var core: (any CoreService)?
    private var connectionTask: Task<Void, Never>?
    private let coreFactory: CoreFactory

    init(coreFactory: CoreFactory? = nil) {
        self.coreFactory = coreFactory ?? { supportURL in
            try await Task.detached(priority: .userInitiated) {
                try InProcessCore(applicationSupportURL: supportURL)
            }.value
        }
    }

    func connect() async {
        guard core == nil else { return }

        if let connectionTask {
            await connectionTask.value
            return
        }

        let task = Task { await performConnection() }
        connectionTask = task
        await task.value
        connectionTask = nil
    }

    private func performConnection() async {
        var openedCore: (any CoreService)?
        do {
            let supportURL = try Self.applicationSupportURL()
            let opened = try await coreFactory(supportURL)
            openedCore = opened
            let identity = try await opened.schemaIdentity()
            guard identity.abiSchemaVersion.hasPrefix("1.") else {
                try? await opened.close()
                openedCore = nil
                health = ChronicleHealthState(
                    status: .repairRequired("Core schema \(identity.abiSchemaVersion) is incompatible.")
                )
                return
            }
            core = opened
            openedCore = nil
            health = ChronicleHealthState(status: .ready)
        } catch {
            if let openedCore {
                try? await openedCore.close()
            }
            health = ChronicleHealthState(status: .repairRequired(error.localizedDescription))
        }
    }

    private static func applicationSupportURL() throws -> URL {
        let base = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        return base.appendingPathComponent("com.screenata.openchronicle", isDirectory: true)
    }
}
