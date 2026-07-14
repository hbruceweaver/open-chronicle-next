import Foundation
import XCTest
@testable import OpenChronicle

@MainActor
final class AgentDetectionTests: XCTestCase {
    func testDetectionFindsHiddenGUIPathAndReportsDistinctDuplicateInstall() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let hidden = temporary.appendingPathComponent(".local/bin", isDirectory: true)
        let fixed = temporary.appendingPathComponent("fixed", isDirectory: true)
        try FileManager.default.createDirectory(at: hidden, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: fixed, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let primary = hidden.appendingPathComponent("codex")
        let duplicate = fixed.appendingPathComponent("codex")
        try makeExecutable(primary)
        try makeExecutable(duplicate)
        let runner = StubAgentCommandRunner { _, arguments in
            if arguments == ["--version"] {
                return AgentCommandResult(
                    exitCode: 0,
                    standardOutput: "codex-cli 0.144.0\nignored\n",
                    standardError: ""
                )
            }
            return AgentCommandResult(exitCode: 0, standardOutput: "MCP get help", standardError: "")
        }
        let service = AgentDetectionService(
            runner: runner,
            applications: StubAgentApplicationLocator(applications: [:]),
            environment: AgentDetectionEnvironment(
                homeDirectory: temporary,
                pathDirectories: [],
                fixedExecutableDirectories: [fixed]
            )
        )

        let installations = await service.detect()
        let codex = try XCTUnwrap(installations.first { $0.kind == .codex })

        XCTAssertEqual(codex.executableURL, primary.resolvingSymlinksInPath())
        XCTAssertEqual(codex.version, "codex-cli 0.144.0")
        XCTAssertEqual(codex.support, .supported)
        XCTAssertEqual(codex.alternateExecutableURLs, [duplicate.resolvingSymlinksInPath()])
        XCTAssertTrue(codex.hasDuplicateExecutables)
    }

    func testDetectionMarksInstalledCLIUnsupportedWhenCapabilityProbeFails() async throws {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let claude = temporary.appendingPathComponent("claude")
        try makeExecutable(claude)
        let runner = StubAgentCommandRunner { _, arguments in
            AgentCommandResult(
                exitCode: arguments == ["--version"] ? 0 : 2,
                standardOutput: arguments == ["--version"] ? "2.1.200" : "",
                standardError: "unsupported"
            )
        }
        let service = AgentDetectionService(
            runner: runner,
            applications: StubAgentApplicationLocator(applications: [:]),
            environment: AgentDetectionEnvironment(
                homeDirectory: temporary,
                pathDirectories: [temporary],
                fixedExecutableDirectories: []
            )
        )

        let installations = await service.detect()

        XCTAssertEqual(installations.first { $0.kind == .claudeCode }?.support, .unsupported)
    }

    private func makeExecutable(_ url: URL) throws {
        try Data("fixture".utf8).write(to: url)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: url.path
        )
    }
}

@MainActor
final class AgentRegistrationServiceTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_784_016_000)

    func testCodexAbsentRegistrationUsesOfficialCLIAndStoresRedactedReceipt() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let plan = fixture.plan(grantID: "grant-secret-codex")
        let runner = ScriptedAgentCommandRunner(responses: [
            missing(),
            success(),
            success(Self.codexJSON(plan: plan)),
        ])
        let receipts = MemoryAgentRegistrationReceiptStore()
        let service = AgentRegistrationService(
            runner: runner,
            receipts: receipts,
            now: { self.now }
        )

        let outcome = await service.register(installation: codex(fixture), plan: plan)

        guard case let .registered(receipt) = outcome else {
            return XCTFail("expected registered, received \(outcome)")
        }
        XCTAssertEqual(receipt.priorState, .absent)
        XCTAssertEqual(receipt.result, .added)
        let calls = await runner.recordedCalls()
        XCTAssertEqual(calls.count, 3)
        XCTAssertEqual(calls[0].arguments, ["mcp", "get", "open-chronicle", "--json"])
        XCTAssertEqual(
            calls[1].arguments,
            ["mcp", "add", "open-chronicle", "--", plan.helperURL.path]
                + plan.helperArguments
        )
        let encoded = try JSONEncoder().encode(try XCTUnwrap(receipts.receipt(for: .codex)))
        let text = try XCTUnwrap(String(data: encoded, encoding: .utf8))
        XCTAssertFalse(text.contains(plan.grantID))
        XCTAssertFalse(text.contains("standardOutput"))
        XCTAssertEqual(receipt.argumentDigest.count, 64)
    }

    func testExactCodexRegistrationIsAdoptedWithoutOverwrite() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let plan = fixture.plan()
        let runner = ScriptedAgentCommandRunner(responses: [
            success(Self.codexJSON(plan: plan)),
        ])
        let receipts = MemoryAgentRegistrationReceiptStore()
        let service = AgentRegistrationService(runner: runner, receipts: receipts)

        let outcome = await service.register(installation: codex(fixture), plan: plan)

        guard case let .alreadyRegistered(receipt) = outcome else {
            return XCTFail("expected already registered, received \(outcome)")
        }
        XCTAssertEqual(receipt.priorState, .exact)
        XCTAssertEqual(receipt.result, .adopted)
        let calls = await runner.recordedCalls()
        XCTAssertEqual(calls.count, 1)
    }

    func testConflictingCodexRegistrationIsNeverOverwritten() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let plan = fixture.plan()
        let conflict = """
        {"name":"open-chronicle","transport":{"command":"/tmp/other","args":[]}}
        """
        let runner = ScriptedAgentCommandRunner(responses: [success(conflict)])
        let receipts = MemoryAgentRegistrationReceiptStore()
        let service = AgentRegistrationService(runner: runner, receipts: receipts)

        let outcome = await service.register(installation: codex(fixture), plan: plan)

        XCTAssertEqual(outcome, .conflict)
        XCTAssertNil(receipts.receipt(for: .codex))
        let calls = await runner.recordedCalls()
        XCTAssertEqual(calls.count, 1)
    }

    func testUnregisterRequiresReceiptAndRemovesOnlyExactLiveEntry() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let plan = fixture.plan()
        let exact = Self.codexJSON(plan: plan)
        let runner = ScriptedAgentCommandRunner(responses: [
            missing(), success(), success(exact),
            success(exact), success(), missing(),
        ])
        let receipts = MemoryAgentRegistrationReceiptStore()
        let service = AgentRegistrationService(runner: runner, receipts: receipts)
        _ = await service.register(installation: codex(fixture), plan: plan)

        let outcome = await service.unregister(installation: codex(fixture), plan: plan)

        XCTAssertEqual(outcome, .removed)
        XCTAssertNil(receipts.receipt(for: .codex))
        let calls = await runner.recordedCalls()
        XCTAssertEqual(calls[4].arguments, ["mcp", "remove", "open-chronicle"])
    }

    func testChangedGrantCannotUseOldReceiptToRemoveRegistration() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let original = fixture.plan(grantID: "grant-original")
        let runner = ScriptedAgentCommandRunner(responses: [
            missing(), success(), success(Self.codexJSON(plan: original)),
        ])
        let receipts = MemoryAgentRegistrationReceiptStore()
        let service = AgentRegistrationService(runner: runner, receipts: receipts)
        _ = await service.register(installation: codex(fixture), plan: original)

        let outcome = await service.unregister(
            installation: codex(fixture),
            plan: fixture.plan(grantID: "grant-replacement")
        )

        XCTAssertEqual(outcome, .failed(.receiptMismatch))
        let calls = await runner.recordedCalls()
        XCTAssertEqual(calls.count, 3)
        XCTAssertNotNil(receipts.receipt(for: .codex))
    }

    func testClaudeCodeUsesUserScopedCommandAndVerifiesHumanOutputConservatively() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let plan = fixture.plan(clientID: "claude-code-local", grantID: "grant-claude")
        let verified = """
        open-chronicle:
          Scope: User config
          Status: Connected
          Command: \(plan.helperURL.path)
          Args: \(plan.helperArguments.joined(separator: " "))
        """
        let runner = ScriptedAgentCommandRunner(responses: [
            missing(), success(), success(verified),
        ])
        let service = AgentRegistrationService(
            runner: runner,
            receipts: MemoryAgentRegistrationReceiptStore()
        )

        let outcome = await service.register(installation: claudeCode(fixture), plan: plan)

        guard case .registered = outcome else {
            return XCTFail("expected registration, received \(outcome)")
        }
        let calls = await runner.recordedCalls()
        XCTAssertEqual(
            calls[1].arguments,
            [
                "mcp", "add", "--transport", "stdio", "--scope", "user",
                "open-chronicle", "--", plan.helperURL.path,
            ] + plan.helperArguments
        )
    }

    func testClaudeDesktopIsGuidedAndNeverInvokesClaudeCodeCLI() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let runner = ScriptedAgentCommandRunner(responses: [])
        let service = AgentRegistrationService(
            runner: runner,
            receipts: MemoryAgentRegistrationReceiptStore()
        )
        let installation = AgentInstallation(
            kind: .claudeDesktop,
            executableURL: nil,
            applicationURL: fixture.bundle,
            version: "1.0",
            support: .supported,
            alternateExecutableURLs: []
        )

        let outcome = await service.register(installation: installation, plan: fixture.plan())

        XCTAssertEqual(outcome, .guidedDesktop)
        let calls = await runner.recordedCalls()
        XCTAssertTrue(calls.isEmpty)
    }

    private func codex(_ fixture: InstallFixture) -> AgentInstallation {
        AgentInstallation(
            kind: .codex,
            executableURL: fixture.root.appendingPathComponent("codex"),
            applicationURL: nil,
            version: "codex-cli 0.144.0",
            support: .supported,
            alternateExecutableURLs: []
        )
    }

    private func claudeCode(_ fixture: InstallFixture) -> AgentInstallation {
        AgentInstallation(
            kind: .claudeCode,
            executableURL: fixture.root.appendingPathComponent("claude"),
            applicationURL: nil,
            version: "2.1.200",
            support: .supported,
            alternateExecutableURLs: []
        )
    }

    private static func codexJSON(plan: AgentRegistrationPlan) -> String {
        let value: [String: Any] = [
            "name": AgentRegistrationPlan.serverName,
            "transport": [
                "type": "stdio",
                "command": plan.helperURL.path,
                "args": plan.helperArguments,
            ],
        ]
        let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }

    private func success(_ output: String = "") -> AgentCommandResult {
        AgentCommandResult(exitCode: 0, standardOutput: output, standardError: "")
    }

    private func missing() -> AgentCommandResult {
        AgentCommandResult(exitCode: 1, standardOutput: "", standardError: "server not found")
    }
}

@MainActor
final class CoreDisclosureGrantTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_784_016_000)

    func testDefaultGrantDeniesOCRPersistsAndRevokesExactlyAcrossReopen() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let firstCore = try InProcessCore(applicationSupportURL: directory, now: now)
        let firstService = CoreDisclosureGrantService(core: firstCore)
        let grant = try await firstService.provision(for: .codex, now: now)

        XCTAssertEqual(grant.contentClasses, [.metadata, .derived])
        XCTAssertFalse(grant.contentClasses.contains(.ocr))
        XCTAssertEqual(grant.timeScope, .rollingHorizon(seconds: 86_400))
        let generation = await firstCore.openedStoreGeneration()
        XCTAssertEqual(grant.storeGeneration, generation)
        let installed = try await firstService.install(
            grant,
            now: now.addingTimeInterval(1)
        )
        XCTAssertEqual(installed.mutation, .installed)
        XCTAssertEqual(installed.grant, grant)
        let replayed = try await firstService.install(
            grant,
            now: now.addingTimeInterval(2)
        )
        XCTAssertEqual(replayed.mutation, .alreadyInstalled)
        try await firstCore.close()

        let reopened = try InProcessCore(
            applicationSupportURL: directory,
            now: now.addingTimeInterval(3)
        )
        let reopenedService = CoreDisclosureGrantService(core: reopened)
        let revoked = try await reopenedService.revoke(
            grantID: grant.grantID,
            clientID: grant.clientID,
            receiptID: grant.receiptID,
            now: now.addingTimeInterval(4)
        )
        XCTAssertEqual(revoked.mutation, .revoked)
        XCTAssertEqual(revoked.grant.state, "revoked")
        let revokeReplay = try await reopenedService.revoke(
            grantID: grant.grantID,
            clientID: grant.clientID,
            receiptID: grant.receiptID,
            now: now.addingTimeInterval(5)
        )
        XCTAssertEqual(revokeReplay.mutation, .alreadyRevoked)
        try await reopened.close()
    }

    func testGrantIdentityMismatchIsRedactedAndDoesNotRevoke() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let core = try InProcessCore(applicationSupportURL: directory, now: now)
        let service = CoreDisclosureGrantService(core: core)
        let grant = try await service.provision(for: .claudeCode, now: now)
        _ = try await service.install(grant, now: now.addingTimeInterval(1))

        do {
            _ = try await service.revoke(
                grantID: grant.grantID,
                clientID: "SECRET_WRONG_CLIENT",
                receiptID: grant.receiptID,
                now: now.addingTimeInterval(2)
            )
            XCTFail("identity mismatch must fail")
        } catch let ChronicleBridgeError.bridgeStatus(_, payload) {
            XCTAssertEqual(payload?.code, "disclosure-grant-identity-mismatch")
            XCTAssertFalse(payload?.message.contains("SECRET_WRONG_CLIENT") ?? true)
        }

        let revoked = try await service.revoke(
            grantID: grant.grantID,
            clientID: grant.clientID,
            receiptID: grant.receiptID,
            now: now.addingTimeInterval(3)
        )
        XCTAssertEqual(revoked.mutation, .revoked)
        try await core.close()
    }
}

@MainActor
final class AgentConnectionServiceTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_784_016_000)

    func testCredentialIsDurableBeforeRegistrationAndSuccessfulConnectionKeepsGrant() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let grant = Self.grant(fixture: fixture)
        let grants = StubDisclosureGrantService(grant: grant)
        let credentials = MemoryAgentGrantCredentialStore()
        let receipt = Self.registrationReceipt(grant: grant, fixture: fixture)
        let registration = InspectingAgentRegistration { plan in
            XCTAssertEqual(plan.grantID, grant.grantID)
            XCTAssertEqual(
                try? credentials.credential(for: .codex)?.grant,
                grant,
                "credential must be durable before external CLI registration"
            )
            return .registered(receipt)
        }
        let service = AgentConnectionService(
            grants: grants,
            registration: registration,
            credentials: credentials,
            applicationBundleURL: fixture.bundle,
            helperURL: fixture.helper,
            managedRootURL: fixture.managedRoot,
            now: { self.now }
        )

        let outcome = await service.connect(Self.codex(fixture: fixture))

        XCTAssertEqual(outcome, .registered(receipt))
        XCTAssertEqual(try credentials.credential(for: .codex)?.grant, grant)
        let activity = await grants.activity()
        XCTAssertEqual(activity.provisionCount, 1)
        XCTAssertEqual(activity.installCount, 1)
        XCTAssertEqual(activity.revokeCount, 0)
    }

    func testRegistrationConflictRevokesNewGrantAndRemovesProtectedCredential() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let grant = Self.grant(fixture: fixture)
        let grants = StubDisclosureGrantService(grant: grant)
        let credentials = MemoryAgentGrantCredentialStore()
        let registration = InspectingAgentRegistration { _ in .conflict }
        let service = AgentConnectionService(
            grants: grants,
            registration: registration,
            credentials: credentials,
            applicationBundleURL: fixture.bundle,
            helperURL: fixture.helper,
            managedRootURL: fixture.managedRoot,
            now: { self.now }
        )

        let outcome = await service.connect(Self.codex(fixture: fixture))

        XCTAssertEqual(outcome, .conflict)
        XCTAssertNil(try credentials.credential(for: .codex))
        let activity = await grants.activity()
        XCTAssertEqual(activity.revokeCount, 1)
        XCTAssertEqual(activity.lastRevokedGrantID, grant.grantID)
    }

    func testInterruptedSetupResumesExactStoredGrantWithoutProvisioningAnother() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let grant = Self.grant(fixture: fixture)
        let grants = StubDisclosureGrantService(grant: grant)
        let credentials = MemoryAgentGrantCredentialStore()
        try credentials.save(
            AgentGrantCredential(
                schemaVersion: AgentGrantCredential.schemaVersion,
                agentKind: .codex,
                grant: grant
            )
        )
        let receipt = Self.registrationReceipt(grant: grant, fixture: fixture)
        let registration = InspectingAgentRegistration { plan in
            XCTAssertEqual(plan.grantID, grant.grantID)
            return .alreadyRegistered(receipt)
        }
        let service = AgentConnectionService(
            grants: grants,
            registration: registration,
            credentials: credentials,
            applicationBundleURL: fixture.bundle,
            helperURL: fixture.helper,
            managedRootURL: fixture.managedRoot,
            now: { self.now }
        )

        let outcome = await service.connect(Self.codex(fixture: fixture))

        XCTAssertEqual(outcome, .alreadyRegistered(receipt))
        let activity = await grants.activity()
        XCTAssertEqual(activity.provisionCount, 0)
        XCTAssertEqual(activity.installCount, 1)
    }

    func testAmbiguousPostAddFailurePreservesCredentialForRepair() async throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let grant = Self.grant(fixture: fixture)
        let grants = StubDisclosureGrantService(grant: grant)
        let credentials = MemoryAgentGrantCredentialStore()
        let registration = InspectingAgentRegistration { _ in
            .failed(.verificationFailed)
        }
        let service = AgentConnectionService(
            grants: grants,
            registration: registration,
            credentials: credentials,
            applicationBundleURL: fixture.bundle,
            helperURL: fixture.helper,
            managedRootURL: fixture.managedRoot,
            now: { self.now }
        )

        let outcome = await service.connect(Self.codex(fixture: fixture))

        XCTAssertEqual(outcome, .failed(.verificationFailed))
        XCTAssertEqual(try credentials.credential(for: .codex)?.grant, grant)
        let activity = await grants.activity()
        XCTAssertEqual(activity.revokeCount, 0)
    }

    private static func codex(fixture: InstallFixture) -> AgentInstallation {
        AgentInstallation(
            kind: .codex,
            executableURL: fixture.root.appendingPathComponent("codex"),
            applicationURL: nil,
            version: "codex-cli 0.144.0",
            support: .supported,
            alternateExecutableURLs: []
        )
    }

    private static func grant(fixture: InstallFixture) -> DisclosureGrantRecord {
        DisclosureGrantRecord(
            schemaVersion: "1.0",
            grantID: "grant-connection-test",
            clientID: "client-open-chronicle-codex",
            receiptID: "receipt-connection-test",
            timeScope: .rollingHorizon(seconds: 86_400),
            contentClasses: [.metadata, .derived],
            createdAt: "2026-07-13T09:00:00Z",
            expiresAt: "2026-07-20T09:00:00Z",
            state: "active",
            limits: .init(
                maxPageItems: 50,
                maxResponseBytes: 262_144,
                maxCumulativeBytes: 67_108_864
            ),
            disclosedBytes: 0,
            storeGeneration: 1
        )
    }

    private static func registrationReceipt(
        grant: DisclosureGrantRecord,
        fixture: InstallFixture
    ) -> AgentRegistrationReceipt {
        AgentRegistrationReceipt(
            schemaVersion: AgentRegistrationReceipt.schemaVersion,
            agentKind: .codex,
            agentVersion: "codex-cli 0.144.0",
            serverName: AgentRegistrationPlan.serverName,
            clientID: grant.clientID,
            resolvedHelperPath: fixture.helper.path,
            managedRootPath: fixture.managedRoot.path,
            argumentDigest: String(repeating: "a", count: 64),
            priorState: .absent,
            result: .added,
            registeredAt: Date(timeIntervalSince1970: 1_784_016_000)
        )
    }
}

final class InstallLocationTests: XCTestCase {
    func testPackagedHelperAndExternalManagedRootAreReady() throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }

        XCTAssertEqual(
            InstallLocationService().assess(
                applicationBundleURL: fixture.bundle,
                helperURL: fixture.helper,
                managedRootURL: fixture.managedRoot
            ),
            .ready
        )
    }

    func testHelperOutsideBundleIsRejectedEvenWhenExecutable() throws {
        let fixture = try InstallFixture()
        defer { fixture.destroy() }
        let outside = fixture.root.appendingPathComponent("chronicle-mcp")
        try fixture.makeExecutable(outside)

        XCTAssertEqual(
            InstallLocationService().assess(
                applicationBundleURL: fixture.bundle,
                helperURL: outside,
                managedRootURL: fixture.managedRoot
            ),
            .blocked(.helperOutsideBundle)
        )
    }

    func testMountedAndTranslocatedBundlesAreRejectedBeforeRegistration() throws {
        let service = InstallLocationService()
        let managedRoot = FileManager.default.temporaryDirectory

        XCTAssertEqual(
            service.assess(
                applicationBundleURL: URL(fileURLWithPath: "/Volumes/Open Chronicle/Open Chronicle.app"),
                helperURL: URL(fileURLWithPath: "/Volumes/Open Chronicle/Open Chronicle.app/Contents/Helpers/chronicle-mcp"),
                managedRootURL: managedRoot
            ),
            .blocked(.mountedVolume)
        )
        XCTAssertEqual(
            service.assess(
                applicationBundleURL: URL(fileURLWithPath: "/private/var/folders/AppTranslocation/Open Chronicle.app"),
                helperURL: URL(fileURLWithPath: "/private/var/folders/AppTranslocation/Open Chronicle.app/Contents/Helpers/chronicle-mcp"),
                managedRootURL: managedRoot
            ),
            .blocked(.appTranslocation)
        )
    }
}

private actor StubAgentCommandRunner: AgentCommandRunning {
    typealias Handler = @Sendable (URL, [String]) throws -> AgentCommandResult
    private let handler: Handler

    init(handler: @escaping Handler) {
        self.handler = handler
    }

    func run(executableURL: URL, arguments: [String]) throws -> AgentCommandResult {
        try handler(executableURL, arguments)
    }
}

private actor ScriptedAgentCommandRunner: AgentCommandRunning {
    struct Call: Equatable, Sendable {
        let executableURL: URL
        let arguments: [String]
    }

    private var responses: [AgentCommandResult]
    private var calls: [Call] = []

    init(responses: [AgentCommandResult]) {
        self.responses = responses
    }

    func run(executableURL: URL, arguments: [String]) throws -> AgentCommandResult {
        calls.append(Call(executableURL: executableURL, arguments: arguments))
        guard !responses.isEmpty else { throw AgentCommandError.launchFailed }
        return responses.removeFirst()
    }

    func recordedCalls() -> [Call] {
        calls
    }
}

private actor StubDisclosureGrantService: DisclosureGrantServicing {
    struct Activity: Equatable, Sendable {
        var provisionCount = 0
        var installCount = 0
        var revokeCount = 0
        var lastRevokedGrantID: String?
    }

    private let grant: DisclosureGrantRecord
    private var recorded = Activity()

    init(grant: DisclosureGrantRecord) {
        self.grant = grant
    }

    func provision(
        for kind: AgentKind,
        policy: DisclosureGrantPolicy,
        now: Date
    ) -> DisclosureGrantRecord {
        _ = kind
        _ = policy
        _ = now
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
        _ = clientID
        _ = receiptID
        _ = now
        recorded.revokeCount += 1
        recorded.lastRevokedGrantID = grantID
        var revoked = grant
        revoked = DisclosureGrantRecord(
            schemaVersion: revoked.schemaVersion,
            grantID: revoked.grantID,
            clientID: revoked.clientID,
            receiptID: revoked.receiptID,
            timeScope: revoked.timeScope,
            contentClasses: revoked.contentClasses,
            createdAt: revoked.createdAt,
            expiresAt: revoked.expiresAt,
            state: "revoked",
            limits: revoked.limits,
            disclosedBytes: revoked.disclosedBytes,
            storeGeneration: revoked.storeGeneration
        )
        return DisclosureGrantMutationResponse(mutation: .revoked, grant: revoked)
    }

    func activity() -> Activity {
        recorded
    }
}

@MainActor
private final class InspectingAgentRegistration: AgentRegistering {
    typealias Handler = (AgentRegistrationPlan) -> AgentRegistrationOutcome
    private let handler: Handler

    init(handler: @escaping Handler) {
        self.handler = handler
    }

    func register(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> AgentRegistrationOutcome {
        _ = installation
        return handler(plan)
    }

    func unregister(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> AgentRegistrationOutcome {
        _ = installation
        _ = plan
        return .removed
    }
}

private final class MemoryAgentGrantCredentialStore: AgentGrantCredentialStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var credentials: [AgentKind: AgentGrantCredential] = [:]

    func credential(for kind: AgentKind) throws -> AgentGrantCredential? {
        lock.lock()
        defer { lock.unlock() }
        return credentials[kind]
    }

    func save(_ credential: AgentGrantCredential) throws {
        lock.lock()
        defer { lock.unlock() }
        credentials[credential.agentKind] = credential
    }

    func remove(kind: AgentKind) throws {
        lock.lock()
        defer { lock.unlock() }
        credentials.removeValue(forKey: kind)
    }
}

@MainActor
private final class MemoryAgentRegistrationReceiptStore: AgentRegistrationReceiptStoring {
    private var receipts: [AgentKind: AgentRegistrationReceipt] = [:]

    func receipt(for kind: AgentKind) -> AgentRegistrationReceipt? {
        receipts[kind]
    }

    func save(_ receipt: AgentRegistrationReceipt) {
        receipts[receipt.agentKind] = receipt
    }

    func remove(kind: AgentKind) {
        receipts.removeValue(forKey: kind)
    }
}

@MainActor
private final class StubAgentApplicationLocator: AgentApplicationLocating {
    private let applications: [String: URL]

    init(applications: [String: URL]) {
        self.applications = applications
    }

    func applicationURL(bundleIdentifier: String) -> URL? {
        applications[bundleIdentifier]
    }
}

private final class InstallFixture {
    let root: URL
    let bundle: URL
    let helper: URL
    let managedRoot: URL

    init() throws {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        bundle = root.appendingPathComponent("Open Chronicle.app", isDirectory: true)
        helper = bundle.appendingPathComponent("Contents/Helpers/chronicle-mcp")
        managedRoot = root.appendingPathComponent("Application Support", isDirectory: true)
        try FileManager.default.createDirectory(
            at: helper.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(at: managedRoot, withIntermediateDirectories: true)
        try makeExecutable(helper)
    }

    func makeExecutable(_ url: URL) throws {
        try Data("fixture".utf8).write(to: url)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: url.path
        )
    }

    func plan(
        clientID: String = "codex-local",
        grantID: String = "grant-codex"
    ) -> AgentRegistrationPlan {
        AgentRegistrationPlan(
            applicationBundleURL: bundle,
            helperURL: helper,
            managedRootURL: managedRoot,
            clientID: clientID,
            grantID: grantID
        )
    }

    func destroy() {
        try? FileManager.default.removeItem(at: root)
    }
}
