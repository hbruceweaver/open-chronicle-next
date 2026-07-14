import CryptoKit
import Foundation

struct AgentRegistrationPlan: Equatable, Sendable {
    static let serverName = "open-chronicle"

    let applicationBundleURL: URL
    let helperURL: URL
    let managedRootURL: URL
    let clientID: String
    let grantID: String

    var helperArguments: [String] {
        [
            "--managed-root", managedRootURL.standardizedFileURL.path,
            "--client-id", clientID,
            "--grant-id", grantID,
        ]
    }
}

enum AgentRegistrationPriorState: String, Codable, Sendable {
    case absent
    case exact
}

enum AgentRegistrationResult: String, Codable, Sendable {
    case added
    case adopted
}

struct AgentRegistrationReceipt: Codable, Equatable, Sendable {
    static let schemaVersion = 1

    let schemaVersion: Int
    let agentKind: AgentKind
    let agentVersion: String?
    let serverName: String
    let clientID: String
    let resolvedHelperPath: String
    let managedRootPath: String
    let argumentDigest: String
    let priorState: AgentRegistrationPriorState
    let result: AgentRegistrationResult
    let registeredAt: Date
}

enum AgentRegistrationIssue: String, Equatable, Sendable {
    case clientUnavailable = "client-unavailable"
    case inspectionFailed = "inspection-failed"
    case registrationFailed = "registration-failed"
    case verificationFailed = "verification-failed"
    case receiptMissing = "receipt-missing"
    case receiptMismatch = "receipt-mismatch"
    case removalFailed = "removal-failed"
    case grantFailed = "grant-failed"
    case credentialStorageFailed = "credential-storage-failed"
}

enum AgentRegistrationOutcome: Equatable, Sendable {
    case registered(AgentRegistrationReceipt)
    case alreadyRegistered(AgentRegistrationReceipt)
    case removed
    case guidedDesktop
    case conflict
    case blocked(InstallLocationBlock)
    case unsupported
    case failed(AgentRegistrationIssue)
}

@MainActor
protocol AgentRegistrationReceiptStoring: AnyObject {
    func receipt(for kind: AgentKind) -> AgentRegistrationReceipt?
    func save(_ receipt: AgentRegistrationReceipt)
    func remove(kind: AgentKind)
}

@MainActor
final class UserDefaultsAgentRegistrationReceiptStore: AgentRegistrationReceiptStoring {
    static let keyPrefix = "agent-registration-receipt.v1."

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func receipt(for kind: AgentKind) -> AgentRegistrationReceipt? {
        guard let data = defaults.data(forKey: Self.keyPrefix + kind.rawValue),
              let receipt = try? JSONDecoder().decode(AgentRegistrationReceipt.self, from: data),
              receipt.schemaVersion == AgentRegistrationReceipt.schemaVersion,
              receipt.agentKind == kind
        else { return nil }
        return receipt
    }

    func save(_ receipt: AgentRegistrationReceipt) {
        guard let encoded = try? JSONEncoder().encode(receipt) else { return }
        defaults.set(encoded, forKey: Self.keyPrefix + receipt.agentKind.rawValue)
    }

    func remove(kind: AgentKind) {
        defaults.removeObject(forKey: Self.keyPrefix + kind.rawValue)
    }
}

@MainActor
final class AgentRegistrationService {
    private enum ExistingRegistration: Equatable {
        case absent
        case exact
        case conflict
        case inaccessible
    }

    private let runner: any AgentCommandRunning
    private let receipts: any AgentRegistrationReceiptStoring
    private let installLocation: InstallLocationService
    private let now: () -> Date

    init(
        runner: any AgentCommandRunning = SystemAgentCommandRunner(),
        receipts: (any AgentRegistrationReceiptStoring)? = nil,
        installLocation: InstallLocationService = InstallLocationService(),
        now: @escaping () -> Date = Date.init
    ) {
        self.runner = runner
        self.receipts = receipts ?? UserDefaultsAgentRegistrationReceiptStore()
        self.installLocation = installLocation
        self.now = now
    }

    func register(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) async -> AgentRegistrationOutcome {
        guard installation.support == .supported else { return .unsupported }
        if installation.kind == .claudeDesktop { return .guidedDesktop }
        guard let executable = installation.executableURL else {
            return .failed(.clientUnavailable)
        }
        let assessment = installLocation.assess(
            applicationBundleURL: plan.applicationBundleURL,
            helperURL: plan.helperURL,
            managedRootURL: plan.managedRootURL
        )
        if case let .blocked(reason) = assessment { return .blocked(reason) }

        let expected = plan.helperArguments
        let existing = await inspect(
            installation: installation,
            executable: executable,
            helperURL: plan.helperURL,
            arguments: expected
        )
        switch existing {
        case .conflict:
            return .conflict
        case .inaccessible:
            return .failed(.inspectionFailed)
        case .exact:
            let receipt = makeReceipt(
                installation: installation,
                plan: plan,
                priorState: .exact,
                result: .adopted
            )
            receipts.save(receipt)
            guard receipts.receipt(for: installation.kind) == receipt else {
                return .failed(.verificationFailed)
            }
            return .alreadyRegistered(receipt)
        case .absent:
            break
        }

        let add: AgentCommandResult
        do {
            add = try await runner.run(
                executableURL: executable,
                arguments: addArguments(kind: installation.kind, plan: plan)
            )
        } catch {
            return .failed(.registrationFailed)
        }
        guard add.exitCode == 0 else { return .failed(.registrationFailed) }

        let verified = await inspect(
            installation: installation,
            executable: executable,
            helperURL: plan.helperURL,
            arguments: expected
        )
        guard verified == .exact else { return .failed(.verificationFailed) }
        let receipt = makeReceipt(
            installation: installation,
            plan: plan,
            priorState: .absent,
            result: .added
        )
        receipts.save(receipt)
        guard receipts.receipt(for: installation.kind) == receipt else {
            return .failed(.verificationFailed)
        }
        return .registered(receipt)
    }

    func unregister(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) async -> AgentRegistrationOutcome {
        guard installation.kind != .claudeDesktop,
              let executable = installation.executableURL
        else { return .failed(.clientUnavailable) }
        guard let receipt = receipts.receipt(for: installation.kind) else {
            return .failed(.receiptMissing)
        }
        guard receiptMatches(receipt, installation: installation, plan: plan) else {
            return .failed(.receiptMismatch)
        }
        let existing = await inspect(
            installation: installation,
            executable: executable,
            helperURL: plan.helperURL,
            arguments: plan.helperArguments
        )
        guard existing == .exact else {
            return existing == .conflict ? .conflict : .failed(.inspectionFailed)
        }

        let removal: AgentCommandResult
        do {
            removal = try await runner.run(
                executableURL: executable,
                arguments: removeArguments(kind: installation.kind)
            )
        } catch {
            return .failed(.removalFailed)
        }
        guard removal.exitCode == 0 else { return .failed(.removalFailed) }
        let after = await inspect(
            installation: installation,
            executable: executable,
            helperURL: plan.helperURL,
            arguments: plan.helperArguments
        )
        guard after == .absent else { return .failed(.verificationFailed) }
        receipts.remove(kind: installation.kind)
        return .removed
    }

    private func inspect(
        installation: AgentInstallation,
        executable: URL,
        helperURL: URL,
        arguments: [String]
    ) async -> ExistingRegistration {
        let result: AgentCommandResult
        do {
            result = try await runner.run(
                executableURL: executable,
                arguments: getArguments(kind: installation.kind)
            )
        } catch {
            return .inaccessible
        }
        let combined = result.standardOutput + "\n" + result.standardError
        if result.exitCode != 0 {
            return Self.isMissingMessage(combined) ? .absent : .inaccessible
        }
        switch installation.kind {
        case .codex:
            guard let command = Self.codexCommand(from: result.standardOutput) else {
                return .conflict
            }
            return command.executable == helperURL.standardizedFileURL.path &&
                command.arguments == arguments ? .exact : .conflict
        case .claudeCode:
            return Self.claudeOutput(
                result.standardOutput,
                matchesHelper: helperURL.standardizedFileURL.path,
                arguments: arguments
            ) ? .exact : .conflict
        case .claudeDesktop:
            return .inaccessible
        }
    }

    private func makeReceipt(
        installation: AgentInstallation,
        plan: AgentRegistrationPlan,
        priorState: AgentRegistrationPriorState,
        result: AgentRegistrationResult
    ) -> AgentRegistrationReceipt {
        AgentRegistrationReceipt(
            schemaVersion: AgentRegistrationReceipt.schemaVersion,
            agentKind: installation.kind,
            agentVersion: installation.version,
            serverName: AgentRegistrationPlan.serverName,
            clientID: plan.clientID,
            resolvedHelperPath: plan.helperURL.standardizedFileURL.path,
            managedRootPath: plan.managedRootURL.standardizedFileURL.path,
            argumentDigest: Self.argumentDigest(kind: installation.kind, plan: plan),
            priorState: priorState,
            result: result,
            registeredAt: now()
        )
    }

    private func receiptMatches(
        _ receipt: AgentRegistrationReceipt,
        installation: AgentInstallation,
        plan: AgentRegistrationPlan
    ) -> Bool {
        receipt.schemaVersion == AgentRegistrationReceipt.schemaVersion &&
            receipt.agentKind == installation.kind &&
            receipt.serverName == AgentRegistrationPlan.serverName &&
            receipt.clientID == plan.clientID &&
            receipt.resolvedHelperPath == plan.helperURL.standardizedFileURL.path &&
            receipt.managedRootPath == plan.managedRootURL.standardizedFileURL.path &&
            receipt.argumentDigest == Self.argumentDigest(kind: installation.kind, plan: plan)
    }

    private func addArguments(kind: AgentKind, plan: AgentRegistrationPlan) -> [String] {
        switch kind {
        case .codex:
            ["mcp", "add", AgentRegistrationPlan.serverName, "--", plan.helperURL.path]
                + plan.helperArguments
        case .claudeCode:
            [
                "mcp", "add", "--transport", "stdio", "--scope", "user",
                AgentRegistrationPlan.serverName, "--", plan.helperURL.path,
            ] + plan.helperArguments
        case .claudeDesktop:
            []
        }
    }

    private func getArguments(kind: AgentKind) -> [String] {
        switch kind {
        case .codex: ["mcp", "get", AgentRegistrationPlan.serverName, "--json"]
        case .claudeCode: ["mcp", "get", AgentRegistrationPlan.serverName]
        case .claudeDesktop: []
        }
    }

    private func removeArguments(kind: AgentKind) -> [String] {
        switch kind {
        case .codex: ["mcp", "remove", AgentRegistrationPlan.serverName]
        case .claudeCode:
            ["mcp", "remove", "--scope", "user", AgentRegistrationPlan.serverName]
        case .claudeDesktop: []
        }
    }

    private static func isMissingMessage(_ value: String) -> Bool {
        let lowered = value.lowercased()
        return ["not found", "does not exist", "no mcp server named", "no server named"]
            .contains { lowered.contains($0) }
    }

    private static func codexCommand(from output: String) -> (executable: String, arguments: [String])? {
        guard let data = output.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        let transport = root["transport"] as? [String: Any]
        let command = (transport?["command"] as? String) ?? (root["command"] as? String)
        let arguments = (transport?["args"] as? [String]) ?? (root["args"] as? [String])
        guard let command, let arguments else { return nil }
        return (command, arguments)
    }

    private static func claudeOutput(
        _ output: String,
        matchesHelper helper: String,
        arguments: [String]
    ) -> Bool {
        let lines = output.split(whereSeparator: \Character.isNewline).map(String.init)
        guard let commandLine = lines.first(where: {
            $0.trimmingCharacters(in: .whitespaces).lowercased().hasPrefix("command:")
        }),
            unquoted(value(afterLabel: commandLine)) == helper,
            let argumentsLine = lines.first(where: {
                $0.trimmingCharacters(in: .whitespaces).lowercased().hasPrefix("args:")
            })
        else { return false }
        let actualArguments = value(afterLabel: argumentsLine)
        let raw = arguments.joined(separator: " ")
        let singleQuoted = arguments.map { "'\($0.replacingOccurrences(of: "'", with: "'\\''"))'" }
            .joined(separator: " ")
        let doubleQuoted = arguments.map { "\"\($0.replacingOccurrences(of: "\"", with: "\\\""))\"" }
            .joined(separator: " ")
        return [raw, singleQuoted, doubleQuoted].contains(actualArguments)
    }

    private static func value(afterLabel line: String) -> String {
        guard let separator = line.firstIndex(of: ":") else { return "" }
        return String(line[line.index(after: separator)...])
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func unquoted(_ value: String) -> String {
        guard value.count >= 2,
              let first = value.first,
              let last = value.last,
              (first == "\"" && last == "\"") || (first == "'" && last == "'")
        else { return value }
        return String(value.dropFirst().dropLast())
    }

    private static func argumentDigest(kind: AgentKind, plan: AgentRegistrationPlan) -> String {
        let fields = [kind.rawValue, plan.helperURL.standardizedFileURL.path]
            + plan.helperArguments
        let digest = SHA256.hash(data: Data(fields.joined(separator: "\u{0}").utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }
}
