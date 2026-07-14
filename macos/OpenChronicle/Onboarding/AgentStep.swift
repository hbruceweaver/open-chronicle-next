import SwiftUI

@MainActor
protocol AgentDetecting: AnyObject {
    func detect() async -> [AgentInstallation]
}

extension AgentDetectionService: AgentDetecting {}

@MainActor
final class AgentSetupModel: ObservableObject {
    typealias ConnectHandler = @MainActor (AgentInstallation) async -> AgentRegistrationOutcome

    @Published private(set) var installations: [AgentInstallation] = []
    @Published private(set) var outcomes: [AgentKind: AgentRegistrationOutcome] = [:]
    @Published private(set) var isScanning = false
    @Published private(set) var connectingKinds: Set<AgentKind> = []
    @Published private(set) var hasScanned = false

    private let detector: any AgentDetecting
    private let connectHandler: ConnectHandler

    init(
        detector: (any AgentDetecting)? = nil,
        connectHandler: @escaping ConnectHandler = { _ in .failed(.clientUnavailable) }
    ) {
        self.detector = detector ?? AgentDetectionService()
        self.connectHandler = connectHandler
    }

    func scanIfNeeded() async {
        guard !hasScanned else { return }
        await scan()
    }

    func scan() async {
        guard !isScanning else { return }
        isScanning = true
        defer {
            isScanning = false
            hasScanned = true
        }
        installations = await detector.detect()
    }

    func connect(_ installation: AgentInstallation) async {
        guard !connectingKinds.contains(installation.kind) else { return }
        connectingKinds.insert(installation.kind)
        defer { connectingKinds.remove(installation.kind) }
        outcomes[installation.kind] = await connectHandler(installation)
    }
}

struct AgentStep: View {
    @ObservedObject var model: OnboardingModel

    private var setup: AgentSetupModel { model.agentSetup }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Connect an AI client (optional)")
                .font(.largeTitle.weight(.semibold))
            Text(
                "Recording and reports work without Claude or Codex. A connected client gets a "
                    + "revocable seven-day grant to the last 24 hours of factual metadata and derived analysis. "
                    + "OCR is off by default, and screenshots never leave this Mac through MCP."
            )
            .foregroundStyle(.secondary)

            if setup.isScanning {
                ProgressView("Looking for supported clients…")
            } else if setup.hasScanned, setup.installations.isEmpty {
                Label("No supported Claude or Codex installation was found", systemImage: "info.circle")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(setup.installations) { installation in
                    installationRow(installation)
                }
            }

            Button(setup.hasScanned ? "Scan Again" : "Find Installed Clients") {
                Task { await setup.scan() }
            }
            .disabled(setup.isScanning)

            Divider()
            Toggle("Finish recording setup even if no AI client is connected", isOn: $model.draft.deferAgentSetup)
                .toggleStyle(.checkbox)
            Text("You can connect or repair clients later in Settings. Open Chronicle never edits AGENTS.md, CLAUDE.md, or global instruction files.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .task { await setup.scanIfNeeded() }
    }

    @ViewBuilder
    private func installationRow(_ installation: AgentInstallation) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(installation.kind.displayName).font(.headline)
                    if let version = installation.version {
                        Text(version).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button(buttonTitle(installation)) {
                    Task { await setup.connect(installation) }
                }
                .disabled(
                    setup.connectingKinds.contains(installation.kind)
                        || installation.support == .unsupported
                )
            }
            if installation.hasDuplicateExecutables {
                Label(
                    "Multiple installations were found. Open Chronicle will use \(installation.executableURL?.path ?? "the first resolved executable").",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.caption)
                .foregroundStyle(.orange)
            }
            if installation.support == .unsupported {
                Label("Installed, but this version does not expose the required MCP commands.", systemImage: "xmark.circle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            if let outcome = setup.outcomes[installation.kind] {
                outcomeView(outcome, installation: installation)
            }
        }
        .padding(14)
        .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 10))
    }

    private func buttonTitle(_ installation: AgentInstallation) -> String {
        if setup.connectingKinds.contains(installation.kind) { return "Connecting…" }
        if installation.kind == .claudeDesktop { return "Setup Steps" }
        return "Connect"
    }

    @ViewBuilder
    private func outcomeView(
        _ outcome: AgentRegistrationOutcome,
        installation: AgentInstallation
    ) -> some View {
        switch outcome {
        case .registered, .alreadyRegistered:
            Label(
                "Connected. Restart \(installation.kind.displayName), then verify Open Chronicle in its MCP view.",
                systemImage: "checkmark.circle.fill"
            )
            .foregroundStyle(.green)
        case .guidedDesktop:
            Label(
                "Claude Desktop requires a user-installed .mcpb extension: Settings → Extensions → Advanced settings → Install Extension.",
                systemImage: "arrow.right.circle"
            )
            .foregroundStyle(.blue)
        case .conflict:
            Label(
                "A different open-chronicle entry already exists. It was not changed.",
                systemImage: "exclamationmark.triangle.fill"
            )
            .foregroundStyle(.orange)
        case let .blocked(reason):
            Label(reason.explanation, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        case .unsupported:
            Label("This client does not support the required MCP commands.", systemImage: "xmark.circle")
                .foregroundStyle(.orange)
        case .failed:
            Label(
                "Setup could not be verified safely. Nothing conflicting was overwritten; retry or use Settings → Repair.",
                systemImage: "exclamationmark.triangle.fill"
            )
            .foregroundStyle(.orange)
        case .removed:
            Label("Disconnected", systemImage: "checkmark.circle")
        }
    }
}
