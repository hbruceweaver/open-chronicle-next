import SwiftUI

enum DerivedAnalysisBoundary {
    static let title = "Derived analysis"
    static let explanation = "Interpretations, annotations, and hypotheses live here. They never rewrite factual evidence or coverage."
}

struct AnalysisView: View {
    @ObservedObject var model: AnalysisViewModel
    let onChunk: (String) -> Void
    let onEvent: (String) -> Void
    @State private var selected: AnalysisArtifactSnapshot?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header
                rangeControls
                content
                pagination
            }
            .padding(24)
            .frame(maxWidth: 900, alignment: .leading)
        }
        .navigationTitle("Analysis")
        .task {
            if model.snapshotToken == nil { await model.load() }
        }
        .sheet(item: $selected) { artifact in
            ArtifactDetailView(
                model: model,
                artifact: artifact,
                onChunk: onChunk,
                onEvent: onEvent
            )
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(DerivedAnalysisBoundary.title, systemImage: "sparkles.rectangle.stack")
                .font(.title2.weight(.semibold))
            Text(DerivedAnalysisBoundary.explanation)
                .foregroundStyle(.secondary)
            Label("Every item retains author, model/client, revision, and factual evidence references.", systemImage: "link")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private var rangeControls: some View {
        HStack {
            DatePicker("From", selection: $model.rangeStart)
            DatePicker("To", selection: $model.rangeEnd)
            Button("Apply range") { Task { await model.load() } }
            Button("Refresh") { Task { await model.load() } }
        }
        .padding(12)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .detached, .loading:
            HStack(spacing: 10) {
                ProgressView()
                Text("Loading a frozen derived-analysis snapshot…")
            }
            .frame(maxWidth: .infinity, minHeight: 260)
        case .empty:
            ContentUnavailableView(
                "No derived analysis in this range",
                systemImage: "text.badge.plus",
                description: Text("Connected Claude or Codex clients can create evidence-linked reports through the scoped MCP interface. Open Chronicle does not invent recommendations automatically.")
            )
            .frame(maxWidth: .infinity, minHeight: 300)
        case let .rebuilding(hasPriorData):
            if hasPriorData {
                warning("The local index is rebuilding. Showing the prior frozen analysis result.")
                artifactRows
            } else {
                RetryUnavailableView(
                    title: "Preparing the analysis index",
                    symbol: "hammer",
                    detail: "Derived artifacts remain durable while the local projection rebuilds.",
                    actionTitle: "Check analysis again",
                    accessibilityHint: "Checks whether the local analysis projection is ready."
                ) { Task { await model.load() } }
            }
        case let .failed(message, hasPriorData):
            if hasPriorData {
                warning("Refresh failed: \(message)")
                artifactRows
            } else {
                RetryUnavailableView(
                    title: "Analysis unavailable",
                    symbol: "exclamationmark.triangle",
                    detail: message,
                    actionTitle: "Retry analysis",
                    accessibilityHint: "Attempts to load a new frozen analysis snapshot."
                ) { Task { await model.load() } }
            }
        case .populated:
            artifactRows
        }
    }

    private var artifactRows: some View {
        ForEach(model.artifacts) { artifact in
            Button {
                selected = artifact
            } label: {
                AnalysisArtifactRow(artifact: artifact)
            }
            .buttonStyle(.plain)
        }
    }

    @ViewBuilder
    private var pagination: some View {
        if model.nextPageLoading {
            HStack { ProgressView(); Text("Loading more derived artifacts…") }
                .frame(maxWidth: .infinity)
        } else if let error = model.nextPageError {
            HStack {
                Label("Next page failed: \(error)", systemImage: "exclamationmark.triangle")
                Spacer()
                Button("Retry") { Task { await model.loadNextPage() } }
            }
            .padding(12)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        } else if model.hasNextPage {
            Button("Load more from this snapshot") {
                Task { await model.loadNextPage() }
            }
            .frame(maxWidth: .infinity)
        }
    }

    private func warning(_ message: String) -> some View {
        Label(message, systemImage: "exclamationmark.triangle")
            .font(.callout)
            .foregroundStyle(.orange)
            .padding(12)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct AnalysisArtifactRow: View {
    let artifact: AnalysisArtifactSnapshot

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(artifact.title).font(.headline)
                Spacer()
                Text(artifact.status.capitalized)
                    .font(.caption)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.quaternary, in: Capsule())
            }
            Text("\(artifact.artifactType.capitalized) · \(artifact.author.label) · \(artifact.createdAt.formatted(date: .abbreviated, time: .shortened))")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("\(artifact.evidence.chunkIDs.count) chunks · \(artifact.evidence.eventIDs.count) events · revision \(artifact.revisionID)")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(14)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(.separator.opacity(0.35)))
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens the exact derived revision and factual evidence references")
    }
}

struct ArtifactDetailView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: AnalysisViewModel
    let artifact: AnalysisArtifactSnapshot
    let onChunk: (String) -> Void
    let onEvent: (String) -> Void
    @State private var state: LoadState = .loading

    private enum LoadState {
        case loading
        case loaded(AnalysisDetailSnapshot)
        case failed(String)
    }

    init(
        model: AnalysisViewModel,
        artifact: AnalysisArtifactSnapshot,
        onChunk: @escaping (String) -> Void,
        onEvent: @escaping (String) -> Void
    ) {
        self.model = model
        self.artifact = artifact
        self.onChunk = onChunk
        self.onEvent = onEvent
    }

    var body: some View {
        NavigationStack {
            Group {
                switch state {
                case .loading:
                    HStack { ProgressView(); Text("Loading exact analysis revision…") }
                case let .failed(message):
                    RetryUnavailableView(
                        title: "Analysis revision unavailable",
                        symbol: "exclamationmark.triangle",
                        detail: message,
                        actionTitle: "Retry analysis revision",
                        accessibilityHint: "Refreshes the frozen snapshot if needed and retries this exact revision."
                    ) { Task { await load() } }
                case let .loaded(snapshot):
                    detail(snapshot)
                }
            }
            .frame(minWidth: 620, minHeight: 520)
            .navigationTitle(artifact.title)
            .toolbar { Button("Done") { dismiss() } }
        }
        .task(id: artifact.revisionID) { await load() }
    }

    private func detail(_ snapshot: AnalysisDetailSnapshot) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Label("Derived claim — not factual evidence", systemImage: "sparkles")
                    .font(.headline)
                    .foregroundStyle(.purple)
                Text(verbatim: snapshot.artifact.body)
                    .textSelection(.enabled)
                Divider()
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 8) {
                    row("Artifact", snapshot.artifact.artifactID)
                    row("Revision", snapshot.artifact.revisionID)
                    row("Prior revision", snapshot.artifact.priorRevisionID ?? "None")
                    row("Author", snapshot.artifact.author.label)
                    row("Client", snapshot.artifact.author.clientID ?? "Not declared")
                    row("Model", snapshot.artifact.author.model ?? "Not declared")
                    row("Status", snapshot.artifact.status)
                    row("Frozen cutoff", snapshot.stableCutoff.formatted())
                    row("Query engine", snapshot.provenance.queryEngineVersion)
                }
                .textSelection(.enabled)
                if !snapshot.artifact.evidence.chunkIDs.isEmpty {
                    evidenceSection(
                        title: "Supporting logical chunks",
                        ids: snapshot.artifact.evidence.chunkIDs,
                        action: onChunk
                    )
                }
                if !snapshot.artifact.evidence.eventIDs.isEmpty {
                    evidenceSection(
                        title: "Supporting events",
                        ids: snapshot.artifact.evidence.eventIDs,
                        action: onEvent
                    )
                }
            }
            .padding(24)
            .frame(maxWidth: 760, alignment: .leading)
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        GridRow { Text(label).foregroundStyle(.secondary); Text(value) }
    }

    private func evidenceSection(
        title: String,
        ids: [String],
        action: @escaping (String) -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            ForEach(ids, id: \.self) { id in
                Button(id) {
                    dismiss()
                    action(id)
                }
                .font(.caption.monospaced())
            }
        }
    }

    private func load() async {
        do {
            state = .loaded(try await model.detail(
                artifactID: artifact.artifactID,
                revisionID: artifact.revisionID
            ))
        } catch {
            state = .failed(error.localizedDescription)
        }
    }
}
