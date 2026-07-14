import Foundation
import Security

struct AgentGrantCredential: Codable, Equatable, Sendable {
    static let schemaVersion = 1

    let schemaVersion: Int
    let agentKind: AgentKind
    let grant: DisclosureGrantRecord
}

enum AgentGrantCredentialStoreError: LocalizedError, Equatable {
    case readFailed
    case writeFailed
    case deleteFailed

    var errorDescription: String? {
        "Open Chronicle could not access the protected agent credential."
    }
}

protocol AgentGrantCredentialStoring: Sendable {
    func credential(for kind: AgentKind) throws -> AgentGrantCredential?
    func save(_ credential: AgentGrantCredential) throws
    func remove(kind: AgentKind) throws
}

final class KeychainAgentGrantCredentialStore: AgentGrantCredentialStoring, @unchecked Sendable {
    private let service: String

    init(service: String = "com.screenata.openchronicle.mcp-grants") {
        self.service = service
    }

    func credential(for kind: AgentKind) throws -> AgentGrantCredential? {
        var query = baseQuery(kind: kind)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess,
              let data = item as? Data,
              let value = try? JSONDecoder().decode(AgentGrantCredential.self, from: data),
              value.schemaVersion == AgentGrantCredential.schemaVersion,
              value.agentKind == kind
        else { throw AgentGrantCredentialStoreError.readFailed }
        return value
    }

    func save(_ credential: AgentGrantCredential) throws {
        guard let data = try? JSONEncoder().encode(credential) else {
            throw AgentGrantCredentialStoreError.writeFailed
        }
        let query = baseQuery(kind: credential.agentKind)
        let updated = SecItemUpdate(
            query as CFDictionary,
            [kSecValueData as String: data] as CFDictionary
        )
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else {
            throw AgentGrantCredentialStoreError.writeFailed
        }
        var insertion = query
        insertion[kSecValueData as String] = data
        insertion[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        guard SecItemAdd(insertion as CFDictionary, nil) == errSecSuccess else {
            throw AgentGrantCredentialStoreError.writeFailed
        }
    }

    func remove(kind: AgentKind) throws {
        let status = SecItemDelete(baseQuery(kind: kind) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw AgentGrantCredentialStoreError.deleteFailed
        }
    }

    private func baseQuery(kind: AgentKind) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: kind.rawValue,
        ]
    }
}

protocol DisclosureGrantServicing: Sendable {
    func provision(
        for kind: AgentKind,
        policy: DisclosureGrantPolicy,
        now: Date
    ) async throws -> DisclosureGrantRecord
    func install(
        _ grant: DisclosureGrantRecord,
        now: Date
    ) async throws -> DisclosureGrantMutationResponse
    func revoke(
        grantID: String,
        clientID: String,
        receiptID: String,
        now: Date
    ) async throws -> DisclosureGrantMutationResponse
}

extension CoreDisclosureGrantService: DisclosureGrantServicing {}

@MainActor
protocol AgentRegistering: AnyObject {
    func register(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) async -> AgentRegistrationOutcome
    func unregister(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) async -> AgentRegistrationOutcome
}

extension AgentRegistrationService: AgentRegistering {}

@MainActor
final class AgentConnectionService {
    private let grants: any DisclosureGrantServicing
    private let registration: any AgentRegistering
    private let credentials: any AgentGrantCredentialStoring
    private let installLocation: InstallLocationService
    private let applicationBundleURL: URL
    private let helperURL: URL
    private let managedRootURL: URL
    private let now: () -> Date

    init(
        grants: any DisclosureGrantServicing,
        registration: any AgentRegistering,
        credentials: any AgentGrantCredentialStoring = KeychainAgentGrantCredentialStore(),
        installLocation: InstallLocationService = InstallLocationService(),
        applicationBundleURL: URL,
        helperURL: URL,
        managedRootURL: URL,
        now: @escaping () -> Date = Date.init
    ) {
        self.grants = grants
        self.registration = registration
        self.credentials = credentials
        self.installLocation = installLocation
        self.applicationBundleURL = applicationBundleURL
        self.helperURL = helperURL
        self.managedRootURL = managedRootURL
        self.now = now
    }

    func connect(
        _ installation: AgentInstallation,
        policy: DisclosureGrantPolicy = .onboardingDefault
    ) async -> AgentRegistrationOutcome {
        guard installation.support == .supported else { return .unsupported }
        if installation.kind == .claudeDesktop { return .guidedDesktop }
        let assessment = installLocation.assess(
            applicationBundleURL: applicationBundleURL,
            helperURL: helperURL,
            managedRootURL: managedRootURL
        )
        if case let .blocked(reason) = assessment { return .blocked(reason) }

        let instant = now()
        let credential: AgentGrantCredential
        do {
            if let existing = try credentials.credential(for: installation.kind) {
                credential = existing
                _ = try await grants.install(existing.grant, now: instant)
            } else {
                let grant = try await grants.provision(
                    for: installation.kind,
                    policy: policy,
                    now: instant
                )
                _ = try await grants.install(grant, now: instant)
                credential = AgentGrantCredential(
                    schemaVersion: AgentGrantCredential.schemaVersion,
                    agentKind: installation.kind,
                    grant: grant
                )
                try credentials.save(credential)
            }
        } catch let error as AgentGrantCredentialStoreError {
            _ = error
            return .failed(.credentialStorageFailed)
        } catch {
            return .failed(.grantFailed)
        }

        let plan = AgentRegistrationPlan(
            applicationBundleURL: applicationBundleURL,
            helperURL: helperURL,
            managedRootURL: managedRootURL,
            clientID: credential.grant.clientID,
            grantID: credential.grant.grantID
        )
        let outcome = await registration.register(installation: installation, plan: plan)
        switch outcome {
        case .registered, .alreadyRegistered:
            return outcome
        case .conflict, .blocked, .unsupported, .guidedDesktop:
            await revokeAndForget(credential)
            return outcome
        case .failed, .removed:
            // A CLI error or failed post-add verification can leave an exact
            // registration behind. Preserve the protected grant identity for
            // a deterministic repair instead of stranding a live entry on a
            // revoked credential.
            return outcome
        }
    }

    private func revokeAndForget(_ credential: AgentGrantCredential) async {
        let grant = credential.grant
        do {
            _ = try await grants.revoke(
                grantID: grant.grantID,
                clientID: grant.clientID,
                receiptID: grant.receiptID,
                now: now()
            )
            try credentials.remove(kind: credential.agentKind)
        } catch {
            // Keep the protected credential so a repair flow can retry exact
            // revocation. Never discard the only durable revocation identity.
        }
    }
}
