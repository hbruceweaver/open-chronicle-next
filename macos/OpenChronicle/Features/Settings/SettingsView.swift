import SwiftUI

enum SettingsPane: String, CaseIterable, Identifiable {
    case recording
    case privacy
    case integrations
    case about

    var id: String { rawValue }

    var title: String {
        switch self {
        case .recording: "Recording"
        case .privacy: "Privacy"
        case .integrations: "AI integrations"
        case .about: "About & diagnostics"
        }
    }

    var symbol: String {
        switch self {
        case .recording: "record.circle"
        case .privacy: "hand.raised"
        case .integrations: "puzzlepiece.extension"
        case .about: "info.circle"
        }
    }
}

struct SettingsView: View {
    @ObservedObject var model: SettingsViewModel
    let onOpenHealth: () -> Void
    @State private var pane: SettingsPane = .recording
    @FocusState private var panePickerFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Picker("Settings section", selection: $pane) {
                ForEach(SettingsPane.allCases) { item in
                    Label(item.title, systemImage: item.symbol).tag(item)
                }
            }
            .pickerStyle(.segmented)
            .focused($panePickerFocused)
            .accessibilityLabel("Settings section")

            statusMessages
            Divider()
            content
        }
        .padding(24)
        .frame(maxWidth: 920, alignment: .leading)
        .navigationTitle("Settings")
        .task {
            if model.snapshot == nil { await model.load() }
            panePickerFocused = true
        }
    }

    @ViewBuilder
    private var statusMessages: some View {
        if let error = model.lastError {
            Label(error, systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
                .accessibilityLabel("Settings error. \(error)")
        } else if let approval = model.launchApprovalNotice {
            Label(approval, systemImage: "gear.badge")
                .foregroundStyle(.orange)
                .accessibilityLabel("Launch at login approval required. \(approval)")
        } else if let notice = model.notice {
            Label(notice, systemImage: "checkmark.circle")
                .foregroundStyle(.green)
                .accessibilityLabel("Settings updated. \(notice)")
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .detached, .loading:
            HStack(spacing: 10) {
                ProgressView()
                Text("Loading authoritative local settings…")
            }
            .frame(maxWidth: .infinity, minHeight: 300)
        case let .failed(message):
            RetryUnavailableView(
                title: "Settings unavailable",
                symbol: "exclamationmark.triangle",
                detail: message,
                actionTitle: "Retry settings",
                accessibilityHint: "Reloads settings from the local Chronicle core."
            ) { Task { await model.load() } }
        case .loaded:
            ScrollView {
                switch pane {
                case .recording:
                    RecordingSettingsView(model: model)
                case .privacy:
                    PrivacySettingsView(model: model)
                case .integrations:
                    IntegrationSettingsView(model: model.integrations)
                case .about:
                    AboutDiagnosticsSettingsView(
                        model: model,
                        onOpenHealth: onOpenHealth
                    )
                }
            }
        }
    }
}

private struct RecordingSettingsView: View {
    @ObservedObject var model: SettingsViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            recordingControl
            observationMode
            cadenceAndRetention
            launchAtLogin
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var recordingControl: some View {
        SettingsCard(title: "Observation", symbol: "record.circle") {
            if let snapshot = model.snapshot {
                Toggle(
                    "Record privacy-approved foreground work",
                    isOn: Binding(
                        get: { snapshot.recordingEnabled },
                        set: { enabled in
                            Task { await model.setRecordingEnabled(enabled) }
                        }
                    )
                )
                .disabled(model.isSaving)
                Text("Pause is immediate. Existing evidence remains readable while recording is paused.")
                    .settingsExplanation()
            }
        }
    }

    private var observationMode: some View {
        SettingsCard(title: "Personal or bounded study", symbol: "calendar.badge.clock") {
            Picker("Observation mode", selection: $model.selectedMode) {
                Text("Personal — continues until paused").tag(SettingsObservationMode.personal)
                Text("Study — stops at an exact boundary").tag(SettingsObservationMode.study)
            }
            .pickerStyle(.radioGroup)
            if model.selectedMode == .study {
                DatePicker("Study starts", selection: $model.studyStart)
                DatePicker("Study ends", selection: $model.studyEnd)
                Text("A study never silently becomes personal mode. Continuing after expiry requires a new explicit boundary.")
                    .settingsExplanation()
            }
            if let snapshot = model.snapshot, snapshot.mode == .study {
                Label(studyStatus(snapshot), systemImage: "clock")
                    .font(.callout)
            }
            Button("Save observation mode") {
                Task { await model.saveMode() }
            }
            .disabled(model.isSaving)
            .keyboardShortcut("s", modifiers: [.command, .shift])
        }
    }

    private var cadenceAndRetention: some View {
        SettingsCard(title: "Capture schedule and local images", symbol: "timer") {
            Picker("Observation cadence", selection: $model.selectedCadenceSeconds) {
                Text("Every 30 seconds").tag(UInt32(30))
                Text("Every 60 seconds").tag(UInt32(60))
            }
            .pickerStyle(.radioGroup)
            Button("Save cadence") { Task { await model.saveCadence() } }
                .disabled(model.isSaving)

            Divider()
            Picker("Screenshot retention", selection: $model.selectedRetentionSeconds) {
                Text("1 hour").tag(UInt32(3_600))
                Text("24 hours").tag(UInt32(86_400))
                Text("7 days").tag(UInt32(604_800))
                Text("30 days").tag(UInt32(2_592_000))
            }
            Button("Save screenshot retention") {
                Task { await model.saveRetention() }
            }
            .disabled(model.isSaving)
            Text("Cadence and retention are stored in the Chronicle core. This build applies them to the capture scheduler after the app restarts; the saved values shown here are authoritative.")
                .settingsExplanation()
        }
    }

    private var launchAtLogin: some View {
        SettingsCard(title: "Launch at login", symbol: "power") {
            if let state = model.snapshot?.launchAtLoginState {
                Toggle(
                    "Open Chronicle after I sign in",
                    isOn: Binding(
                        get: { state == .enabled },
                        set: { enabled in Task { await model.setLaunchAtLogin(enabled) } }
                    )
                )
                .disabled(model.isSaving)
                Text(launchDescription(state)).settingsExplanation()
                if state == .requiresApproval {
                    Button("Open Login Items settings") {
                        model.openLaunchAtLoginApproval()
                    }
                }
            }
        }
    }

    private func studyStatus(_ snapshot: SettingsRuntimeSnapshot) -> String {
        switch snapshot.studyState {
        case .personal: "Personal mode is active"
        case .scheduled: "Study is scheduled"
        case .active: "Study is active"
        case .expired: "Study ended and observation is paused"
        }
    }

    private func launchDescription(_ state: LaunchAtLoginState) -> String {
        switch state {
        case .enabled:
            "Enabled. This launches the app at login; it is not crash supervision."
        case .notRegistered:
            "Disabled. Observation starts again only when you open Chronicle."
        case .requiresApproval:
            "macOS requires approval in Login Items before this can run automatically."
        case .notFound:
            "Launch at login is unavailable for this app installation."
        }
    }
}

private struct AboutDiagnosticsSettingsView: View {
    @ObservedObject var model: SettingsViewModel
    let onOpenHealth: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SettingsCard(title: "Open Chronicle", symbol: "info.circle") {
                LabeledContent("Version", value: version)
                Text("Local-first factual work evidence for personal and consultant-assisted analysis. Chronicle does not score productivity or infer workflows in the MVP.")
                    .settingsExplanation()
            }
            if let diagnostics = model.snapshot?.diagnostics {
                SettingsCard(title: "Operational diagnostics", symbol: "stethoscope") {
                    LabeledContent("Projection", value: diagnostics.projection.rawValue.capitalized)
                    LabeledContent("Durability", value: diagnostics.acknowledgement.rawValue)
                    LabeledContent(
                        "Managed data",
                        value: ByteCountFormatter.string(
                            fromByteCount: Int64(clamping: diagnostics.managedBytes),
                            countStyle: .file
                        )
                    )
                    LabeledContent(
                        "Available storage",
                        value: ByteCountFormatter.string(
                            fromByteCount: Int64(clamping: diagnostics.availableBytes),
                            countStyle: .file
                        )
                    )
                    LabeledContent("Active MCP grants", value: "\(diagnostics.activeGrantCount)")
                    LabeledContent("Last durable journal write", value: diagnostics.latestJournalAt ?? "None yet")
                    Button("Open detailed health") { onOpenHealth() }
                }
            }
            SettingsCard(title: "Privacy boundary", symbol: "lock.shield") {
                Text("Screenshots and OCR stay in Chronicle's local managed storage. MCP never returns screenshot bytes or arbitrary local paths. A compromised process in the same macOS account remains inside the MVP trust boundary.")
                    .settingsExplanation()
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            ?? "Development build"
    }
}

struct SettingsCard<Content: View>: View {
    let title: String
    let symbol: String
    let content: Content

    init(
        title: String,
        symbol: String,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.symbol = symbol
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(title, systemImage: symbol).font(.headline)
            content
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .contain)
    }
}

private extension View {
    func settingsExplanation() -> some View {
        font(.callout).foregroundStyle(.secondary)
    }
}
