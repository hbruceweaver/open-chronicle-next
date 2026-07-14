import AppKit
import SwiftUI

struct IntegrationSettingsView: View {
    @ObservedObject var model: IntegrationSettingsModel
    @State private var grantEditor: GrantEditorRequest?
    @State private var confirmation: IntegrationConfirmation?

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SettingsCard(title: "Grant-scoped AI access", symbol: "lock.shield") {
                Text("Finding an installed client does not grant it access. Each connection gets a separate, expiring disclosure grant. OCR is off by default, and MCP never returns screenshots or Chronicle's managed paths.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Button(model.isScanning ? "Scanning…" : "Scan for supported clients") {
                    Task { await model.scan() }
                }
                .disabled(model.isScanning)
                .keyboardShortcut("r", modifiers: [.command, .shift])
            }

            if let error = model.lastError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
                    .accessibilityLabel("Integration error. \(error)")
            } else if let notice = model.notice {
                Label(notice, systemImage: "checkmark.circle")
                    .foregroundStyle(.green)
            }

            ForEach(model.rows) { row in
                integrationCard(row)
            }

            if model.rows.isEmpty, !model.isScanning {
                ContentUnavailableView(
                    "No scan results",
                    systemImage: "puzzlepiece.extension",
                    description: Text("Scan to find supported Codex and Claude clients on this Mac.")
                )
                .frame(maxWidth: .infinity, minHeight: 180)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .task {
            if model.rows.isEmpty { await model.scan() }
        }
        .sheet(item: $grantEditor) { request in
            DisclosureGrantView(
                clientName: request.kind.displayName,
                title: request.title,
                saveTitle: request.saveTitle,
                draft: request.draft
            ) { draft in
                perform(request.action, kind: request.kind, draft: draft)
            }
        }
        .alert(item: $confirmation) { value in
            Alert(
                title: Text(value.title),
                message: Text(value.message),
                primaryButton: .destructive(Text(value.buttonTitle)) {
                    Task {
                        switch value.action {
                        case .unregister: await model.unregister(kind: value.kind)
                        case .revoke: await model.revoke(kind: value.kind)
                        }
                    }
                },
                secondaryButton: .cancel()
            )
        }
    }

    private func integrationCard(_ row: SettingsIntegrationSnapshot) -> some View {
        SettingsCard(title: row.kind.displayName, symbol: symbol(for: row.kind)) {
            HStack(alignment: .firstTextBaseline) {
                Label(
                    row.detected ? "Detected" : "Not detected",
                    systemImage: row.detected ? "checkmark.circle" : "minus.circle"
                )
                if let version = row.version {
                    Text("Version \(version)").foregroundStyle(.secondary)
                }
                Spacer()
                Text(row.receiptStatus.label)
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.quaternary, in: Capsule())
            }

            if row.hasDuplicateInstallations {
                Label(
                    "Multiple installations were found. Review the chosen client before connecting.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.callout)
                .foregroundStyle(.orange)
            }
            if row.detected, !row.supported {
                Text("This installed version does not expose the required MCP setup commands.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            if let scope = row.cachedGrantScope {
                cachedScopeSummary(scope, kind: row.kind)
            }

            HStack {
                if row.canPrepareDesktopPackage {
                    Button("Create Extension Package…") {
                        grantEditor = GrantEditorRequest(
                            kind: row.kind,
                            action: .desktopPackage,
                            draft: SettingsGrantDraft()
                        )
                    }
                } else if row.canConnect {
                    Button("Connect…") {
                        grantEditor = GrantEditorRequest(
                            kind: row.kind,
                            action: .connect,
                            draft: SettingsGrantDraft()
                        )
                    }
                }
                if row.canRepair {
                    Button("Repair Exact Registration") {
                        Task { await model.repair(kind: row.kind) }
                    }
                }
                if row.canEditAccess {
                    Button("Edit Access…") {
                        grantEditor = GrantEditorRequest(
                            kind: row.kind,
                            action: row.kind == .claudeDesktop ? .desktopPackage : .replace,
                            draft: SettingsGrantDraft(cachedScope: row.cachedGrantScope)
                        )
                    }
                }
                Spacer()
                if row.canUnregister {
                    Button("Disconnect…", role: .destructive) {
                        confirmation = .unregister(row.kind)
                    }
                } else if row.canRevoke {
                    Button("Revoke Access…", role: .destructive) {
                        confirmation = .revoke(row.kind)
                    }
                }
            }
            .disabled(model.activeKinds.contains(row.kind))

            if row.kind == .claudeDesktop, model.packageReceipt != nil {
                Divider()
                Label("Manual installation required", systemImage: "arrow.right.circle")
                    .font(.subheadline.weight(.semibold))
                Text("In Claude Desktop, open Settings → Extensions → Advanced → Install Extension, then select the .mcpb file you saved.")
                    .font(.callout)
                Text(model.packageReceipt?.scopeDescription
                    ?? "The package is bound to this Mac and one revocable Open Chronicle disclosure grant.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func cachedScopeSummary(
        _ scope: SettingsCachedGrantScope,
        kind: AgentKind
    ) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(SettingsIntegrationCopy.cachedScopeExplanation)
                .font(.caption)
                .foregroundStyle(.secondary)
            LabeledContent(
                "Provisioned history window",
                value: historyWindow(scope.rollingHorizonSeconds)
            )
            LabeledContent(
                "Provisioned OCR scope",
                value: scope.allowsOCR ? "Included" : "Not included"
            )
            if let expiresAt = scope.expiresAt {
                LabeledContent(
                    "Provisioned expiry",
                    value: expiresAt.formatted(date: .abbreviated, time: .shortened)
                )
            }
            if kind == .claudeDesktop {
                Text("To change this provisioned scope or create another Claude Desktop package, revoke the existing access first. Chronicle never rotates a working desktop grant implicitly.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .font(.callout)
    }

    private func perform(
        _ action: GrantEditorAction,
        kind: AgentKind,
        draft: SettingsGrantDraft
    ) {
        switch action {
        case .connect:
            Task { await model.connect(kind: kind, draft: draft) }
        case .replace:
            Task { await model.replaceGrant(kind: kind, draft: draft) }
        case .desktopPackage:
            guard let destination = choosePackageDestination() else { return }
            Task { await model.createClaudeDesktopPackage(at: destination, draft: draft) }
        }
    }

    private func choosePackageDestination() -> URL? {
        let panel = NSSavePanel()
        panel.title = "Save Claude Desktop Extension"
        panel.nameFieldStringValue = "Open-Chronicle.mcpb"
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        return panel.runModal() == .OK ? panel.url : nil
    }

    private func symbol(for kind: AgentKind) -> String {
        switch kind {
        case .codex: "terminal"
        case .claudeCode: "chevron.left.forwardslash.chevron.right"
        case .claudeDesktop: "macwindow"
        }
    }

    private func historyWindow(_ seconds: UInt64) -> String {
        switch seconds {
        case 3_600: "Last hour"
        case 86_400: "Last 24 hours"
        case 604_800: "Last 7 days"
        default: "\(seconds / 3_600) hours"
        }
    }
}

private enum GrantEditorAction {
    case connect
    case replace
    case desktopPackage
}

private struct GrantEditorRequest: Identifiable {
    let id = UUID()
    let kind: AgentKind
    let action: GrantEditorAction
    let draft: SettingsGrantDraft

    var title: String {
        switch action {
        case .connect: "Connect \(kind.displayName)"
        case .replace: "Replace \(kind.displayName) grant"
        case .desktopPackage: "Create Claude Desktop extension"
        }
    }

    var saveTitle: String {
        switch action {
        case .connect: "Connect"
        case .replace: "Replace Grant"
        case .desktopPackage: "Choose Save Location…"
        }
    }
}

private enum IntegrationConfirmationAction {
    case unregister
    case revoke
}

private struct IntegrationConfirmation: Identifiable {
    let id = UUID()
    let kind: AgentKind
    let action: IntegrationConfirmationAction
    let title: String
    let message: String
    let buttonTitle: String

    static func unregister(_ kind: AgentKind) -> IntegrationConfirmation {
        IntegrationConfirmation(
            kind: kind,
            action: .unregister,
            title: "Disconnect \(kind.displayName)?",
            message: "Chronicle will remove only the exact registration covered by its receipt, then revoke that client's grant. A different or changed registration will be left untouched.",
            buttonTitle: "Disconnect and Revoke"
        )
    }

    static func revoke(_ kind: AgentKind) -> IntegrationConfirmation {
        IntegrationConfirmation(
            kind: kind,
            action: .revoke,
            title: "Revoke \(kind.displayName) access?",
            message: "The disclosure grant stops working immediately. Chronicle will not remove an external entry unless an exact matching receipt is available.",
            buttonTitle: "Revoke Access"
        )
    }
}
