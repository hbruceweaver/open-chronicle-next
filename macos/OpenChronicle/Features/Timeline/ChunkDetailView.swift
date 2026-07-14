import SwiftUI

struct ChunkDetailView: View {
    private enum LoadState {
        case loading
        case loaded(TimelineChunkDetailSnapshot)
        case failed(String)
    }

    @ObservedObject var model: TimelineViewModel
    let reference: TimelineChunkReference
    let onEvent: (String) -> Void
    @State private var state: LoadState = .loading

    init(
        model: TimelineViewModel,
        reference: TimelineChunkReference,
        onEvent: @escaping (String) -> Void
    ) {
        self.model = model
        self.reference = reference
        self.onEvent = onEvent
    }

    var body: some View {
        ScrollView {
            Group {
                switch state {
                case .loading:
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Loading exact chunk revision…")
                    }
                    .frame(maxWidth: .infinity, minHeight: 260)
                case let .loaded(snapshot):
                    content(snapshot)
                case let .failed(message):
                    RetryUnavailableView(
                        title: "Chunk evidence unavailable",
                        symbol: "exclamationmark.triangle",
                        detail: message,
                        actionTitle: "Retry chunk evidence",
                        accessibilityHint: "Refreshes the frozen snapshot if needed and retries this chunk."
                    ) { Task { await load() } }
                }
            }
            .padding(24)
            .frame(maxWidth: 900, alignment: .leading)
        }
        .navigationTitle("Chunk evidence")
        .task(id: reference) { await load() }
    }

    @ViewBuilder
    private func content(_ snapshot: TimelineChunkDetailSnapshot) -> some View {
        let chunk = snapshot.chunk
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 6) {
                Text(chunk.window.start.formatted(date: .abbreviated, time: .shortened))
                    .font(.title2.weight(.semibold))
                Text("to \(chunk.window.end.formatted(date: .abbreviated, time: .shortened)) · \(chunk.displayTimezone)")
                    .foregroundStyle(.secondary)
                if chunk.lateInput {
                    Label("This immutable revision includes late evidence", systemImage: "clock.arrow.circlepath")
                        .foregroundStyle(.orange)
                }
            }
            TimelineCoverageBar(evidence: chunk.evidenceSeconds)
                .frame(height: 14)
            coverageGrid(chunk)
            if !chunk.durationEstimates.isEmpty { durationSection(chunk) }
            if !chunk.transitions.isEmpty { transitionSection(chunk) }
            if !chunk.gaps.isEmpty { gapSection(chunk) }
            if !chunk.ocrExtracts.isEmpty { extractSection(chunk) }
            supportingEvents(chunk)
            provenance(snapshot)
        }
    }

    private func coverageGrid(_ chunk: TimelineChunkRevision) -> some View {
        Grid(alignment: .leading, horizontalSpacing: 22, verticalSpacing: 8) {
            GridRow {
                Text("Captured").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.evidenceSeconds.captured))
                Text("Active").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.presenceSeconds.active))
            }
            GridRow {
                Text("Protected").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.evidenceSeconds.protected))
                Text("Idle").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.presenceSeconds.idle))
            }
            GridRow {
                Text("Paused").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.evidenceSeconds.paused))
                Text("Unknown presence").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.presenceSeconds.unknown))
            }
            GridRow {
                Text("Unavailable / error").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(
                    UInt64(chunk.evidenceSeconds.unavailable) + UInt64(chunk.evidenceSeconds.error)
                ))
                Text("Missing").foregroundStyle(.secondary)
                Text(TimelineFormat.duration(chunk.evidenceSeconds.gap))
            }
        }
        .padding(14)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
    }

    private func durationSection(_ chunk: TimelineChunkRevision) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Factual duration estimates").font(.headline)
            ForEach(chunk.durationEstimates) { estimate in
                HStack {
                    Text(estimate.dimension.capitalized)
                        .foregroundStyle(.secondary)
                    Text(estimate.label)
                    Spacer()
                    Text(TimelineFormat.duration(estimate.estimatedSeconds))
                        .monospacedDigit()
                }
            }
        }
    }

    private func transitionSection(_ chunk: TimelineChunkRevision) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Observed transitions").font(.headline)
            ForEach(chunk.transitions) { transition in
                Button {
                    onEvent(transition.supportingEventID)
                } label: {
                    HStack {
                        Text(transition.at.formatted(date: .omitted, time: .standard))
                            .monospacedDigit()
                        Text("\(transition.fromKey ?? "Unknown") → \(transition.toKey)")
                        Spacer()
                        Image(systemName: "chevron.right")
                    }
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func gapSection(_ chunk: TimelineChunkRevision) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Non-captured intervals").font(.headline)
            ForEach(chunk.gaps) { gap in
                HStack {
                    Text(gap.kind.replacingOccurrences(of: "-", with: " ").capitalized)
                    Spacer()
                    Text("\(gap.start.formatted(date: .omitted, time: .standard))–\(gap.end.formatted(date: .omitted, time: .standard))")
                        .monospacedDigit()
                }
            }
        }
    }

    private func extractSection(_ chunk: TimelineChunkRevision) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Factual OCR excerpts").font(.headline)
                Spacer()
                Label("Untrusted evidence", systemImage: "exclamationmark.shield")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            ForEach(chunk.ocrExtracts, id: \.sourceEventID) { extract in
                VStack(alignment: .leading, spacing: 6) {
                    Text(verbatim: extract.text)
                        .font(.callout)
                        .textSelection(.enabled)
                    Button("Open source event \(extract.sourceEventID)") {
                        onEvent(extract.sourceEventID)
                    }
                    .font(.caption)
                }
                .padding(12)
                .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 10))
            }
        }
    }

    private func supportingEvents(_ chunk: TimelineChunkRevision) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Supporting events").font(.headline)
            FlowLayout(spacing: 8) {
                ForEach(chunk.supportingEventIDs, id: \.self) { eventID in
                    Button(eventID) { onEvent(eventID) }
                        .font(.caption.monospaced())
                }
            }
        }
    }

    private func provenance(_ snapshot: TimelineChunkDetailSnapshot) -> some View {
        DisclosureGroup("Revision and query provenance") {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                detailRow("Revision", snapshot.chunk.revisionID)
                detailRow("Prior revision", snapshot.chunk.priorRevisionID ?? "None")
                detailRow("Aggregator", snapshot.chunk.aggregatorVersion)
                detailRow("Input digest", snapshot.chunk.inputDigest)
                detailRow("Generated", snapshot.chunk.generatedAt.formatted())
                detailRow("Frozen cutoff", snapshot.stableCutoff.formatted())
                detailRow("Query engine", snapshot.provenance.queryEngineVersion)
                detailRow("Projection build", snapshot.provenance.projectionBuildID)
            }
            .textSelection(.enabled)
            .padding(.top, 8)
        }
    }

    private func detailRow(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary)
            Text(value)
        }
    }

    private func load() async {
        state = .loading
        do {
            state = .loaded(try await model.chunkDetail(reference: reference))
        } catch {
            state = .failed(error.localizedDescription)
        }
    }
}

/// Small wrapping layout used for opaque evidence IDs without importing a UI dependency.
private struct FlowLayout: Layout {
    let spacing: CGFloat

    func sizeThatFits(
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) -> CGSize {
        place(subviews: subviews, width: proposal.width ?? .infinity).size
    }

    func placeSubviews(
        in bounds: CGRect,
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) {
        let result = place(subviews: subviews, width: bounds.width)
        for (index, point) in result.points.enumerated() {
            subviews[index].place(
                at: CGPoint(x: bounds.minX + point.x, y: bounds.minY + point.y),
                proposal: .unspecified
            )
        }
    }

    private func place(subviews: Subviews, width: CGFloat) -> (size: CGSize, points: [CGPoint]) {
        var points: [CGPoint] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineHeight: CGFloat = 0
        var maxWidth: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += lineHeight + spacing
                lineHeight = 0
            }
            points.append(CGPoint(x: x, y: y))
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
            maxWidth = max(maxWidth, x)
        }
        return (CGSize(width: min(maxWidth, width), height: y + lineHeight), points)
    }
}
