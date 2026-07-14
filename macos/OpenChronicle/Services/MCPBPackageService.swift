import CryptoKit
import Foundation

struct MCPBPackageReceipt: Equatable, Sendable {
    let packageURL: URL
    let packageFileName: String
    let grantExpiresAt: String
    let scopeDescription: String
}

enum MCPBPackageServiceError: LocalizedError, Equatable {
    case invalidDestination
    case managedRootUnavailable
    case grantUnavailable
    case helperMissing
    case helperNotExecutable
    case helperNotUniversal
    case stagingFailed
    case packagingFailed
    case packageValidationFailed

    var errorDescription: String? {
        switch self {
        case .invalidDestination:
            "Choose a writable local folder and package name."
        case .managedRootUnavailable:
            "Open Chronicle's local data store is not ready."
        case .grantUnavailable:
            "Create or renew the Claude Desktop disclosure grant, then try again."
        case .helperMissing:
            "This Open Chronicle build does not contain the Claude Desktop helper."
        case .helperNotExecutable:
            "The bundled Claude Desktop helper is not executable. Reinstall Open Chronicle."
        case .helperNotUniversal:
            "The bundled Claude Desktop helper does not support both Apple silicon and Intel Macs."
        case .stagingFailed:
            "Open Chronicle could not prepare the Claude Desktop package."
        case .packagingFailed:
            "Open Chronicle could not create the Claude Desktop package."
        case .packageValidationFailed:
            "The Claude Desktop package could not be verified."
        }
    }
}

struct MCPBCommandResult: Equatable, Sendable {
    let exitCode: Int32
    let standardOutput: String
    let standardOutputTruncated: Bool

    init(
        exitCode: Int32,
        standardOutput: String,
        standardOutputTruncated: Bool = false
    ) {
        self.exitCode = exitCode
        self.standardOutput = standardOutput
        self.standardOutputTruncated = standardOutputTruncated
    }
}

protocol MCPBCommandRunning: Sendable {
    func run(executableURL: URL, arguments: [String]) throws -> MCPBCommandResult
}

final class SystemMCPBCommandRunner: MCPBCommandRunning, @unchecked Sendable {
    private let outputLimit: Int

    init(outputLimit: Int = 8 * 1_024) {
        self.outputLimit = max(0, outputLimit)
    }

    func run(executableURL: URL, arguments: [String]) throws -> MCPBCommandResult {
        let executable = executableURL.standardizedFileURL.resolvingSymlinksInPath()
        guard executable.isFileURL,
              executable.path.hasPrefix("/"),
              FileManager.default.isExecutableFile(atPath: executable.path)
        else {
            throw MCPBPackageServiceError.packagingFailed
        }

        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        let outputURL = FileManager.default.temporaryDirectory.appendingPathComponent(
            ".open-chronicle-command-\(UUID().uuidString.lowercased())"
        )
        guard FileManager.default.createFile(
            atPath: outputURL.path,
            contents: nil,
            attributes: [.posixPermissions: 0o600]
        ) else {
            throw MCPBPackageServiceError.packagingFailed
        }
        defer { try? FileManager.default.removeItem(at: outputURL) }
        let outputWriter: FileHandle
        do {
            outputWriter = try FileHandle(forWritingTo: outputURL)
        } catch {
            throw MCPBPackageServiceError.packagingFailed
        }
        process.standardOutput = outputWriter
        // File-backed stdout cannot fill a pipe while `waitUntilExit()` waits.
        // Stderr is never user-facing and is intentionally discarded.
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            try? outputWriter.close()
            throw MCPBPackageServiceError.packagingFailed
        }
        process.waitUntilExit()
        try? outputWriter.close()
        let outputReader: FileHandle
        do {
            outputReader = try FileHandle(forReadingFrom: outputURL)
        } catch {
            throw MCPBPackageServiceError.packagingFailed
        }
        defer { try? outputReader.close() }
        let output: Data
        do {
            let readLimit = outputLimit == Int.max ? Int.max : outputLimit + 1
            output = try outputReader.read(upToCount: readLimit) ?? Data()
        } catch {
            throw MCPBPackageServiceError.packagingFailed
        }
        return MCPBCommandResult(
            exitCode: process.terminationStatus,
            standardOutput: String(decoding: output.prefix(outputLimit), as: UTF8.self),
            standardOutputTruncated: output.count > outputLimit
        )
    }
}

struct MCPBPackageService: @unchecked Sendable {
    static let manifestSchema =
        "https://raw.githubusercontent.com/modelcontextprotocol/mcpb/main/schemas/mcpb-manifest-v0.4.schema.json"
    static let scopeDescription =
        "Bound to this Mac and one revocable Open Chronicle disclosure grant."

    private let applicationBundleURL: URL
    private let packageVersion: String
    private let fileManager: FileManager
    private let commandRunner: any MCPBCommandRunning
    private let temporaryDirectory: URL
    private let now: () -> Date
    private let makeIdentifier: () -> String

    init(
        applicationBundleURL: URL = Bundle.main.bundleURL,
        packageVersion: String = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleShortVersionString"
        ) as? String ?? "0.1.0",
        fileManager: FileManager = .default,
        commandRunner: any MCPBCommandRunning = SystemMCPBCommandRunner(),
        temporaryDirectory: URL = FileManager.default.temporaryDirectory,
        now: @escaping () -> Date = Date.init,
        makeIdentifier: @escaping () -> String = { UUID().uuidString.lowercased() }
    ) {
        self.applicationBundleURL = applicationBundleURL
        self.packageVersion = packageVersion
        self.fileManager = fileManager
        self.commandRunner = commandRunner
        self.temporaryDirectory = temporaryDirectory
        self.now = now
        self.makeIdentifier = makeIdentifier
    }

    func createPackage(
        at selectedURL: URL,
        managedRootURL: URL,
        grant: DisclosureGrantRecord
    ) throws -> MCPBPackageReceipt {
        let outputURL = try sanitizedOutputURL(selectedURL)
        let managedRoot = try validatedManagedRoot(managedRootURL)
        let expiresAt = try validatedGrant(grant)
        let helper = try validatedHelper()

        let identifier = Self.sanitizeInternalIdentifier(makeIdentifier())
        let stageRoot = temporaryDirectory.appendingPathComponent(
            "open-chronicle-mcpb-\(identifier)",
            isDirectory: true
        )
        let bundleRoot = stageRoot.appendingPathComponent("bundle", isDirectory: true)
        let serverDirectory = bundleRoot.appendingPathComponent("server", isDirectory: true)
        let stagedHelper = serverDirectory.appendingPathComponent("chronicle-mcp")
        let manifestURL = bundleRoot.appendingPathComponent("manifest.json")
        let validationDirectory = stageRoot.appendingPathComponent(
            "validation",
            isDirectory: true
        )
        let partialArchive = outputURL.deletingLastPathComponent().appendingPathComponent(
            ".open-chronicle-\(identifier).mcpb.partial"
        )
        defer {
            try? fileManager.removeItem(at: stageRoot)
            try? fileManager.removeItem(at: partialArchive)
        }

        let expectedManifest = makeManifest(
            managedRoot: managedRoot,
            grant: grant,
            expiresAt: expiresAt
        )
        do {
            try fileManager.createDirectory(
                at: serverDirectory,
                withIntermediateDirectories: true
            )
            try fileManager.copyItem(at: helper, to: stagedHelper)
            try fileManager.setAttributes(
                [.posixPermissions: 0o755],
                ofItemAtPath: stagedHelper.path
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
            try encoder.encode(expectedManifest).write(to: manifestURL, options: .atomic)
        } catch {
            throw MCPBPackageServiceError.stagingFailed
        }

        let archive: MCPBCommandResult
        do {
            archive = try commandRunner.run(
                executableURL: URL(fileURLWithPath: "/usr/bin/ditto"),
                arguments: [
                    "-c", "-k", "--norsrc", "--noextattr", "--noqtn", "--noacl",
                    bundleRoot.path,
                    partialArchive.path,
                ]
            )
        } catch {
            throw MCPBPackageServiceError.packagingFailed
        }
        guard archive.exitCode == 0,
              fileManager.fileExists(atPath: partialArchive.path),
              (try? partialArchive.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0 > 0
        else {
            throw MCPBPackageServiceError.packagingFailed
        }

        do {
            try validateArchive(
                partialArchive,
                extractingTo: validationDirectory,
                expectedManifest: expectedManifest,
                stagedHelper: stagedHelper
            )
        } catch {
            throw MCPBPackageServiceError.packageValidationFailed
        }

        do {
            if fileManager.fileExists(atPath: outputURL.path) {
                _ = try fileManager.replaceItemAt(outputURL, withItemAt: partialArchive)
            } else {
                try fileManager.moveItem(at: partialArchive, to: outputURL)
            }
        } catch {
            throw MCPBPackageServiceError.packagingFailed
        }
        guard fileManager.fileExists(atPath: outputURL.path) else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        return MCPBPackageReceipt(
            packageURL: outputURL,
            packageFileName: outputURL.lastPathComponent,
            grantExpiresAt: grant.expiresAt,
            scopeDescription: Self.scopeDescription
        )
    }

    private func validateArchive(
        _ archive: URL,
        extractingTo validationDirectory: URL,
        expectedManifest: MCPBManifest,
        stagedHelper: URL
    ) throws {
        try requireRegularFile(archive)
        let archiveDigest = try sha256(archive)
        let listing = try commandRunner.run(
            executableURL: URL(fileURLWithPath: "/usr/bin/zipinfo"),
            arguments: ["-1", archive.path]
        )
        let entries = listing.standardOutput
            .split(whereSeparator: \Character.isNewline)
            .map(String.init)
        let expectedEntries = Set([
            "manifest.json",
            "server/",
            "server/chronicle-mcp",
        ])
        guard listing.exitCode == 0,
              !listing.standardOutputTruncated,
              entries.count == expectedEntries.count,
              Set(entries) == expectedEntries
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }

        try fileManager.createDirectory(
            at: validationDirectory,
            withIntermediateDirectories: false
        )
        let extraction = try commandRunner.run(
            executableURL: URL(fileURLWithPath: "/usr/bin/ditto"),
            arguments: ["-x", "-k", archive.path, validationDirectory.path]
        )
        guard extraction.exitCode == 0 else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        let extracted = try validateExtractedTree(validationDirectory)

        let manifestValues = try extracted.manifest.resourceValues(forKeys: [.fileSizeKey])
        guard let manifestSize = manifestValues.fileSize,
              manifestSize > 0,
              manifestSize <= 64 * 1_024
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        let manifestData = try Data(contentsOf: extracted.manifest, options: [.mappedIfSafe])
        let decodedManifest = try JSONDecoder().decode(MCPBManifest.self, from: manifestData)
        guard decodedManifest == expectedManifest,
              try jsonObject(manifestData) == jsonObject(JSONEncoder().encode(expectedManifest))
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }

        guard fileManager.isExecutableFile(atPath: extracted.helper.path),
              fileManager.contentsEqual(
                atPath: extracted.helper.path,
                andPath: stagedHelper.path
              ),
              try hasUniversalArchitectures(extracted.helper)
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        try requireRegularFile(archive)
        guard try sha256(archive) == archiveDigest else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
    }

    private func validateExtractedTree(
        _ directory: URL
    ) throws -> (manifest: URL, helper: URL) {
        let base = directory.standardizedFileURL.resolvingSymlinksInPath()
        let prefix = base.path + "/"
        let keys: Set<URLResourceKey> = [
            .isDirectoryKey,
            .isRegularFileKey,
            .isSymbolicLinkKey,
        ]
        guard let enumerator = fileManager.enumerator(
            at: base,
            includingPropertiesForKeys: Array(keys)
        ) else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        var regularFiles = Set<String>()
        var directories = Set<String>()
        for case let item as URL in enumerator {
            let lexical = item.standardizedFileURL
            guard lexical.path.hasPrefix(prefix) else {
                throw MCPBPackageServiceError.packageValidationFailed
            }
            let relative = String(lexical.path.dropFirst(prefix.count))
            let values = try lexical.resourceValues(forKeys: keys)
            let resolved = lexical.resolvingSymlinksInPath()
            guard values.isSymbolicLink != true,
                  resolved.path.hasPrefix(prefix)
            else {
                throw MCPBPackageServiceError.packageValidationFailed
            }
            if values.isDirectory == true {
                directories.insert(relative)
            } else if values.isRegularFile == true {
                regularFiles.insert(relative)
            } else {
                throw MCPBPackageServiceError.packageValidationFailed
            }
        }
        guard directories == ["server"],
              regularFiles == ["manifest.json", "server/chronicle-mcp"]
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        return (
            base.appendingPathComponent("manifest.json"),
            base.appendingPathComponent("server/chronicle-mcp")
        )
    }

    private func requireRegularFile(_ url: URL) throws {
        let attributes = try fileManager.attributesOfItem(atPath: url.path)
        guard attributes[.type] as? FileAttributeType == .typeRegular,
              (attributes[.size] as? NSNumber)?.uint64Value ?? 0 > 0
        else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
    }

    private func sha256(_ url: URL) throws -> Data {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let data = try handle.read(upToCount: 64 * 1_024), !data.isEmpty {
            hasher.update(data: data)
        }
        return Data(hasher.finalize())
    }

    private func jsonObject(_ data: Data) throws -> Data {
        let value = try JSONSerialization.jsonObject(with: data)
        guard value is [String: Any] else {
            throw MCPBPackageServiceError.packageValidationFailed
        }
        return try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    }

    private func sanitizedOutputURL(_ selectedURL: URL) throws -> URL {
        guard selectedURL.isFileURL, selectedURL.path.hasPrefix("/") else {
            throw MCPBPackageServiceError.invalidDestination
        }
        let parent = selectedURL.deletingLastPathComponent().standardizedFileURL
        var isDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: parent.path, isDirectory: &isDirectory),
              isDirectory.boolValue,
              fileManager.isWritableFile(atPath: parent.path)
        else {
            throw MCPBPackageServiceError.invalidDestination
        }
        let requestedStem = selectedURL.deletingPathExtension().lastPathComponent
        let sanitized = Self.sanitizeFileName(requestedStem)
        return parent.appendingPathComponent(sanitized + ".mcpb", isDirectory: false)
    }

    private func validatedManagedRoot(_ url: URL) throws -> URL {
        let root = url.standardizedFileURL.resolvingSymlinksInPath()
        var isDirectory: ObjCBool = false
        guard root.isFileURL,
              root.path.hasPrefix("/"),
              fileManager.fileExists(atPath: root.path, isDirectory: &isDirectory),
              isDirectory.boolValue
        else {
            throw MCPBPackageServiceError.managedRootUnavailable
        }
        return root
    }

    private func validatedGrant(_ grant: DisclosureGrantRecord) throws -> Date {
        guard grant.state == "active",
              Self.isRegistrationID(grant.clientID),
              Self.isRegistrationID(grant.grantID),
              let expiry = Self.parseTimestamp(grant.expiresAt),
              expiry > now()
        else {
            throw MCPBPackageServiceError.grantUnavailable
        }
        return expiry
    }

    private func validatedHelper() throws -> URL {
        let bundle = applicationBundleURL.standardizedFileURL.resolvingSymlinksInPath()
        let helper = bundle
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("Helpers", isDirectory: true)
            .appendingPathComponent("chronicle-mcp", isDirectory: false)
        let resolvedHelper = helper.resolvingSymlinksInPath()
        let helpersPrefix = bundle
            .appendingPathComponent("Contents/Helpers", isDirectory: true)
            .path + "/"
        let attributes = try? fileManager.attributesOfItem(atPath: helper.path)
        let fileType = attributes?[.type] as? FileAttributeType
        var isDirectory: ObjCBool = false
        guard fileManager.fileExists(atPath: helper.path, isDirectory: &isDirectory),
              !isDirectory.boolValue,
              resolvedHelper.path.hasPrefix(helpersPrefix),
              fileType == .typeRegular
        else {
            throw MCPBPackageServiceError.helperMissing
        }
        guard fileManager.isExecutableFile(atPath: helper.path) else {
            throw MCPBPackageServiceError.helperNotExecutable
        }
        do {
            guard try hasUniversalArchitectures(helper) else {
                throw MCPBPackageServiceError.helperNotUniversal
            }
        } catch {
            throw MCPBPackageServiceError.helperNotUniversal
        }
        return helper
    }

    private func hasUniversalArchitectures(_ helper: URL) throws -> Bool {
        let result = try commandRunner.run(
            executableURL: URL(fileURLWithPath: "/usr/bin/lipo"),
            arguments: ["-archs", helper.path]
        )
        let architectures = Set(result.standardOutput.split { $0.isWhitespace })
        return result.exitCode == 0
            && !result.standardOutputTruncated
            && architectures.contains("x86_64")
            && (architectures.contains("arm64") || architectures.contains("arm64e"))
    }

    private func makeManifest(
        managedRoot: URL,
        grant: DisclosureGrantRecord,
        expiresAt: Date
    ) -> MCPBManifest {
        let expiry = Self.displayTimestamp(expiresAt)
        return MCPBManifest(
            schema: Self.manifestSchema,
            manifestVersion: "0.4",
            name: "open-chronicle",
            displayName: "Open Chronicle",
            version: Self.isSemanticVersion(packageVersion) ? packageVersion : "0.1.0",
            description: "Local, grant-scoped access to your Open Chronicle evidence.",
            longDescription: "Runs entirely on this Mac over local stdio. This package is bound to one Open Chronicle data store and one revocable disclosure grant, which expires \(expiry). Re-export after moving the app or data, or after replacing the grant. Do not share this machine-specific package.",
            author: MCPBManifestAuthor(name: "Open Chronicle"),
            server: MCPBManifestServer(
                type: "binary",
                entryPoint: "server/chronicle-mcp",
                mcpConfig: MCPBManifestConfiguration(
                    command: "${__dirname}/server/chronicle-mcp",
                    arguments: [
                        "--managed-root", managedRoot.path,
                        "--client-id", grant.clientID,
                        "--grant-id", grant.grantID,
                    ],
                    environment: [:]
                )
            ),
            toolsGenerated: true,
            keywords: ["chronicle", "local", "mcp", "time"],
            license: "Apache-2.0",
            compatibility: MCPBManifestCompatibility(platforms: ["darwin"]),
            metadata: [
                "com.screenata.openchronicle": MCPBManifestScope(
                    machineScoped: true,
                    grantScoped: true,
                    grantExpiresAt: grant.expiresAt,
                    transport: "local-stdio",
                    networkRequired: false
                )
            ]
        )
    }

    private static func sanitizeFileName(_ value: String) -> String {
        var output = ""
        var needsSeparator = false
        for scalar in value.unicodeScalars {
            if CharacterSet.alphanumerics.contains(scalar) || scalar == "-" || scalar == "_" {
                if needsSeparator, !output.isEmpty { output.append("-") }
                output.unicodeScalars.append(scalar)
                needsSeparator = false
            } else {
                needsSeparator = true
            }
        }
        let trimmed = output.trimmingCharacters(in: CharacterSet(charactersIn: "-_."))
        return String((trimmed.isEmpty ? "Open-Chronicle" : trimmed).prefix(80))
    }

    private static func isRegistrationID(_ value: String) -> Bool {
        guard !value.isEmpty, value.utf8.count <= 255 else { return false }
        return value.utf8.allSatisfy { byte in
            (byte >= 48 && byte <= 57)
                || (byte >= 65 && byte <= 90)
                || (byte >= 97 && byte <= 122)
                || byte == 45
                || byte == 46
                || byte == 95
        }
    }

    private static func sanitizeInternalIdentifier(_ value: String) -> String {
        let safe = value.utf8.filter { byte in
            (byte >= 48 && byte <= 57)
                || (byte >= 65 && byte <= 90)
                || (byte >= 97 && byte <= 122)
                || byte == 45
                || byte == 95
        }
        return safe.isEmpty ? UUID().uuidString.lowercased() : String(decoding: safe, as: UTF8.self)
    }

    private static func isSemanticVersion(_ value: String) -> Bool {
        value.range(
            of: #"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"#,
            options: .regularExpression
        ) != nil
    }

    private static func parseTimestamp(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let parsed = fractional.date(from: value) { return parsed }
        let standard = ISO8601DateFormatter()
        standard.formatOptions = [.withInternetDateTime]
        return standard.date(from: value)
    }

    private static func displayTimestamp(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyyy-MM-dd HH:mm 'UTC'"
        return formatter.string(from: date)
    }
}

private struct MCPBManifest: Codable, Equatable {
    let schema: String
    let manifestVersion: String
    let name: String
    let displayName: String
    let version: String
    let description: String
    let longDescription: String
    let author: MCPBManifestAuthor
    let server: MCPBManifestServer
    let toolsGenerated: Bool
    let keywords: [String]
    let license: String
    let compatibility: MCPBManifestCompatibility
    let metadata: [String: MCPBManifestScope]

    enum CodingKeys: String, CodingKey {
        case schema = "$schema"
        case manifestVersion = "manifest_version"
        case name
        case displayName = "display_name"
        case version
        case description
        case longDescription = "long_description"
        case author
        case server
        case toolsGenerated = "tools_generated"
        case keywords
        case license
        case compatibility
        case metadata = "_meta"
    }
}

private struct MCPBManifestAuthor: Codable, Equatable {
    let name: String
}

private struct MCPBManifestServer: Codable, Equatable {
    let type: String
    let entryPoint: String
    let mcpConfig: MCPBManifestConfiguration

    enum CodingKeys: String, CodingKey {
        case type
        case entryPoint = "entry_point"
        case mcpConfig = "mcp_config"
    }
}

private struct MCPBManifestConfiguration: Codable, Equatable {
    let command: String
    let arguments: [String]
    let environment: [String: String]

    enum CodingKeys: String, CodingKey {
        case command
        case arguments = "args"
        case environment = "env"
    }
}

private struct MCPBManifestCompatibility: Codable, Equatable {
    let platforms: [String]
}

private struct MCPBManifestScope: Codable, Equatable {
    let machineScoped: Bool
    let grantScoped: Bool
    let grantExpiresAt: String
    let transport: String
    let networkRequired: Bool

    enum CodingKeys: String, CodingKey {
        case machineScoped = "machine_scoped"
        case grantScoped = "grant_scoped"
        case grantExpiresAt = "grant_expires_at"
        case transport
        case networkRequired = "network_required"
    }
}
