import AppKit
import Foundation

enum AgentKind: String, CaseIterable, Codable, Identifiable, Sendable {
    case codex
    case claudeCode = "claude-code"
    case claudeDesktop = "claude-desktop"

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .codex: "Codex"
        case .claudeCode: "Claude Code"
        case .claudeDesktop: "Claude Desktop"
        }
    }
}

enum AgentSupport: Equatable, Sendable {
    case supported
    case unsupported
}

struct AgentInstallation: Identifiable, Equatable, Sendable {
    var id: String { kind.rawValue }
    let kind: AgentKind
    let executableURL: URL?
    let applicationURL: URL?
    let version: String?
    let support: AgentSupport
    let alternateExecutableURLs: [URL]

    var hasDuplicateExecutables: Bool {
        !alternateExecutableURLs.isEmpty
    }
}

struct AgentCommandResult: Equatable, Sendable {
    let exitCode: Int32
    let standardOutput: String
    let standardError: String
}

enum AgentCommandError: LocalizedError, Equatable {
    case unsafeExecutable
    case launchFailed
    case timedOut

    var errorDescription: String? {
        switch self {
        case .unsafeExecutable: "The agent command was not an absolute executable path."
        case .launchFailed: "The agent command could not be started."
        case .timedOut: "The agent command did not finish in time."
        }
    }
}

protocol AgentCommandRunning: Sendable {
    func run(executableURL: URL, arguments: [String]) async throws -> AgentCommandResult
}

final class SystemAgentCommandRunner: AgentCommandRunning, @unchecked Sendable {
    private let timeout: TimeInterval
    private let outputLimit: Int

    init(timeout: TimeInterval = 15, outputLimit: Int = 64 * 1_024) {
        self.timeout = timeout
        self.outputLimit = outputLimit
    }

    func run(executableURL: URL, arguments: [String]) async throws -> AgentCommandResult {
        let timeout = timeout
        let outputLimit = outputLimit
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                do {
                    continuation.resume(
                        returning: try Self.runSynchronously(
                            executableURL: executableURL,
                            arguments: arguments,
                            timeout: timeout,
                            outputLimit: outputLimit
                        )
                    )
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    private static func runSynchronously(
        executableURL: URL,
        arguments: [String],
        timeout: TimeInterval,
        outputLimit: Int
    ) throws -> AgentCommandResult {
        let executable = executableURL.standardizedFileURL.resolvingSymlinksInPath()
        guard executable.isFileURL,
              executable.path.hasPrefix("/"),
              FileManager.default.isExecutableFile(atPath: executable.path)
        else {
            throw AgentCommandError.unsafeExecutable
        }

        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        let stdout = BoundedCommandOutput(limit: outputLimit)
        let stderr = BoundedCommandOutput(limit: outputLimit)
        let readers = DispatchGroup()
        readers.enter()
        DispatchQueue.global(qos: .utility).async {
            stdout.drain(stdoutPipe.fileHandleForReading)
            readers.leave()
        }
        readers.enter()
        DispatchQueue.global(qos: .utility).async {
            stderr.drain(stderrPipe.fileHandleForReading)
            readers.leave()
        }

        do {
            try process.run()
        } catch {
            stdoutPipe.fileHandleForWriting.closeFile()
            stderrPipe.fileHandleForWriting.closeFile()
            readers.wait()
            throw AgentCommandError.launchFailed
        }

        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.025)
        }
        let timedOut = process.isRunning
        if timedOut {
            process.terminate()
        }
        process.waitUntilExit()
        readers.wait()
        if timedOut { throw AgentCommandError.timedOut }

        return AgentCommandResult(
            exitCode: process.terminationStatus,
            standardOutput: stdout.string,
            standardError: stderr.string
        )
    }
}

private final class BoundedCommandOutput: @unchecked Sendable {
    private let limit: Int
    private let lock = NSLock()
    private var data = Data()

    init(limit: Int) {
        self.limit = max(0, limit)
    }

    func drain(_ handle: FileHandle) {
        while true {
            let next = handle.availableData
            if next.isEmpty { break }
            lock.lock()
            if data.count < limit {
                data.append(next.prefix(limit - data.count))
            }
            lock.unlock()
        }
    }

    var string: String {
        lock.lock()
        defer { lock.unlock() }
        return String(decoding: data, as: UTF8.self)
    }
}

@MainActor
protocol AgentApplicationLocating: AnyObject {
    func applicationURL(bundleIdentifier: String) -> URL?
}

@MainActor
final class WorkspaceAgentApplicationLocator: AgentApplicationLocating {
    func applicationURL(bundleIdentifier: String) -> URL? {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleIdentifier)
    }
}

struct AgentDetectionEnvironment: Sendable {
    let homeDirectory: URL
    let pathDirectories: [URL]
    let fixedExecutableDirectories: [URL]

    static func current(
        processInfo: ProcessInfo = .processInfo,
        fileManager: FileManager = .default
    ) -> AgentDetectionEnvironment {
        let paths = processInfo.environment["PATH", default: ""]
            .split(separator: ":", omittingEmptySubsequences: true)
            .map { URL(fileURLWithPath: String($0), isDirectory: true) }
        return AgentDetectionEnvironment(
            homeDirectory: fileManager.homeDirectoryForCurrentUser,
            pathDirectories: paths,
            fixedExecutableDirectories: [
                URL(fileURLWithPath: "/opt/homebrew/bin", isDirectory: true),
                URL(fileURLWithPath: "/usr/local/bin", isDirectory: true),
            ]
        )
    }
}

@MainActor
final class AgentDetectionService {
    private let runner: any AgentCommandRunning
    private let applications: any AgentApplicationLocating
    private let environment: AgentDetectionEnvironment
    private let fileManager: FileManager

    init(
        runner: any AgentCommandRunning = SystemAgentCommandRunner(),
        applications: (any AgentApplicationLocating)? = nil,
        environment: AgentDetectionEnvironment = .current(),
        fileManager: FileManager = .default
    ) {
        self.runner = runner
        self.applications = applications ?? WorkspaceAgentApplicationLocator()
        self.environment = environment
        self.fileManager = fileManager
    }

    func detect() async -> [AgentInstallation] {
        var installations: [AgentInstallation] = []
        if let codex = await detectCLI(
            kind: .codex,
            name: "codex",
            bundledCandidates: codexBundledCandidates(),
            capabilityArguments: ["mcp", "get", "--help"]
        ) {
            installations.append(codex)
        }
        if let claude = await detectCLI(
            kind: .claudeCode,
            name: "claude",
            bundledCandidates: [],
            capabilityArguments: ["mcp", "add", "--help"]
        ) {
            installations.append(claude)
        }
        if let desktopURL = applications.applicationURL(
            bundleIdentifier: "com.anthropic.claudefordesktop"
        ) {
            installations.append(
                AgentInstallation(
                    kind: .claudeDesktop,
                    executableURL: nil,
                    applicationURL: desktopURL,
                    version: Bundle(url: desktopURL)?.object(
                        forInfoDictionaryKey: "CFBundleShortVersionString"
                    ) as? String,
                    support: .supported,
                    alternateExecutableURLs: []
                )
            )
        }
        return installations
    }

    private func detectCLI(
        kind: AgentKind,
        name: String,
        bundledCandidates: [URL],
        capabilityArguments: [String]
    ) async -> AgentInstallation? {
        let candidates = executableCandidates(name: name) + bundledCandidates
        var seen = Set<String>()
        let executables = candidates.compactMap { candidate -> URL? in
            let resolved = candidate.standardizedFileURL.resolvingSymlinksInPath()
            guard fileManager.isExecutableFile(atPath: resolved.path),
                  seen.insert(resolved.path).inserted
            else { return nil }
            return resolved
        }
        guard let primary = executables.first else { return nil }

        let versionResult = try? await runner.run(
            executableURL: primary,
            arguments: ["--version"]
        )
        let capability = try? await runner.run(
            executableURL: primary,
            arguments: capabilityArguments
        )
        let version = versionResult.flatMap { result -> String? in
            guard result.exitCode == 0 else { return nil }
            return Self.safeVersion(result.standardOutput)
        }
        let supported = capability?.exitCode == 0
        return AgentInstallation(
            kind: kind,
            executableURL: primary,
            applicationURL: nil,
            version: version,
            support: supported ? .supported : .unsupported,
            alternateExecutableURLs: Array(executables.dropFirst())
        )
    }

    private func executableCandidates(name: String) -> [URL] {
        var directories = environment.pathDirectories
        directories.append(
            environment.homeDirectory.appendingPathComponent(".local/bin", isDirectory: true)
        )
        directories.append(contentsOf: environment.fixedExecutableDirectories)
        return directories.map { $0.appendingPathComponent(name, isDirectory: false) }
    }

    private func codexBundledCandidates() -> [URL] {
        ["com.openai.codex", "com.openai.chat"].compactMap { identifier in
            applications.applicationURL(bundleIdentifier: identifier)?
                .appendingPathComponent("Contents/Resources/codex", isDirectory: false)
        }
    }

    private static func safeVersion(_ output: String) -> String? {
        guard let line = output
            .split(whereSeparator: { $0.isNewline })
            .first
            .map(String.init)
            .map({ $0.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines) }),
            !line.isEmpty
        else { return nil }
        return String(line.prefix(160))
    }
}
