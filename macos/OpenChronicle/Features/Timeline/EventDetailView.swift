import SwiftUI

struct EventDetailView: View {
    private enum LoadState {
        case loading
        case loaded(TimelineEventDetailSnapshot)
        case failed(String)
    }

    @ObservedObject var model: TimelineViewModel
    let eventID: String
    @State private var state: LoadState = .loading

    init(model: TimelineViewModel, eventID: String) {
        self.model = model
        self.eventID = eventID
    }

    var body: some View {
        ScrollView {
            Group {
                switch state {
                case .loading:
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Loading exact event evidence…")
                    }
                    .frame(maxWidth: .infinity, minHeight: 260)
                case let .loaded(snapshot):
                    content(snapshot)
                case let .failed(message):
                    RetryUnavailableView(
                        title: "Event evidence unavailable",
                        symbol: "exclamationmark.triangle",
                        detail: message,
                        actionTitle: "Retry event evidence",
                        accessibilityHint: "Refreshes the frozen snapshot if needed and retries this event."
                    ) { Task { await load() } }
                }
            }
            .padding(24)
            .frame(maxWidth: 900, alignment: .leading)
        }
        .navigationTitle("Observation event")
        .task(id: eventID) { await load() }
    }

    @ViewBuilder
    private func content(_ snapshot: TimelineEventDetailSnapshot) -> some View {
        let event = snapshot.event
        VStack(alignment: .leading, spacing: 20) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(event.observedAt.formatted(date: .abbreviated, time: .standard))
                        .font(.title2.weight(.semibold))
                    Text(event.kind.replacingOccurrences(of: "-", with: " ").capitalized)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Label("Factual evidence", systemImage: "checkmark.seal")
                    .foregroundStyle(.blue)
            }
            if let context = event.context { contextSection(context) }
            if let text = event.ocrText {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("OCR evidence").font(.headline)
                        Spacer()
                        Label("Untrusted evidence", systemImage: "exclamationmark.shield")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Text(verbatim: text)
                        .textSelection(.enabled)
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 10))
                }
            }
            if let image = event.image {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Local screenshot evidence").font(.headline)
                    EvidenceImageView(model: model, metadata: image)
                }
            }
            rawPayload(event)
            provenance(snapshot)
        }
    }

    private func contextSection(_ context: TimelineWindowContext) -> some View {
        Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 8) {
            row("Application", context.processName)
            row("Bundle ID", context.applicationBundleID)
            row("Window", context.windowTitle ?? "Not retained")
            if let domain = context.authorizedDomain {
                row("Authorized domain", domain.domain)
                row("Domain adapter", domain.adapter)
            }
        }
        .textSelection(.enabled)
        .padding(14)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
    }

    private func rawPayload(_ event: TimelineEvent) -> some View {
        DisclosureGroup("Complete factual event payload") {
            Text(verbatim: prettyPayload(event.payload))
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 8)
                .accessibilityLabel("Complete factual event payload")
        }
    }

    private func provenance(_ snapshot: TimelineEventDetailSnapshot) -> some View {
        DisclosureGroup("Event and query provenance") {
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 8) {
                row("Event ID", snapshot.event.eventID)
                row("Device ID", snapshot.event.deviceID)
                row("Recorded", snapshot.event.recordedAt.formatted())
                row("Display timezone", snapshot.event.displayTimezone)
                row("Source", "\(snapshot.event.source.adapter) \(snapshot.event.source.version)")
                row("Frozen cutoff", snapshot.stableCutoff.formatted())
                row("Query engine", snapshot.provenance.queryEngineVersion)
            }
            .textSelection(.enabled)
            .padding(.top, 8)
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value)
        }
    }

    private func prettyPayload(_ payload: TimelineTaggedPayload) -> String {
        guard let data = try? JSONEncoder().encode(payload),
              let object = try? JSONSerialization.jsonObject(with: data),
              let pretty = try? JSONSerialization.data(
                  withJSONObject: object,
                  options: [.prettyPrinted, .sortedKeys]
              )
        else { return "Payload could not be rendered." }
        return String(data: pretty, encoding: .utf8) ?? "Payload could not be rendered."
    }

    private func load() async {
        state = .loading
        do {
            state = .loaded(try await model.eventDetail(eventID: eventID))
        } catch {
            state = .failed(error.localizedDescription)
        }
    }
}
