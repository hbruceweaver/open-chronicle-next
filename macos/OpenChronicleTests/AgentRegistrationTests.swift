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
