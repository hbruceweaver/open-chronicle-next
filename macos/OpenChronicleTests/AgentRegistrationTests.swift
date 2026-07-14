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

    func destroy() {
        try? FileManager.default.removeItem(at: root)
    }
}
