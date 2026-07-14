import SwiftUI

struct HealthView: View {
    @ObservedObject var viewModel: HealthViewModel

    var body: some View {
        Group {
            if let snapshot = viewModel.snapshot {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Label(healthTitle(snapshot), systemImage: healthSymbol(snapshot))
                            .font(.headline)
                        Spacer()
                        Button("Refresh") {
                            Task { await viewModel.refresh() }
                        }
                        .disabled(viewModel.isRefreshing)
                    }
                    Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 8) {
                        row("Projection", snapshot.projection.rawValue.capitalized)
                        row("Last durable event", latestEvent(snapshot))
                        row("Available storage", format(bytes: snapshot.storage.availableBytes))
                        row("Managed screenshots", format(bytes: snapshot.storage.managedBytes))
                        row("Screenshot records", "\(snapshot.screenshotRetention.retained) retained")
                        row("Mode", studyDescription(snapshot.study))
                        row("MCP grants", "\(snapshot.mcp.activeGrants) active")
                    }
                    if let error = viewModel.lastError {
                        Label("Latest refresh failed: \(error)", systemImage: "arrow.clockwise.circle")
                            .foregroundStyle(.orange)
                            .accessibilityLabel("Health refresh failed. \(error)")
                    }
                }
                .padding(16)
                .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 12))
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Open Chronicle operational health")
            } else if let error = viewModel.lastError {
                ContentUnavailableView(
                    "Health unavailable",
                    systemImage: "exclamationmark.triangle",
                    description: Text(error)
                )
            } else {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Loading operational health…")
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private func row(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label)
                .foregroundStyle(.secondary)
            Text(value)
                .textSelection(.enabled)
        }
    }

    private func healthTitle(_ snapshot: DiagnosticHealthSnapshot) -> String {
        if snapshot.issues.contains(where: { $0.severity == .critical }) {
            return "Action required"
        }
        if !snapshot.issues.isEmpty
            || HealthViewModel.storageState(for: snapshot.storage) == .warning
        {
            return "Needs attention"
        }
        return "Operational health"
    }

    private func healthSymbol(_ snapshot: DiagnosticHealthSnapshot) -> String {
        switch HealthViewModel.storageState(for: snapshot.storage) {
        case .blocked: "exclamationmark.triangle.fill"
        case .warning: "exclamationmark.circle"
        case .healthy: snapshot.issues.isEmpty ? "checkmark.circle.fill" : "info.circle"
        }
    }

    private func latestEvent(_ snapshot: DiagnosticHealthSnapshot) -> String {
        snapshot.latest.lastJournalAt ?? "None yet"
    }

    private func studyDescription(_ study: DiagnosticStudySummary) -> String {
        switch study.state {
        case .personal: "Personal · always on when enabled"
        case .scheduled: "Study scheduled"
        case .active: "Study active"
        case .expired: "Study ended"
        }
    }

    private func format(bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }
}
