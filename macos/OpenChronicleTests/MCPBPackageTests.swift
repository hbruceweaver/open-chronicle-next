import Foundation
import XCTest
@testable import OpenChronicle

final class MCPBPackageTests: XCTestCase {
    private var root: URL!
    private var applicationBundle: URL!
    private var managedRoot: URL!
    private var exports: URL!
    private var staging: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "open-chronicle-mcpb-tests-\(UUID().uuidString)",
            isDirectory: true
        )
        applicationBundle = root.appendingPathComponent("Open Chronicle.app", isDirectory: true)
        managedRoot = root.appendingPathComponent("private-machine-store", isDirectory: true)
        exports = root.appendingPathComponent("exports", isDirectory: true)
        staging = root.appendingPathComponent("staging", isDirectory: true)
        for directory in [managedRoot!, exports!, staging!] {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
        }
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    func testCreatesSanitizedAtomicDarwinBinaryPackageWithExactLocalScope() throws {
        let sourceHelper = URL(fileURLWithPath: "/usr/bin/true")
        let bundledHelper = try installHelper(from: sourceHelper)
        let selected = exports.appendingPathComponent("Bruce's Chronicle 💻 backup.mcpb")
        let expectedOutput = exports.appendingPathComponent("Bruce-s-Chronicle-backup.mcpb")
        try Data("old package".utf8).write(to: expectedOutput)
        let grant = makeGrant()
        let service = makeService()

        let receipt = try service.createPackage(
            at: selected,
            managedRootURL: managedRoot,
            grant: grant
        )

        XCTAssertEqual(receipt.packageURL, expectedOutput)
        XCTAssertEqual(receipt.packageFileName, "Bruce-s-Chronicle-backup.mcpb")
        XCTAssertEqual(receipt.grantExpiresAt, grant.expiresAt)
        XCTAssertEqual(receipt.scopeDescription, MCPBPackageService.scopeDescription)
        XCTAssertFalse(receipt.scopeDescription.contains(managedRoot.path))
        XCTAssertFalse(receipt.scopeDescription.contains(grant.grantID))

        let extracted = root.appendingPathComponent("extracted", isDirectory: true)
        try FileManager.default.createDirectory(at: extracted, withIntermediateDirectories: true)
        let result = try SystemMCPBCommandRunner().run(
            executableURL: URL(fileURLWithPath: "/usr/bin/ditto"),
            arguments: ["-x", "-k", expectedOutput.path, extracted.path]
        )
        XCTAssertEqual(result.exitCode, 0)
        let files = try regularFiles(relativeTo: extracted)
        XCTAssertEqual(files, ["manifest.json", "server/chronicle-mcp"])

        let extractedHelper = extracted.appendingPathComponent("server/chronicle-mcp")
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: extractedHelper.path))
        XCTAssertEqual(try Data(contentsOf: extractedHelper), try Data(contentsOf: bundledHelper))

        let manifestData = try Data(
            contentsOf: extracted.appendingPathComponent("manifest.json")
        )
        let manifest = try XCTUnwrap(
            JSONSerialization.jsonObject(with: manifestData) as? [String: Any]
        )
        XCTAssertEqual(manifest["$schema"] as? String, MCPBPackageService.manifestSchema)
        XCTAssertEqual(manifest["manifest_version"] as? String, "0.4")
        XCTAssertEqual(manifest["name"] as? String, "open-chronicle")
        XCTAssertEqual(manifest["version"] as? String, "0.1.0")
        XCTAssertEqual(manifest["tools_generated"] as? Bool, true)
        let compatibility = try XCTUnwrap(manifest["compatibility"] as? [String: Any])
        XCTAssertEqual(compatibility["platforms"] as? [String], ["darwin"])
        let server = try XCTUnwrap(manifest["server"] as? [String: Any])
        XCTAssertEqual(server["type"] as? String, "binary")
        XCTAssertEqual(server["entry_point"] as? String, "server/chronicle-mcp")
        let config = try XCTUnwrap(server["mcp_config"] as? [String: Any])
        XCTAssertEqual(
            config["command"] as? String,
            "${__dirname}/server/chronicle-mcp"
        )
        XCTAssertEqual(
            config["args"] as? [String],
            [
                "--managed-root", managedRoot.resolvingSymlinksInPath().path,
                "--client-id", grant.clientID,
                "--grant-id", grant.grantID,
            ]
        )
        XCTAssertEqual((config["env"] as? [String: String])?.count, 0)
        let metadata = try XCTUnwrap(manifest["_meta"] as? [String: Any])
        let scope = try XCTUnwrap(metadata["com.screenata.openchronicle"] as? [String: Any])
        XCTAssertEqual(scope["machine_scoped"] as? Bool, true)
        XCTAssertEqual(scope["grant_scoped"] as? Bool, true)
        XCTAssertEqual(scope["grant_expires_at"] as? String, grant.expiresAt)
        XCTAssertEqual(scope["transport"] as? String, "local-stdio")
        XCTAssertEqual(scope["network_required"] as? Bool, false)

        let manifestText = String(decoding: manifestData, as: UTF8.self)
        XCTAssertFalse(manifestText.contains(grant.receiptID))
        XCTAssertFalse(manifestText.contains("evidence contents"))
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: staging.appendingPathComponent(
                    "open-chronicle-mcpb-package-test"
                ).path
            )
        )
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: exports.appendingPathComponent(
                    ".open-chronicle-package-test.mcpb.partial"
                ).path
            )
        )
    }

    func testRejectsMissingAndNonExecutableBundledHelpers() throws {
        try FileManager.default.createDirectory(
            at: applicationBundle.appendingPathComponent("Contents/Helpers"),
            withIntermediateDirectories: true
        )
        let selected = exports.appendingPathComponent("Open Chronicle.mcpb")
        XCTAssertThrowsError(
            try makeService().createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: makeGrant()
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .helperMissing)
        }

        let helper = applicationBundle.appendingPathComponent(
            "Contents/Helpers/chronicle-mcp"
        )
        try Data("not executable".utf8).write(to: helper)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o644],
            ofItemAtPath: helper.path
        )
        XCTAssertThrowsError(
            try makeService().createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: makeGrant()
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .helperNotExecutable)
            let message = (error as? LocalizedError)?.errorDescription ?? ""
            XCTAssertFalse(message.contains(managedRoot.path))
            XCTAssertFalse(message.contains("grant-local-machine"))
            XCTAssertFalse(message.contains("client-local-machine"))
        }
    }

    func testRejectsExecutableThatIsNotUniversal() throws {
        let helpers = applicationBundle.appendingPathComponent(
            "Contents/Helpers",
            isDirectory: true
        )
        try FileManager.default.createDirectory(at: helpers, withIntermediateDirectories: true)
        let helper = helpers.appendingPathComponent("chronicle-mcp")
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: helper)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755],
            ofItemAtPath: helper.path
        )

        XCTAssertThrowsError(
            try makeService().createPackage(
                at: exports.appendingPathComponent("Open Chronicle.mcpb"),
                managedRootURL: managedRoot,
                grant: makeGrant()
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .helperNotUniversal)
        }
    }

    func testPackagingFailureCleansStageAndDoesNotLeakMachineArguments() throws {
        _ = try installHelper(from: URL(fileURLWithPath: "/usr/bin/true"))
        let grant = makeGrant()
        let service = makeService(commandRunner: FailingArchiveRunner())
        let selected = exports.appendingPathComponent("Open Chronicle.mcpb")

        XCTAssertThrowsError(
            try service.createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: grant
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .packagingFailed)
            let message = (error as? LocalizedError)?.errorDescription ?? ""
            for privateValue in [managedRoot.path, grant.clientID, grant.grantID, grant.receiptID] {
                XCTAssertFalse(message.contains(privateValue))
            }
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: selected.path))
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: staging.appendingPathComponent(
                    "open-chronicle-mcpb-package-test"
                ).path
            )
        )
    }

    func testRejectsTamperedManifestBeforeAtomicPublication() throws {
        _ = try installHelper(from: URL(fileURLWithPath: "/usr/bin/true"))
        let grant = makeGrant()
        let selected = exports.appendingPathComponent("Open Chronicle.mcpb")
        let output = exports.appendingPathComponent("Open-Chronicle.mcpb")
        let prior = Data("known-good prior package".utf8)
        try prior.write(to: output)

        XCTAssertThrowsError(
            try makeService(
                commandRunner: TamperingArchiveRunner(mutation: .manifest)
            ).createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: grant
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .packageValidationFailed)
            assertRedacted(error, grant: grant)
        }
        XCTAssertEqual(try Data(contentsOf: output), prior)
        assertPackagingScratchWasCleaned()
    }

    func testRejectsUnexpectedArchiveEntryBeforeExtractionPublication() throws {
        _ = try installHelper(from: URL(fileURLWithPath: "/usr/bin/true"))
        let grant = makeGrant()
        let selected = exports.appendingPathComponent("Open Chronicle.mcpb")

        XCTAssertThrowsError(
            try makeService(
                commandRunner: TamperingArchiveRunner(mutation: .unexpectedEntry)
            ).createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: grant
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .packageValidationFailed)
            assertRedacted(error, grant: grant)
        }
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: exports.appendingPathComponent("Open-Chronicle.mcpb").path
            )
        )
        assertPackagingScratchWasCleaned()
    }

    func testRejectsMalformedArchiveBeforeAtomicPublication() throws {
        _ = try installHelper(from: URL(fileURLWithPath: "/usr/bin/true"))
        let grant = makeGrant()
        let selected = exports.appendingPathComponent("Open Chronicle.mcpb")

        XCTAssertThrowsError(
            try makeService(commandRunner: MalformedArchiveRunner()).createPackage(
                at: selected,
                managedRootURL: managedRoot,
                grant: grant
            )
        ) { error in
            XCTAssertEqual(error as? MCPBPackageServiceError, .packageValidationFailed)
            assertRedacted(error, grant: grant)
        }
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: exports.appendingPathComponent("Open-Chronicle.mcpb").path
            )
        )
        assertPackagingScratchWasCleaned()
    }

    func testSystemRunnerBoundsLargeOutputWithoutPipeDeadlock() throws {
        let output = root.appendingPathComponent("large-command-output.txt")
        try Data(repeating: 0x61, count: 1_048_576).write(to: output)

        let result = try SystemMCPBCommandRunner(outputLimit: 128).run(
            executableURL: URL(fileURLWithPath: "/bin/cat"),
            arguments: [output.path]
        )

        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(result.standardOutput, String(repeating: "a", count: 128))
        XCTAssertTrue(result.standardOutputTruncated)
    }

    private func makeService(
        commandRunner: any MCPBCommandRunning = SystemMCPBCommandRunner()
    ) -> MCPBPackageService {
        MCPBPackageService(
            applicationBundleURL: applicationBundle,
            packageVersion: "0.1.0",
            commandRunner: commandRunner,
            temporaryDirectory: staging,
            now: { Self.date("2026-07-13T09:00:00Z") },
            makeIdentifier: { "package-test" }
        )
    }

    private func makeGrant() -> DisclosureGrantRecord {
        DisclosureGrantRecord(
            schemaVersion: "1.0",
            grantID: "grant-local-machine",
            clientID: "client-local-machine",
            receiptID: "receipt-private-do-not-package",
            timeScope: .rollingHorizon(seconds: 86_400),
            contentClasses: [.metadata, .derived],
            createdAt: "2026-07-13T09:00:00Z",
            expiresAt: "2026-07-20T09:00:00Z",
            state: "active",
            limits: DisclosureGrantLimits(
                maxPageItems: 50,
                maxResponseBytes: 256 * 1_024,
                maxCumulativeBytes: 64 * 1_024 * 1_024
            ),
            disclosedBytes: 0,
            storeGeneration: 1
        )
    }

    private func installHelper(from source: URL) throws -> URL {
        let helpers = applicationBundle.appendingPathComponent(
            "Contents/Helpers",
            isDirectory: true
        )
        try FileManager.default.createDirectory(at: helpers, withIntermediateDirectories: true)
        let helper = helpers.appendingPathComponent("chronicle-mcp")
        try FileManager.default.copyItem(at: source, to: helper)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755],
            ofItemAtPath: helper.path
        )
        return helper
    }

    private func regularFiles(relativeTo directory: URL) throws -> Set<String> {
        let keys: [URLResourceKey] = [.isRegularFileKey]
        let base = directory.standardizedFileURL.resolvingSymlinksInPath()
        let prefix = base.path + "/"
        guard let enumerator = FileManager.default.enumerator(
            at: base,
            includingPropertiesForKeys: keys
        ) else { return [] }
        var files = Set<String>()
        for case let url as URL in enumerator {
            let resolved = url.standardizedFileURL.resolvingSymlinksInPath()
            if try resolved.resourceValues(forKeys: Set(keys)).isRegularFile == true {
                guard resolved.path.hasPrefix(prefix) else {
                    throw MCPBPackageServiceError.packageValidationFailed
                }
                files.insert(String(resolved.path.dropFirst(prefix.count)))
            }
        }
        return files
    }

    private func assertRedacted(
        _ error: Error,
        grant: DisclosureGrantRecord,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let message = error.localizedDescription
        for privateValue in [managedRoot.path, grant.clientID, grant.grantID, grant.receiptID] {
            XCTAssertFalse(message.contains(privateValue), file: file, line: line)
        }
    }

    private func assertPackagingScratchWasCleaned(
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: staging.appendingPathComponent(
                    "open-chronicle-mcpb-package-test"
                ).path
            ),
            file: file,
            line: line
        )
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: exports.appendingPathComponent(
                    ".open-chronicle-package-test.mcpb.partial"
                ).path
            ),
            file: file,
            line: line
        )
    }

    private static func date(_ value: String) -> Date {
        ISO8601DateFormatter().date(from: value)!
    }
}

private final class FailingArchiveRunner: MCPBCommandRunning, @unchecked Sendable {
    func run(executableURL: URL, arguments: [String]) throws -> MCPBCommandResult {
        if executableURL.lastPathComponent == "lipo" {
            return MCPBCommandResult(exitCode: 0, standardOutput: "x86_64 arm64")
        }
        return MCPBCommandResult(exitCode: 1, standardOutput: "")
    }
}

private final class MalformedArchiveRunner: MCPBCommandRunning, @unchecked Sendable {
    private let system = SystemMCPBCommandRunner()

    func run(executableURL: URL, arguments: [String]) throws -> MCPBCommandResult {
        if executableURL.lastPathComponent == "ditto",
           arguments.first == "-c",
           let destination = arguments.last
        {
            try Data("not a zip archive".utf8).write(to: URL(fileURLWithPath: destination))
            return MCPBCommandResult(exitCode: 0, standardOutput: "")
        }
        return try system.run(executableURL: executableURL, arguments: arguments)
    }
}

private final class TamperingArchiveRunner: MCPBCommandRunning, @unchecked Sendable {
    enum Mutation {
        case manifest
        case unexpectedEntry
    }

    private let mutation: Mutation
    private let system = SystemMCPBCommandRunner()

    init(mutation: Mutation) {
        self.mutation = mutation
    }

    func run(executableURL: URL, arguments: [String]) throws -> MCPBCommandResult {
        let created = try system.run(executableURL: executableURL, arguments: arguments)
        guard executableURL.lastPathComponent == "ditto",
              arguments.first == "-c",
              created.exitCode == 0,
              let destination = arguments.last
        else { return created }

        let files = FileManager.default
        let archive = URL(fileURLWithPath: destination)
        let scratch = files.temporaryDirectory.appendingPathComponent(
            "open-chronicle-mcpb-tamper-\(UUID().uuidString)",
            isDirectory: true
        )
        defer { try? files.removeItem(at: scratch) }
        try files.createDirectory(at: scratch, withIntermediateDirectories: true)
        let extracted = try system.run(
            executableURL: URL(fileURLWithPath: "/usr/bin/ditto"),
            arguments: ["-x", "-k", archive.path, scratch.path]
        )
        guard extracted.exitCode == 0 else { return extracted }

        switch mutation {
        case .manifest:
            let manifestURL = scratch.appendingPathComponent("manifest.json")
            var manifest = try XCTUnwrap(
                JSONSerialization.jsonObject(with: Data(contentsOf: manifestURL))
                    as? [String: Any]
            )
            manifest["manifest_version"] = "0.3"
            try JSONSerialization.data(withJSONObject: manifest, options: [.sortedKeys])
                .write(to: manifestURL, options: .atomic)
        case .unexpectedEntry:
            try Data("unexpected".utf8).write(
                to: scratch.appendingPathComponent("unexpected.txt")
            )
        }

        try files.removeItem(at: archive)
        return try system.run(
            executableURL: URL(fileURLWithPath: "/usr/bin/ditto"),
            arguments: [
                "-c", "-k", "--norsrc", "--noextattr", "--noqtn", "--noacl",
                scratch.path,
                archive.path,
            ]
        )
    }
}
