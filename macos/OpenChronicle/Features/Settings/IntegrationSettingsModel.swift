import CryptoKit
import Foundation

enum SettingsReceiptStatus: String, Equatable, Sendable {
    case notConnected = "not-connected"
    case exact
    case desktopCredentialCached = "desktop-credential-cached"
    case incomplete
    case mismatch
    case unsupported

    var label: String {
        switch self {
        case .notConnected: "Not connected"
        case .exact: "Verified receipt"
        case .desktopCredentialCached: "Provisioned scope cached"
        case .incomplete: "Setup incomplete"
        case .mismatch: "Receipt needs repair"
        case .unsupported: "Unsupported client version"
        }
    }
}

struct SettingsCachedGrantScope: Equatable, Sendable {
    let expiresAt: Date?
    let rollingHorizonSeconds: UInt64
    let allowsOCR: Bool
    let maxPageItems: UInt32
    let maxResponseBytes: UInt64
    let maxCumulativeBytes: UInt64
}

enum SettingsIntegrationCopy {
    static let cachedScopeExplanation =
        "Cached provisioning record only. Chronicle verifies access when the client connects; this is not live core grant status or usage."
}

struct SettingsIntegrationSnapshot: Identifiable, Equatable, Sendable {
    var id: AgentKind { kind }
    let kind: AgentKind
    let detected: Bool
    let version: String?
    let supported: Bool
    let receiptStatus: SettingsReceiptStatus
    let cachedGrantScope: SettingsCachedGrantScope?
    let hasDuplicateInstallations: Bool
    let canConnect: Bool
    let canRepair: Bool
    let canUnregister: Bool
    let canEditAccess: Bool
    let canRevoke: Bool
    let canPrepareDesktopPackage: Bool
}

enum GrantHorizonOption: UInt64, CaseIterable, Identifiable, Sendable {
    case oneHour = 3_600
    case oneDay = 86_400
    case sevenDays = 604_800

    var id: UInt64 { rawValue }
    var label: String {
        switch self {
        case .oneHour: "Last hour"
        case .oneDay: "Last 24 hours"
        case .sevenDays: "Last 7 days"
        }
    }
}

enum GrantExpiryOption: UInt64, CaseIterable, Identifiable, Sendable {
    case oneDay = 86_400
    case sevenDays = 604_800
    case thirtyDays = 2_592_000

    var id: UInt64 { rawValue }
    var label: String {
        switch self {
        case .oneDay: "1 day"
        case .sevenDays: "7 days"
        case .thirtyDays: "30 days"
        }
    }
}

enum GrantPageLimitOption: UInt32, CaseIterable, Identifiable, Sendable {
    case twentyFive = 25
    case fifty = 50
    case oneHundred = 100

    var id: UInt32 { rawValue }
}

enum GrantResponseLimitOption: UInt64, CaseIterable, Identifiable, Sendable {
    case oneHundredTwentyEightKiB = 131_072
    case twoHundredFiftySixKiB = 262_144
    case oneMiB = 1_048_576

    var id: UInt64 { rawValue }
}

enum GrantCumulativeLimitOption: UInt64, CaseIterable, Identifiable, Sendable {
    case sixteenMiB = 16_777_216
    case sixtyFourMiB = 67_108_864
    case twoHundredFiftySixMiB = 268_435_456

    var id: UInt64 { rawValue }
}

struct SettingsGrantDraft: Equatable, Sendable {
    var horizon: GrantHorizonOption
    var expiry: GrantExpiryOption
    var allowOCR: Bool
    var pageLimit: GrantPageLimitOption
    var responseLimit: GrantResponseLimitOption
    var cumulativeLimit: GrantCumulativeLimitOption

    init(cachedScope: SettingsCachedGrantScope? = nil, now: Date = Date()) {
        horizon = cachedScope.flatMap {
            GrantHorizonOption(rawValue: $0.rollingHorizonSeconds)
        } ?? .oneDay
        if let expiresAt = cachedScope?.expiresAt {
            let remaining = max(0, expiresAt.timeIntervalSince(now))
            expiry = GrantExpiryOption.allCases.min(by: {
                abs(TimeInterval($0.rawValue) - remaining) <
                    abs(TimeInterval($1.rawValue) - remaining)
            }) ?? .sevenDays
        } else {
            expiry = .sevenDays
        }
        allowOCR = cachedScope?.allowsOCR ?? false
        pageLimit = cachedScope.flatMap {
            GrantPageLimitOption(rawValue: $0.maxPageItems)
        } ?? .fifty
        responseLimit = cachedScope.flatMap {
            GrantResponseLimitOption(rawValue: $0.maxResponseBytes)
        } ?? .twoHundredFiftySixKiB
        cumulativeLimit = cachedScope.flatMap {
            GrantCumulativeLimitOption(rawValue: $0.maxCumulativeBytes)
        } ?? .sixtyFourMiB
    }

    var policy: DisclosureGrantPolicy {
        DisclosureGrantPolicy(
            rollingHorizonSeconds: horizon.rawValue,
            expiresAfter: TimeInterval(expiry.rawValue),
            allowOCR: allowOCR,
            limits: DisclosureGrantLimits(
                maxPageItems: pageLimit.rawValue,
                maxResponseBytes: responseLimit.rawValue,
                maxCumulativeBytes: cumulativeLimit.rawValue
            )
        )
    }
}

enum SettingsIntegrationError: LocalizedError, Equatable {
    case clientUnavailable
    case receiptRequired
    case setupAlreadyExists
    case credentialUnavailable
    case grantMutationFailed
    case registrationFailed(String)

    var errorDescription: String? {
        switch self {
        case .clientUnavailable:
            "The selected AI client is not currently available in a supported version."
        case .receiptRequired:
            "A matching Open Chronicle receipt is required before repair or removal. No external entry was changed."
        case .setupAlreadyExists:
            "Existing access must be explicitly revoked before creating another grant or desktop package."
        case .credentialUnavailable:
            "The protected grant credential is unavailable. Existing external settings were not changed."
        case .grantMutationFailed:
            "The disclosure grant could not be changed safely."
        case let .registrationFailed(message):
            message
        }
    }
}

@MainActor
protocol SettingsIntegrationManaging: AnyObject {
    func scan(at date: Date) async -> [SettingsIntegrationSnapshot]
    func connect(kind: AgentKind, policy: DisclosureGrantPolicy) async throws -> String
    func repair(kind: AgentKind) async throws -> String
    func replaceGrant(kind: AgentKind, policy: DisclosureGrantPolicy) async throws -> String
    func unregister(kind: AgentKind) async throws -> String
    func revoke(kind: AgentKind) async throws -> String
    func createClaudeDesktopPackage(
        at destination: URL,
        policy: DisclosureGrantPolicy
    ) async throws -> MCPBPackageReceipt
}

@MainActor
protocol SettingsAgentConnecting: AnyObject {
    func connect(
        _ installation: AgentInstallation,
        policy: DisclosureGrantPolicy
    ) async -> AgentRegistrationOutcome
}

extension AgentConnectionService: SettingsAgentConnecting {}

protocol MCPBPackageCreating: Sendable {
    func createPackage(
        at selectedURL: URL,
        managedRootURL: URL,
        grant: DisclosureGrantRecord
    ) throws -> MCPBPackageReceipt
}

extension MCPBPackageService: MCPBPackageCreating {}

@MainActor
final class SettingsIntegrationService: SettingsIntegrationManaging {
    private let detector: any AgentDetecting
    private let connection: any SettingsAgentConnecting
    private let registration: any AgentRegistering
    private let grants: any DisclosureGrantServicing
    private let credentials: any AgentGrantCredentialStoring
    private let receipts: any AgentRegistrationReceiptStoring
    private let packageService: any MCPBPackageCreating
    private let applicationBundleURL: URL
    private let helperURL: URL
    private let managedRootURL: URL
    private let now: () -> Date
    private var installations: [AgentKind: AgentInstallation] = [:]

    init(
        detector: any AgentDetecting,
        connection: any SettingsAgentConnecting,
        registration: any AgentRegistering,
        grants: any DisclosureGrantServicing,
        credentials: any AgentGrantCredentialStoring,
        receipts: any AgentRegistrationReceiptStoring,
        packageService: any MCPBPackageCreating,
        applicationBundleURL: URL,
        helperURL: URL,
        managedRootURL: URL,
        now: @escaping () -> Date = Date.init
    ) {
        self.detector = detector
        self.connection = connection
        self.registration = registration
        self.grants = grants
        self.credentials = credentials
        self.receipts = receipts
        self.packageService = packageService
        self.applicationBundleURL = applicationBundleURL
        self.helperURL = helperURL
        self.managedRootURL = managedRootURL
        self.now = now
    }

    static func live(
        core: any CoreService,
        applicationBundleURL: URL,
        helperURL: URL,
        managedRootURL: URL
    ) -> SettingsIntegrationService {
        let grantService = CoreDisclosureGrantService(core: core)
        let registration = AgentRegistrationService()
        let credentials = KeychainAgentGrantCredentialStore()
        let receipts = UserDefaultsAgentRegistrationReceiptStore()
        let connection = AgentConnectionService(
            grants: grantService,
            registration: registration,
            credentials: credentials,
            applicationBundleURL: applicationBundleURL,
            helperURL: helperURL,
            managedRootURL: managedRootURL
        )
        return SettingsIntegrationService(
            detector: AgentDetectionService(),
            connection: connection,
            registration: registration,
            grants: grantService,
            credentials: credentials,
            receipts: receipts,
            packageService: MCPBPackageService(
                applicationBundleURL: applicationBundleURL,
                packageVersion: Bundle.main.object(
                    forInfoDictionaryKey: "CFBundleShortVersionString"
                ) as? String ?? "0.1.0"
            ),
            applicationBundleURL: applicationBundleURL,
            helperURL: helperURL,
            managedRootURL: managedRootURL
        )
    }

    func scan(at date: Date = Date()) async -> [SettingsIntegrationSnapshot] {
        let detected = await detector.detect()
        installations = Dictionary(uniqueKeysWithValues: detected.map { ($0.kind, $0) })
        return AgentKind.allCases.map { snapshot(kind: $0, at: date) }
    }

    func connect(kind: AgentKind, policy: DisclosureGrantPolicy) async throws -> String {
        guard let installation = installations[kind], installation.support == .supported else {
            throw SettingsIntegrationError.clientUnavailable
        }
        guard try credentials.credential(for: kind) == nil,
              receipts.receipt(for: kind) == nil
        else { throw SettingsIntegrationError.setupAlreadyExists }
        return try message(for: await connection.connect(installation, policy: policy))
    }

    func repair(kind: AgentKind) async throws -> String {
        let context = try exactContext(kind: kind)
        return try message(for: await connection.connect(
            context.installation,
            policy: policy(from: context.credential.grant)
        ))
    }

    func replaceGrant(kind: AgentKind, policy: DisclosureGrantPolicy) async throws -> String {
        guard kind != .claudeDesktop else {
            throw SettingsIntegrationError.receiptRequired
        }
        let context = try exactContext(kind: kind)
        let removed = await registration.unregister(
            installation: context.installation,
            plan: context.plan
        )
        guard removed == .removed else { return try message(for: removed) }
        try await revokeAndForget(context.credential)
        return try message(for: await connection.connect(context.installation, policy: policy))
    }

    func unregister(kind: AgentKind) async throws -> String {
        let context = try exactContext(kind: kind)
        let outcome = await registration.unregister(
            installation: context.installation,
            plan: context.plan
        )
        guard outcome == .removed else { return try message(for: outcome) }
        try await revokeAndForget(context.credential)
        return "Disconnected \(kind.displayName) and revoked its disclosure grant."
    }

    func revoke(kind: AgentKind) async throws -> String {
        guard let credential = try credentials.credential(for: kind) else {
            throw SettingsIntegrationError.credentialUnavailable
        }
        if kind != .claudeDesktop, let context = try? exactContext(kind: kind) {
            _ = await registration.unregister(
                installation: context.installation,
                plan: context.plan
            )
        }
        try await revokeAndForget(credential)
        return "Revoked \(kind.displayName) access immediately."
    }

    func createClaudeDesktopPackage(
        at destination: URL,
        policy: DisclosureGrantPolicy
    ) async throws -> MCPBPackageReceipt {
        guard let installation = installations[.claudeDesktop],
              installation.support == .supported
        else { throw SettingsIntegrationError.clientUnavailable }
        guard try credentials.credential(for: .claudeDesktop) == nil else {
            throw SettingsIntegrationError.setupAlreadyExists
        }
        let instant = now()
        let grant = try await grants.provision(
            for: .claudeDesktop,
            policy: policy,
            now: instant
        )
        _ = try await grants.install(grant, now: instant)
        let credential = AgentGrantCredential(
            schemaVersion: AgentGrantCredential.schemaVersion,
            agentKind: .claudeDesktop,
            grant: grant
        )
        do {
            try credentials.save(credential)
            return try packageService.createPackage(
                at: destination,
                managedRootURL: managedRootURL,
                grant: grant
            )
        } catch let packageError {
            // Revoke first. If rollback itself fails, retain the Keychain
            // identity so the explicit Revoke action can retry cleanup.
            if (try? await grants.revoke(
                grantID: grant.grantID,
                clientID: grant.clientID,
                receiptID: grant.receiptID,
                now: now()
            )) != nil {
                try? credentials.remove(kind: .claudeDesktop)
            }
            throw packageError
        }
    }

    private func snapshot(kind: AgentKind, at date: Date) -> SettingsIntegrationSnapshot {
        let installation = installations[kind]
        let credentialResult = Result { try credentials.credential(for: kind) }
        let credential = try? credentialResult.get()
        let receipt = receipts.receipt(for: kind)
        let exact = installation.flatMap { installation in
            credential.map { credential in
                RegistrationReceiptMatcher.matches(
                    receipt: receipt,
                    installation: installation,
                    plan: plan(for: credential)
                )
            }
        } ?? false
        let receiptStatus: SettingsReceiptStatus
        if installation?.support == .unsupported {
            receiptStatus = .unsupported
        } else if kind == .claudeDesktop, credential != nil {
            receiptStatus = .desktopCredentialCached
        } else if exact {
            receiptStatus = .exact
        } else if receipt != nil, credential != nil {
            receiptStatus = .mismatch
        } else if receipt != nil || credential != nil || credentialResult.isFailure {
            receiptStatus = .incomplete
        } else {
            receiptStatus = .notConnected
        }
        let detected = installation != nil
        let supported = installation?.support == .supported
        return SettingsIntegrationSnapshot(
            kind: kind,
            detected: detected,
            version: installation?.version,
            supported: supported,
            receiptStatus: receiptStatus,
            cachedGrantScope: credential.map { cachedScope(for: $0.grant) },
            hasDuplicateInstallations: installation?.hasDuplicateExecutables == true,
            canConnect: detected
                && supported
                && receipt == nil
                && credential == nil
                && !credentialResult.isFailure,
            canRepair: kind != .claudeDesktop && exact,
            canUnregister: kind != .claudeDesktop && exact,
            canEditAccess: kind == .claudeDesktop ? false : exact,
            canRevoke: credential != nil,
            canPrepareDesktopPackage: kind == .claudeDesktop
                && detected
                && supported
                && credential == nil
                && !credentialResult.isFailure
        )
    }

    private func exactContext(kind: AgentKind) throws -> (
        installation: AgentInstallation,
        credential: AgentGrantCredential,
        plan: AgentRegistrationPlan
    ) {
        guard let installation = installations[kind],
              let credential = try credentials.credential(for: kind)
        else { throw SettingsIntegrationError.receiptRequired }
        let plan = plan(for: credential)
        guard RegistrationReceiptMatcher.matches(
            receipt: receipts.receipt(for: kind),
            installation: installation,
            plan: plan
        ) else { throw SettingsIntegrationError.receiptRequired }
        return (installation, credential, plan)
    }

    private func plan(for credential: AgentGrantCredential) -> AgentRegistrationPlan {
        AgentRegistrationPlan(
            applicationBundleURL: applicationBundleURL,
            helperURL: helperURL,
            managedRootURL: managedRootURL,
            clientID: credential.grant.clientID,
            grantID: credential.grant.grantID
        )
    }

    private func revokeAndForget(_ credential: AgentGrantCredential) async throws {
        _ = try await grants.revoke(
            grantID: credential.grant.grantID,
            clientID: credential.grant.clientID,
            receiptID: credential.grant.receiptID,
            now: now()
        )
        try credentials.remove(kind: credential.agentKind)
    }

    private func policy(from grant: DisclosureGrantRecord) -> DisclosureGrantPolicy {
        DisclosureGrantPolicy(
            rollingHorizonSeconds: grant.timeScope.seconds,
            expiresAfter: max(
                1,
                (ChronicleTimestamp.date(grant.expiresAt) ?? now()).timeIntervalSince(now())
            ),
            allowOCR: grant.contentClasses.contains(.ocr),
            limits: grant.limits
        )
    }

    private func cachedScope(for grant: DisclosureGrantRecord) -> SettingsCachedGrantScope {
        let expiry = ChronicleTimestamp.date(grant.expiresAt)
        return SettingsCachedGrantScope(
            expiresAt: expiry,
            rollingHorizonSeconds: grant.timeScope.seconds,
            allowsOCR: grant.contentClasses.contains(.ocr),
            maxPageItems: grant.limits.maxPageItems,
            maxResponseBytes: grant.limits.maxResponseBytes,
            maxCumulativeBytes: grant.limits.maxCumulativeBytes
        )
    }

    private func message(for outcome: AgentRegistrationOutcome) throws -> String {
        switch outcome {
        case .registered:
            "Connected and verified the exact Open Chronicle registration."
        case .alreadyRegistered:
            "Verified the existing exact Open Chronicle registration."
        case .removed:
            "Disconnected the exact Open Chronicle registration."
        case .guidedDesktop:
            "Claude Desktop uses the extension package flow."
        case .conflict:
            throw SettingsIntegrationError.registrationFailed(
                "A different open-chronicle entry exists. It was not changed."
            )
        case let .blocked(reason):
            throw SettingsIntegrationError.registrationFailed(reason.explanation)
        case .unsupported:
            throw SettingsIntegrationError.clientUnavailable
        case .failed:
            throw SettingsIntegrationError.registrationFailed(
                "The client could not be verified safely. No conflicting entry was overwritten."
            )
        }
    }
}

enum RegistrationReceiptMatcher {
    static func matches(
        receipt: AgentRegistrationReceipt?,
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> Bool {
        guard let receipt else { return false }
        return receipt.schemaVersion == AgentRegistrationReceipt.schemaVersion &&
            receipt.agentKind == installation.kind &&
            receipt.serverName == AgentRegistrationPlan.serverName &&
            receipt.clientID == plan.clientID &&
            receipt.resolvedHelperPath == plan.helperURL.standardizedFileURL.path &&
            receipt.managedRootPath == plan.managedRootURL.standardizedFileURL.path &&
            receipt.argumentDigest == argumentDigest(kind: installation.kind, plan: plan)
    }

    static func argumentDigest(
        kind: AgentKind,
        plan: AgentRegistrationPlan
    ) -> String {
        let fields = [kind.rawValue, plan.helperURL.standardizedFileURL.path]
            + plan.helperArguments
        let digest = SHA256.hash(data: Data(fields.joined(separator: "\u{0}").utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }
}

private extension Result {
    var isFailure: Bool {
        if case .failure = self { return true }
        return false
    }
}

@MainActor
final class IntegrationSettingsModel: ObservableObject {
    @Published private(set) var rows: [SettingsIntegrationSnapshot] = []
    @Published private(set) var isScanning = false
    @Published private(set) var activeKinds: Set<AgentKind> = []
    @Published private(set) var lastError: String?
    @Published private(set) var notice: String?
    @Published private(set) var packageReceipt: MCPBPackageReceipt?

    private var service: (any SettingsIntegrationManaging)?
    private let now: () -> Date

    init(now: @escaping () -> Date = Date.init) {
        self.now = now
    }

    func attach(service: any SettingsIntegrationManaging) {
        self.service = service
    }

    func detach() {
        service = nil
        rows = []
        lastError = nil
        notice = nil
        packageReceipt = nil
    }

    func scan() async {
        guard let service, !isScanning else { return }
        isScanning = true
        rows = await service.scan(at: now())
        isScanning = false
    }

    func connect(kind: AgentKind, draft: SettingsGrantDraft) async {
        await perform(kind: kind) { service in
            try await service.connect(kind: kind, policy: draft.policy)
        }
    }

    func repair(kind: AgentKind) async {
        await perform(kind: kind) { service in
            try await service.repair(kind: kind)
        }
    }

    func replaceGrant(kind: AgentKind, draft: SettingsGrantDraft) async {
        await perform(kind: kind) { service in
            try await service.replaceGrant(kind: kind, policy: draft.policy)
        }
    }

    func unregister(kind: AgentKind) async {
        await perform(kind: kind) { service in
            try await service.unregister(kind: kind)
        }
    }

    func revoke(kind: AgentKind) async {
        await perform(kind: kind) { service in
            try await service.revoke(kind: kind)
        }
    }

    func createClaudeDesktopPackage(
        at destination: URL,
        draft: SettingsGrantDraft
    ) async {
        guard let service else { return }
        activeKinds.insert(.claudeDesktop)
        lastError = nil
        notice = nil
        packageReceipt = nil
        defer { activeKinds.remove(.claudeDesktop) }
        do {
            let receipt = try await service.createClaudeDesktopPackage(
                at: destination,
                policy: draft.policy
            )
            packageReceipt = receipt
            notice = "Created \(receipt.packageFileName). Install it manually in Claude Desktop."
            await scan()
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func perform(
        kind: AgentKind,
        operation: (any SettingsIntegrationManaging) async throws -> String
    ) async {
        guard let service, !activeKinds.contains(kind) else { return }
        activeKinds.insert(kind)
        lastError = nil
        notice = nil
        defer { activeKinds.remove(kind) }
        do {
            notice = try await operation(service)
            await scan()
        } catch {
            lastError = error.localizedDescription
        }
    }
}
