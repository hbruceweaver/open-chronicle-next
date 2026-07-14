import Foundation
import XCTest
@testable import OpenChronicle

@MainActor
final class SettingsTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_784_016_000)

    func testViewModelLoadsAuthoritativeRuntimeValuesAndRejectsUnsupportedExclusionMutation() async {
        let runtime = StubSettingsRuntime(snapshot: makeRuntimeSnapshot())
        let integrations = StubSettingsIntegrationManager()
        let model = SettingsViewModel(now: { self.now })
        model.attach(runtime: runtime, integrations: integrations)

        await model.load()

        XCTAssertEqual(model.state, .loaded)
        XCTAssertEqual(model.selectedCadenceSeconds, 30)
        XCTAssertEqual(model.selectedRetentionSeconds, 604_800)
        XCTAssertEqual(model.selectedMode, .study)
        XCTAssertEqual(model.snapshot?.studyState, .active)
        XCTAssertEqual(model.snapshot?.launchAtLoginState, .enabled)
        XCTAssertEqual(model.snapshot?.exclusions.policyVersion, "privacy-test-v1")

        let saved = await model.saveCustomExclusions(
            bundleIdentifiers: ["com.example.private"],
            titleFragments: ["Private"]
        )

        XCTAssertFalse(saved)
        XCTAssertEqual(runtime.customExclusionMutationCount, 0)
        XCTAssertEqual(
            model.lastError,
            SettingsServiceError.customExclusionsUnavailable.localizedDescription
        )
    }

    func testDefaultGrantDraftExcludesOCRAndUsesBoundedScopeAndLimits() {
        let draft = SettingsGrantDraft()

        XCTAssertFalse(draft.allowOCR)
        XCTAssertEqual(draft.policy.rollingHorizonSeconds, 86_400)
        XCTAssertEqual(draft.policy.expiresAfter, 604_800)
        XCTAssertEqual(draft.policy.limits.maxPageItems, 50)
        XCTAssertEqual(draft.policy.limits.maxResponseBytes, 262_144)
        XCTAssertEqual(draft.policy.limits.maxCumulativeBytes, 67_108_864)
        XCTAssertFalse(GrantHorizonOption.allCases.contains { $0.rawValue > 604_800 })
        XCTAssertFalse(GrantExpiryOption.allCases.contains { $0.rawValue > 2_592_000 })
    }

    func testRecordingPersistenceFailureDoesNotClaimSuccessOrChangeAuthoritativeSnapshot() async {
        let runtime = StubSettingsRuntime(snapshot: makeRuntimeSnapshot())
        runtime.recordingError = SettingsRuntimeTestError.persistenceFailed
        let model = SettingsViewModel(now: { self.now })
        model.attach(runtime: runtime, integrations: StubSettingsIntegrationManager())
        await model.load()

        await model.setRecordingEnabled(false)

        XCTAssertEqual(model.snapshot?.recordingEnabled, true)
        XCTAssertNil(model.notice)
        XCTAssertEqual(model.lastError, "The recording preference was not persisted.")
        XCTAssertEqual(runtime.recordingMutationCount, 1)
    }

    func testLaunchAtLoginApprovalIsAttentionStateRatherThanEnabledSuccess() async {
        let runtime = StubSettingsRuntime(
            snapshot: makeRuntimeSnapshot(launchAtLoginState: .notRegistered)
        )
        runtime.launchStateAfterMutation = .requiresApproval
        let model = SettingsViewModel(now: { self.now })
        model.attach(runtime: runtime, integrations: StubSettingsIntegrationManager())
        await model.load()

        await model.setLaunchAtLogin(true)

        XCTAssertEqual(model.snapshot?.launchAtLoginState, .requiresApproval)
        XCTAssertNil(model.notice)
        XCTAssertNil(model.lastError)
        XCTAssertEqual(
            model.launchApprovalNotice,
            "Approval required. Allow Open Chronicle in System Settings → General → Login Items."
        )
    }

    func testSettingsNavigationIsIdempotent() {
        let navigation = NavigationModel()

        navigation.show(.settings)
        navigation.show(.settings)

        XCTAssertEqual(navigation.path, [.settings])
    }

    func testExactReceiptEnablesRepairAndUnregisterWithoutExposingManagedValues() async throws {
        let fixture = SettingsIntegrationFixture(kind: .codex, exactReceipt: true)
        let service = fixture.service()

        let rows = await service.scan(at: now)
        let row = try XCTUnwrap(rows.first { $0.kind == .codex })

        XCTAssertEqual(row.receiptStatus, .exact)
        XCTAssertTrue(row.canRepair)
        XCTAssertTrue(row.canUnregister)
        XCTAssertTrue(row.canEditAccess)
        let presentation = String(describing: row)
        XCTAssertFalse(presentation.contains(fixture.managedRoot.path))
        XCTAssertFalse(presentation.contains(fixture.grant.grantID))
        XCTAssertFalse(presentation.contains(fixture.grant.clientID))

        let repaired = try await service.repair(kind: .codex)
        XCTAssertEqual(repaired, "Verified the existing exact Open Chronicle registration.")
        let disconnected = try await service.unregister(kind: .codex)
        let revokeCount = await fixture.grants.revokeCount()
        XCTAssertEqual(disconnected, "Disconnected Codex and revoked its disclosure grant.")
        XCTAssertEqual(fixture.registration.unregisterCount, 1)
        XCTAssertEqual(revokeCount, 1)
    }

    func testReceiptMismatchRefusesRepairBeforeCallingExternalRegistration() async throws {
        let fixture = SettingsIntegrationFixture(kind: .codex, exactReceipt: false)
        let service = fixture.service()
        let rows = await service.scan(at: now)
        let row = try XCTUnwrap(rows.first { $0.kind == .codex })

        XCTAssertEqual(row.receiptStatus, .mismatch)
        XCTAssertFalse(row.canRepair)
        XCTAssertFalse(row.canUnregister)

        do {
            _ = try await service.repair(kind: .codex)
            XCTFail("Repair should require an exact receipt")
        } catch {
            XCTAssertEqual(error as? SettingsIntegrationError, .receiptRequired)
        }
        XCTAssertEqual(fixture.connection.connectCount, 0)
        XCTAssertEqual(fixture.registration.unregisterCount, 0)
    }

    func testCredentialReadFailureNeverOffersConnect() async throws {
        let fixture = SettingsIntegrationFixture(kind: .codex, exactReceipt: false)
        fixture.receipts.remove(kind: .codex)
        fixture.credentials.readError = AgentGrantCredentialStoreError.readFailed
        let service = fixture.service()

        let rows = await service.scan(at: now)
        let row = try XCTUnwrap(rows.first { $0.kind == .codex })

        XCTAssertEqual(row.receiptStatus, .incomplete)
        XCTAssertFalse(row.canConnect)
        XCTAssertFalse(row.canRepair)
    }

    func testClaudeDesktopCredentialReadFailureNeverOffersPackageCreation() async throws {
        let fixture = SettingsIntegrationFixture(kind: .claudeDesktop, exactReceipt: false)
        fixture.receipts.remove(kind: .claudeDesktop)
        fixture.credentials.readError = AgentGrantCredentialStoreError.readFailed
        let service = fixture.service()

        let rows = await service.scan(at: now)
        let row = try XCTUnwrap(rows.first { $0.kind == .claudeDesktop })

        XCTAssertEqual(row.receiptStatus, .incomplete)
        XCTAssertFalse(row.canPrepareDesktopPackage)
        XCTAssertFalse(row.canRevoke)
    }

    func testRevocationOutcomeUsesActualClientNameAndClearsCredential() async throws {
        let fixture = SettingsIntegrationFixture(kind: .claudeCode, exactReceipt: false)
        let service = fixture.service()
        _ = await service.scan(at: now)

        let outcome = try await service.revoke(kind: .claudeCode)
        let revokeCount = await fixture.grants.revokeCount()

        XCTAssertEqual(outcome, "Revoked Claude Code access immediately.")
        XCTAssertNil(try fixture.credentials.credential(for: .claudeCode))
        XCTAssertEqual(revokeCount, 1)
    }

    func testCachedProvisioningScopeDoesNotClaimLiveCoreStateOrUsage() async throws {
        let fixture = SettingsIntegrationFixture(kind: .claudeDesktop, exactReceipt: false)
        let service = fixture.service()
        let rows = await service.scan(at: now)
        let row = try XCTUnwrap(rows.first { $0.kind == .claudeDesktop })

        XCTAssertEqual(row.receiptStatus, .desktopCredentialCached)
        XCTAssertNotNil(row.cachedGrantScope)
        XCTAssertFalse(row.canPrepareDesktopPackage)
        XCTAssertFalse(row.canEditAccess)
        XCTAssertTrue(row.canRevoke)
        let copy = SettingsIntegrationCopy.cachedScopeExplanation.lowercased()
        XCTAssertTrue(copy.contains("cached provisioning record only"))
        XCTAssertTrue(copy.contains("not live core grant status or usage"))
        XCTAssertFalse(copy.contains("grant is active"))
        XCTAssertFalse(String(describing: row).contains("disclosedBytes"))
    }

    func testClaudeDesktopExistingCredentialMustBeExplicitlyRevokedBeforeNewPackage() async throws {
        let fixture = SettingsIntegrationFixture(kind: .claudeDesktop, exactReceipt: false)
        let service = fixture.service()
        _ = await service.scan(at: now)

        do {
            _ = try await service.createClaudeDesktopPackage(
                at: URL(fileURLWithPath: "/tmp/replacement.mcpb"),
                policy: SettingsGrantDraft().policy
            )
            XCTFail("Existing Claude Desktop access must be revoked explicitly first")
        } catch {
            XCTAssertEqual(error as? SettingsIntegrationError, .setupAlreadyExists)
        }

        let grantActivity = await fixture.grants.activity()
        XCTAssertEqual(grantActivity.provisionCount, 0)
        XCTAssertEqual(grantActivity.installCount, 0)
        XCTAssertEqual(grantActivity.revokeCount, 0)
        XCTAssertEqual(fixture.package.callCount, 0)
        XCTAssertNotNil(try fixture.credentials.credential(for: .claudeDesktop))
    }

    func testPackageModelReportsOnlySafeFileNameAndManualInstallOutcome() async {
        let manager = StubSettingsIntegrationManager()
        manager.packageReceipt = MCPBPackageReceipt(
            packageURL: URL(fileURLWithPath: "/private/tmp/secret/Open-Chronicle.mcpb"),
            packageFileName: "Open-Chronicle.mcpb",
            grantExpiresAt: "2026-07-20T09:00:00Z",
            scopeDescription: MCPBPackageService.scopeDescription
        )
        let model = IntegrationSettingsModel(now: { self.now })
        model.attach(service: manager)

        await model.createClaudeDesktopPackage(
            at: URL(fileURLWithPath: "/private/tmp/secret/Open-Chronicle.mcpb"),
            draft: SettingsGrantDraft()
        )

        XCTAssertEqual(
            model.notice,
            "Created Open-Chronicle.mcpb. Install it manually in Claude Desktop."
        )
        XCTAssertFalse(model.notice?.contains("/private/tmp") == true)
        XCTAssertFalse(model.notice?.contains("grant-") == true)
    }

    private func makeRuntimeSnapshot(
        launchAtLoginState: LaunchAtLoginState = .enabled
    ) -> SettingsRuntimeSnapshot {
        SettingsRuntimeSnapshot(
            recordingEnabled: true,
            cadenceSeconds: 30,
            screenshotRetentionSeconds: 604_800,
            mode: .study,
            studyState: .active,
            studyStart: now.addingTimeInterval(-3_600),
            studyEnd: now.addingTimeInterval(86_400),
            launchAtLoginState: launchAtLoginState,
            exclusions: SettingsExclusionsSnapshot(
                policyVersion: "privacy-test-v1",
                builtInBundleIdentifiers: ["com.apple.keychainaccess"],
                builtInTitleFragments: [],
                customBundleIdentifiers: [],
                customTitleFragments: [],
                supportsCustomExclusions: false
            ),
            diagnostics: SettingsDiagnosticsSnapshot(
                projection: .current,
                acknowledgement: .durable,
                managedBytes: 1_024,
                availableBytes: 1_048_576,
                activeGrantCount: 1,
                latestJournalAt: "2026-07-13T09:00:00Z"
            )
        )
    }
}

@MainActor
private final class StubSettingsRuntime: SettingsRuntimeServicing {
    private var value: SettingsRuntimeSnapshot
    private(set) var customExclusionMutationCount = 0
    private(set) var recordingMutationCount = 0
    var recordingError: Error?
    var launchStateAfterMutation: LaunchAtLoginState?

    init(snapshot: SettingsRuntimeSnapshot) {
        value = snapshot
    }

    func snapshot(at date: Date) -> SettingsRuntimeSnapshot {
        _ = date
        return value
    }

    func setRecordingEnabled(_ enabled: Bool) throws {
        _ = enabled
        recordingMutationCount += 1
        if let recordingError { throw recordingError }
    }
    func setCadence(seconds: UInt32, at date: Date) { _ = (seconds, date) }
    func setScreenshotRetention(seconds: UInt32, at date: Date) { _ = (seconds, date) }
    func usePersonalMode(at date: Date) { _ = date }
    func configureStudy(start: Date, end: Date, at date: Date) { _ = (start, end, date) }
    func setLaunchAtLogin(_ enabled: Bool) async throws {
        _ = enabled
        guard let launchStateAfterMutation else { return }
        value = SettingsRuntimeSnapshot(
            recordingEnabled: value.recordingEnabled,
            cadenceSeconds: value.cadenceSeconds,
            screenshotRetentionSeconds: value.screenshotRetentionSeconds,
            mode: value.mode,
            studyState: value.studyState,
            studyStart: value.studyStart,
            studyEnd: value.studyEnd,
            launchAtLoginState: launchStateAfterMutation,
            exclusions: value.exclusions,
            diagnostics: value.diagnostics
        )
    }
    func openLaunchAtLoginApproval() {}

    func updateCustomExclusions(
        bundleIdentifiers: [String],
        titleFragments: [String],
        at date: Date
    ) throws {
        _ = (bundleIdentifiers, titleFragments, date)
        customExclusionMutationCount += 1
    }
}

@MainActor
private final class StubSettingsIntegrationManager: SettingsIntegrationManaging {
    var rows: [SettingsIntegrationSnapshot] = []
    var packageReceipt = MCPBPackageReceipt(
        packageURL: URL(fileURLWithPath: "/tmp/Open-Chronicle.mcpb"),
        packageFileName: "Open-Chronicle.mcpb",
        grantExpiresAt: "2026-07-20T09:00:00Z",
        scopeDescription: MCPBPackageService.scopeDescription
    )

    func scan(at date: Date) -> [SettingsIntegrationSnapshot] {
        _ = date
        return rows
    }

    func connect(kind: AgentKind, policy: DisclosureGrantPolicy) -> String {
        _ = (kind, policy)
        return "Connected"
    }

    func repair(kind: AgentKind) -> String { "Repaired \(kind.displayName)" }

    func replaceGrant(kind: AgentKind, policy: DisclosureGrantPolicy) -> String {
        _ = policy
        return "Replaced \(kind.displayName)"
    }

    func unregister(kind: AgentKind) -> String { "Disconnected \(kind.displayName)" }
    func revoke(kind: AgentKind) -> String { "Revoked \(kind.displayName)" }

    func createClaudeDesktopPackage(
        at destination: URL,
        policy: DisclosureGrantPolicy
    ) -> MCPBPackageReceipt {
        _ = (destination, policy)
        return packageReceipt
    }
}

@MainActor
private final class SettingsIntegrationFixture {
    let kind: AgentKind
    let applicationBundle = URL(fileURLWithPath: "/Applications/Open Chronicle.app")
    let helper = URL(fileURLWithPath: "/Applications/Open Chronicle.app/Contents/Helpers/chronicle-mcp")
    let managedRoot = URL(fileURLWithPath: "/Users/test/Library/Application Support/Open Chronicle")
    let grant: DisclosureGrantRecord
    let detector: FixedSettingsAgentDetector
    let connection = StubSettingsAgentConnection()
    let registration = StubSettingsAgentRegistration()
    let grants: StubSettingsGrantService
    let credentials = MemorySettingsCredentialStore()
    let receipts = MemorySettingsReceiptStore()
    let package = StubSettingsPackageService()

    init(kind: AgentKind, exactReceipt: Bool) {
        self.kind = kind
        grant = DisclosureGrantRecord(
            schemaVersion: "1.0",
            grantID: "grant-private-test",
            clientID: "client-private-test",
            receiptID: "receipt-private-test",
            timeScope: .rollingHorizon(seconds: 86_400),
            contentClasses: [.metadata, .derived],
            createdAt: "2026-07-13T09:00:00Z",
            expiresAt: "2026-07-20T09:00:00Z",
            state: "active",
            limits: DisclosureGrantLimits(
                maxPageItems: 50,
                maxResponseBytes: 262_144,
                maxCumulativeBytes: 67_108_864
            ),
            disclosedBytes: 0,
            storeGeneration: 1
        )
        grants = StubSettingsGrantService(grant: grant)
        let installation = AgentInstallation(
            kind: kind,
            executableURL: URL(fileURLWithPath: "/usr/local/bin/\(kind.rawValue)"),
            applicationURL: nil,
            version: "1.2.3",
            support: .supported,
            alternateExecutableURLs: []
        )
        detector = FixedSettingsAgentDetector(installations: [installation])
        try! credentials.save(AgentGrantCredential(
            schemaVersion: AgentGrantCredential.schemaVersion,
            agentKind: kind,
            grant: grant
        ))
        let plan = AgentRegistrationPlan(
            applicationBundleURL: applicationBundle,
            helperURL: helper,
            managedRootURL: managedRoot,
            clientID: grant.clientID,
            grantID: grant.grantID
        )
        receipts.save(AgentRegistrationReceipt(
            schemaVersion: AgentRegistrationReceipt.schemaVersion,
            agentKind: kind,
            agentVersion: installation.version,
            serverName: AgentRegistrationPlan.serverName,
            clientID: grant.clientID,
            resolvedHelperPath: helper.path,
            managedRootPath: managedRoot.path,
            argumentDigest: exactReceipt
                ? RegistrationReceiptMatcher.argumentDigest(kind: kind, plan: plan)
                : String(repeating: "0", count: 64),
            priorState: .absent,
            result: .added,
            registeredAt: Date(timeIntervalSince1970: 1_784_016_000)
        ))
    }

    func service() -> SettingsIntegrationService {
        SettingsIntegrationService(
            detector: detector,
            connection: connection,
            registration: registration,
            grants: grants,
            credentials: credentials,
            receipts: receipts,
            packageService: package,
            applicationBundleURL: applicationBundle,
            helperURL: helper,
            managedRootURL: managedRoot,
            now: { Date(timeIntervalSince1970: 1_784_016_000) }
        )
    }
}

@MainActor
private final class FixedSettingsAgentDetector: AgentDetecting {
    let installations: [AgentInstallation]

    init(installations: [AgentInstallation]) {
        self.installations = installations
    }

    func detect() -> [AgentInstallation] { installations }
}

@MainActor
private final class StubSettingsAgentConnection: SettingsAgentConnecting {
    private(set) var connectCount = 0

    func connect(
        _ installation: AgentInstallation,
        policy: DisclosureGrantPolicy
    ) -> AgentRegistrationOutcome {
        _ = policy
        connectCount += 1
        let receipt = AgentRegistrationReceipt(
            schemaVersion: AgentRegistrationReceipt.schemaVersion,
            agentKind: installation.kind,
            agentVersion: installation.version,
            serverName: AgentRegistrationPlan.serverName,
            clientID: "client-private-test",
            resolvedHelperPath: "/private/helper",
            managedRootPath: "/private/root",
            argumentDigest: String(repeating: "1", count: 64),
            priorState: .exact,
            result: .adopted,
            registeredAt: Date(timeIntervalSince1970: 1_784_016_000)
        )
        return .alreadyRegistered(receipt)
    }
}

@MainActor
private final class StubSettingsAgentRegistration: AgentRegistering {
    private(set) var unregisterCount = 0

    func register(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> AgentRegistrationOutcome {
        _ = (installation, plan)
        return .alreadyRegistered(makeReceipt())
    }

    func unregister(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> AgentRegistrationOutcome {
        _ = (installation, plan)
        unregisterCount += 1
        return .removed
    }

    private func makeReceipt() -> AgentRegistrationReceipt {
        AgentRegistrationReceipt(
            schemaVersion: AgentRegistrationReceipt.schemaVersion,
            agentKind: .codex,
            agentVersion: nil,
            serverName: AgentRegistrationPlan.serverName,
            clientID: "client-private-test",
            resolvedHelperPath: "/private/helper",
            managedRootPath: "/private/root",
            argumentDigest: String(repeating: "1", count: 64),
            priorState: .exact,
            result: .adopted,
            registeredAt: Date(timeIntervalSince1970: 1_784_016_000)
        )
    }
}

private actor StubSettingsGrantService: DisclosureGrantServicing {
    struct Activity: Equatable, Sendable {
        var provisionCount = 0
        var installCount = 0
        var revokeCount = 0
    }

    let grant: DisclosureGrantRecord
    private var recorded = Activity()

    init(grant: DisclosureGrantRecord) {
        self.grant = grant
    }

    func provision(
        for kind: AgentKind,
        policy: DisclosureGrantPolicy,
        now: Date
    ) -> DisclosureGrantRecord {
        _ = (kind, policy, now)
        recorded.provisionCount += 1
        return grant
    }

    func install(
        _ grant: DisclosureGrantRecord,
        now: Date
    ) -> DisclosureGrantMutationResponse {
        _ = now
        recorded.installCount += 1
        return DisclosureGrantMutationResponse(mutation: .installed, grant: grant)
    }

    func revoke(
        grantID: String,
        clientID: String,
        receiptID: String,
        now: Date
    ) -> DisclosureGrantMutationResponse {
        _ = (grantID, clientID, receiptID, now)
        recorded.revokeCount += 1
        return DisclosureGrantMutationResponse(mutation: .revoked, grant: grant)
    }

    func revokeCount() -> Int { recorded.revokeCount }
    func activity() -> Activity { recorded }
}

private final class MemorySettingsCredentialStore: AgentGrantCredentialStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [AgentKind: AgentGrantCredential] = [:]
    var readError: Error?

    func credential(for kind: AgentKind) throws -> AgentGrantCredential? {
        lock.lock()
        defer { lock.unlock() }
        if let readError { throw readError }
        return values[kind]
    }

    func save(_ credential: AgentGrantCredential) throws {
        lock.lock()
        defer { lock.unlock() }
        values[credential.agentKind] = credential
    }

    func remove(kind: AgentKind) throws {
        lock.lock()
        defer { lock.unlock() }
        values.removeValue(forKey: kind)
    }
}

@MainActor
private final class MemorySettingsReceiptStore: AgentRegistrationReceiptStoring {
    private var values: [AgentKind: AgentRegistrationReceipt] = [:]

    func receipt(for kind: AgentKind) -> AgentRegistrationReceipt? { values[kind] }
    func save(_ receipt: AgentRegistrationReceipt) { values[receipt.agentKind] = receipt }
    func remove(kind: AgentKind) { values.removeValue(forKey: kind) }
}

private final class StubSettingsPackageService: MCPBPackageCreating, @unchecked Sendable {
    private let lock = NSLock()
    private(set) var callCount = 0

    func createPackage(
        at selectedURL: URL,
        managedRootURL: URL,
        grant: DisclosureGrantRecord
    ) -> MCPBPackageReceipt {
        _ = (managedRootURL, grant)
        lock.lock()
        callCount += 1
        lock.unlock()
        return MCPBPackageReceipt(
            packageURL: selectedURL,
            packageFileName: selectedURL.lastPathComponent,
            grantExpiresAt: grant.expiresAt,
            scopeDescription: MCPBPackageService.scopeDescription
        )
    }
}

private enum SettingsRuntimeTestError: LocalizedError {
    case persistenceFailed

    var errorDescription: String? {
        "The recording preference was not persisted."
    }
}
