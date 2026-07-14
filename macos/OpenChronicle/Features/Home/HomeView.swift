import SwiftUI

struct HomeView: View {
    @ObservedObject var model: HomeViewModel
    let captureStatus: CapturePresentationState
    let health: ChronicleHealthState
    let onChunk: (String) -> Void
    let onEvent: (String) -> Void
    @State private var evidenceSelection: HomeEvidenceSelection?

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 20) {
                HomeHealthBanner(captureStatus: captureStatus, health: health)
                HomeRangePicker(model: model)
                if model.newDataAvailable {
                    HStack {
                        Label("New evidence is available", systemImage: "arrow.clockwise.circle")
                        Spacer()
                        Button("Refresh") { Task { await model.load() } }
                    }
                    .padding(12)
                    .background(.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
                    .accessibilityElement(children: .combine)
                }
                content
            }
            .padding(24)
            .frame(maxWidth: 1_100, alignment: .leading)
        }
        .navigationTitle("Home")
        .task {
            if model.snapshot == nil { await model.load() }
        }
        .onReceive(
            NotificationCenter.default.publisher(for: NSNotification.Name.NSSystemTimeZoneDidChange)
        ) { _ in
            Task { await model.systemTimeZoneDidChange(to: .current) }
        }
        .sheet(item: $evidenceSelection) { selection in
            HomeEvidenceSheet(
                selection: selection,
                displayTimeZone: model.displayTimeZone,
                onChunk: onChunk,
                onEvent: onEvent
            )
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .detached, .loading:
            HStack(spacing: 10) {
                ProgressView()
                Text("Loading factual activity…")
            }
            .frame(maxWidth: .infinity, minHeight: 180)
        case .waitingForFirstBucket:
            HomeEmptyState(
                title: "The first five-minute interval is still forming",
                detail: "Chronicle reports complete UTC-aligned intervals so totals do not shift underneath you."
            )
        case let .empty(snapshot):
            metricGrid
            HomeEmptyState(
                title: "No captured activity in this range",
                detail: emptyDetail(snapshot)
            )
        case let .loaded(snapshot, partialCoverage):
            if partialCoverage {
                Label(
                    "This range has partial evidence. Gaps and protected or unavailable intervals remain visible below.",
                    systemImage: "circle.lefthalf.filled"
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }
            metricGrid
            HomeActivityBands(
                snapshot: snapshot,
                displayTimeZone: model.displayTimeZone,
                onChunk: onChunk
            )
            breakdowns
            transitions(snapshot)
            recentChunks
        case let .permissionBlocked(snapshot):
            if snapshot != nil { metricGrid }
            HomeEmptyState(
                title: "Screen Recording permission is unavailable",
                detail: "Existing evidence remains readable. Restore permission in System Settings to resume new observations."
            )
        case let .failed(message):
            HomeEmptyState(
                title: "The report could not be loaded",
                detail: message,
                actionTitle: "Retry",
                action: { Task { await model.load() } }
            )
        }
    }

    private var metricGrid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 180), spacing: 12)], spacing: 12) {
            if let metrics = model.metrics {
                HomeMetricCard(
                    title: "Observed computer time",
                    value: formatDuration(metrics.observedComputerSeconds),
                    detail: "Application-attributed captured evidence",
                    action: {
                        showReportEvidence("Observed computer time", .observedComputerTime)
                    }
                )
                HomeMetricCard(
                    title: "Evidence accounted",
                    value: formatPercent(metrics.coverageFraction),
                    detail: "\(formatDuration(metrics.accountedSeconds)) of \(formatDuration(metrics.expectedSeconds))",
                    action: { showReportEvidence("Evidence accounted", .evidenceAccounted) }
                )
                HomeMetricCard(
                    title: "Captured evidence",
                    value: formatPercent(metrics.captureFraction),
                    detail: "Idle included; gaps excluded",
                    action: { showReportEvidence("Captured evidence", .captured) }
                )
                HomeMetricCard(
                    title: "Idle in captured evidence",
                    value: formatDuration(metrics.idleSeconds),
                    detail: "Not attributed to applications",
                    action: { showReportEvidence("Idle evidence", .idle) }
                )
                HomeMetricCard(
                    title: "Transitions",
                    value: metrics.transitionCount.formatted(),
                    detail: "Observed application changes",
                    action: { showReportEvidence("Transitions", .transitions) }
                )
                HomeMetricCard(
                    title: "Protected",
                    value: formatDuration(metrics.protectedSeconds),
                    detail: "Content was intentionally not retained",
                    action: { showReportEvidence("Protected evidence", .protected) }
                )
                HomeMetricCard(
                    title: "Paused",
                    value: formatDuration(metrics.pausedSeconds),
                    detail: "Observation was paused",
                    action: { showReportEvidence("Paused evidence", .paused) }
                )
                HomeMetricCard(
                    title: "Unavailable",
                    value: formatDuration(metrics.unavailableSeconds),
                    detail: "Observation could not run",
                    action: { showReportEvidence("Unavailable evidence", .unavailable) }
                )
                HomeMetricCard(
                    title: "Capture errors",
                    value: formatDuration(metrics.errorSeconds),
                    detail: "Attempts that failed",
                    action: { showReportEvidence("Capture errors", .error) }
                )
                HomeMetricCard(
                    title: "Evidence gaps",
                    value: formatDuration(metrics.gapSeconds),
                    detail: "No factual observation is available",
                    action: { showReportEvidence("Evidence gaps", .gap) }
                )
            }
        }
    }

    @ViewBuilder
    private var breakdowns: some View {
        if !model.applicationBreakdown.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Applications")
                    .font(.title2.weight(.semibold))
                ForEach(model.applicationBreakdown) { item in
                    DisclosureGroup {
                        ForEach(item.windows) { window in
                            HomeBreakdownRow(
                                total: window,
                                indent: true,
                                onEvidence: showEvidence
                            )
                        }
                    } label: {
                        HomeBreakdownRow(
                            total: item.application,
                            indent: false,
                            onEvidence: showEvidence
                        )
                    }
                }
            }
        }
        if model.snapshot?.domainContextAvailable == true, !model.domainBreakdown.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Authorized domains")
                    .font(.title2.weight(.semibold))
                ForEach(model.domainBreakdown) { total in
                    HomeBreakdownRow(
                        total: total,
                        indent: false,
                        onEvidence: showEvidence
                    )
                }
            }
        }
    }

    @ViewBuilder
    private func transitions(_ snapshot: FactualReportSnapshot) -> some View {
        if !snapshot.transitions.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Recent transitions")
                    .font(.title2.weight(.semibold))
                ForEach(snapshot.transitions.suffix(8).reversed()) { transition in
                    Button {
                        onEvent(transition.supportingEventID)
                    } label: {
                        HStack {
                            Text(transition.fromKey ?? "No prior application")
                                .foregroundStyle(.secondary)
                            Image(systemName: "arrow.right")
                            Text(transition.toKey)
                            Spacer()
                            Text(HomeReportFormatter.clockTime(
                                transition.at,
                                timeZone: model.displayTimeZone
                            ))
                                .foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("Opens supporting event \(transition.supportingEventID)")
                }
            }
        }
    }

    @ViewBuilder
    private var recentChunks: some View {
        if !model.recentBuckets.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("Recent five-minute intervals")
                    .font(.title2.weight(.semibold))
                ForEach(model.recentBuckets) { bucket in
                    Button {
                        onChunk(bucket.chunkID)
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 3) {
                                Text("\(HomeReportFormatter.clockTime(bucket.start, timeZone: model.displayTimeZone))–\(HomeReportFormatter.clockTime(bucket.end, timeZone: model.displayTimeZone))")
                                Text(bucketSummary(bucket))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if bucket.lateInput {
                                Text("Revised")
                                    .font(.caption)
                                    .padding(.horizontal, 7)
                                    .padding(.vertical, 3)
                                    .background(.orange.opacity(0.12), in: Capsule())
                            }
                            Image(systemName: "chevron.right")
                                .foregroundStyle(.tertiary)
                        }
                        .padding(10)
                        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("Opens chunk \(bucket.chunkID)")
                }
            }
        }
    }

    private func emptyDetail(_ snapshot: FactualReportSnapshot) -> String {
        let gap = snapshot.coverage.evidenceSeconds.gap
        return gap > 0
            ? "No activity was inferred. \(formatDuration(UInt64(gap))) remains explicitly unobserved."
            : "Chronicle found no application-attributed captured evidence."
    }

    private func bucketSummary(_ bucket: FactualReportActivityBucket) -> String {
        let captured = UInt64(bucket.evidenceSeconds.captured)
        let gap = UInt64(bucket.evidenceSeconds.gap)
        return "\(formatDuration(captured)) captured · \(formatDuration(gap)) gap"
    }

    private func showReportEvidence(_ title: String, _ metric: HomeMetricEvidenceKind) {
        evidenceSelection = HomeEvidenceRouteBuilder.metric(
            title: title,
            metric: metric,
            model: model
        )
    }

    private func showEvidence(
        _ title: String,
        _ chunkIDs: [String],
        _ eventIDs: [String]
    ) {
        evidenceSelection = HomeEvidenceRouteBuilder.total(
            title: title,
            chunkIDs: chunkIDs,
            eventIDs: eventIDs
        )
    }
}

private struct HomeHealthBanner: View {
    let captureStatus: CapturePresentationState
    let health: ChronicleHealthState

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: symbol)
                .font(.title2)
                .foregroundStyle(color)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(14)
        .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
    }

    private var title: String {
        switch health.status {
        case .connecting: "Chronicle is connecting"
        case .repairRequired: "Chronicle needs attention"
        case .ready:
            switch captureStatus {
            case .recording: "Observation is active"
            case .paused: "Observation is paused"
            case .protected: "Current content is protected"
            case .sleeping: "Observation is suspended while this Mac sleeps"
            case .studyExpired: "This study has ended"
            case .studyNotStarted: "This study has not started"
            case .storageBlocked, .repairRequired: "Observation needs attention"
            case .unavailable: "Observation is unavailable"
            case .setupRequired: "Setup is incomplete"
            case .starting: "Observation is starting"
            case .stopped: "Observation is stopped"
            }
        }
    }

    private var detail: String {
        switch health.status {
        case .connecting: "Reports will appear after the local core is ready."
        case let .repairRequired(message): message
        case .ready: "Reports contain factual estimates and visible coverage gaps—not productivity judgments."
        }
    }

    private var symbol: String {
        switch health.status {
        case .ready where captureStatus == .recording: "record.circle.fill"
        case .repairRequired: "exclamationmark.triangle.fill"
        default: "info.circle.fill"
        }
    }

    private var color: Color {
        switch health.status {
        case .ready where captureStatus == .recording: .green
        case .repairRequired: .orange
        default: .blue
        }
    }
}

private struct HomeRangePicker: View {
    @ObservedObject var model: HomeViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker("Report range", selection: Binding(
                get: { model.selectedRange },
                set: { model.select($0) }
            )) {
                ForEach(HomeRangePreset.allCases) { preset in
                    Text(preset.title).tag(preset)
                }
            }
            .pickerStyle(.segmented)
            if model.selectedRange == .custom {
                HStack {
                    DatePicker(
                        "From",
                        selection: Binding(
                            get: { model.customStart },
                            set: { model.setCustomRange(start: $0, end: model.customEnd) }
                        )
                    )
                    DatePicker(
                        "To",
                        selection: Binding(
                            get: { model.customEnd },
                            set: { model.setCustomRange(start: model.customStart, end: $0) }
                        )
                    )
                    Button("Apply") { model.applyCustomRange() }
                }
                .environment(\.timeZone, model.displayTimeZone)
            }
            Text("Displayed in \(model.displayTimeZoneName) (\(model.displayTimeZone.identifier)). Five-minute storage intervals remain UTC-aligned.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

private struct HomeMetricCard: View {
    let title: String
    let value: String
    let detail: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(title)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Image(systemName: "info.circle")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                Text(value)
                    .font(.title2.monospacedDigit().weight(.semibold))
                Text(detail)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .frame(maxWidth: .infinity, minHeight: 82, alignment: .leading)
            .padding(12)
            .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows every supporting chunk and event identifier")
        .accessibilityElement(children: .combine)
    }
}

private struct HomeActivityBands: View {
    let snapshot: FactualReportSnapshot
    let displayTimeZone: TimeZone
    let onChunk: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Activity over time")
                .font(.title2.weight(.semibold))
            Text("Every five-minute interval shows its complete evidence partition. Application estimates never absorb idle or missing time.")
                .font(.callout)
                .foregroundStyle(.secondary)
            HomeActivityLegend()
            ForEach(snapshot.activityBuckets.suffix(36)) { bucket in
                Button {
                    onChunk(bucket.chunkID)
                } label: {
                    HStack(spacing: 10) {
                        Text(HomeReportFormatter.clockTime(
                            bucket.start,
                            timeZone: displayTimeZone
                        ))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .frame(width: 48, alignment: .leading)
                        GeometryReader { geometry in
                            let segments = HomeActivityPartition.parts(bucket)
                            let interval = max(1, bucket.end.timeIntervalSince(bucket.start))
                            HStack(spacing: 1) {
                                ForEach(segments) { segment in
                                    Rectangle()
                                        .fill(color(segment.kind))
                                        .frame(
                                            width: max(
                                                1,
                                                geometry.size.width *
                                                    Double(segment.seconds) / interval
                                            )
                                        )
                                        .help("\(segment.label): \(segment.seconds) seconds")
                                }
                            }
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                        }
                        .frame(height: 14)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(activityLabel(bucket))
                .accessibilityHint("Opens chunk \(bucket.chunkID)")
            }
        }
    }

    private func activityLabel(_ bucket: FactualReportActivityBucket) -> String {
        let facts = HomeActivityPartition.parts(bucket)
            .map { "\($0.label), \($0.seconds) seconds" }
            .joined(separator: ", ")
        return "\(HomeReportFormatter.clockTime(bucket.start, timeZone: displayTimeZone)) interval. \(facts)."
    }

    private func color(_ kind: HomeActivityPartKind) -> Color {
        switch kind {
        case let .application(key): reportColor(key)
        case .idle: .gray
        case .capturedUnattributed: .teal.opacity(0.65)
        case .protected: .purple
        case .paused: .yellow
        case .unavailable: .orange
        case .error: .red
        case .gap: .gray.opacity(0.25)
        }
    }
}

private struct HomeActivityLegend: View {
    private let entries: [(String, Color)] = [
        ("Applications", .blue),
        ("Idle", .gray),
        ("Captured, unattributed", .teal.opacity(0.65)),
        ("Protected", .purple),
        ("Paused", .yellow),
        ("Unavailable", .orange),
        ("Error", .red),
        ("Gap", .gray.opacity(0.25)),
    ]

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 120), spacing: 8)], spacing: 6) {
            ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
                HStack(spacing: 6) {
                    RoundedRectangle(cornerRadius: 2)
                        .fill(entry.1)
                        .frame(width: 12, height: 8)
                    Text(entry.0)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                }
            }
        }
        .accessibilityElement(children: .contain)
    }
}

private struct HomeBreakdownRow: View {
    let total: FactualReportTotal
    let indent: Bool
    let onEvidence: (String, [String], [String]) -> Void

    var body: some View {
        Button {
            onEvidence(total.label, total.supportingChunkIDs, total.supportingEventIDs)
        } label: {
            HStack {
                Text(total.label)
                    .lineLimit(1)
                Spacer()
                Text(formatDuration(UInt64(total.estimatedSeconds)))
                    .font(.body.monospacedDigit())
                    .foregroundStyle(.secondary)
                Image(systemName: "chevron.right")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            .padding(.leading, indent ? 18 : 0)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(total.supportingChunkIDs.isEmpty && total.supportingEventIDs.isEmpty)
        .accessibilityHint("Shows every supporting chunk and event identifier")
    }
}

struct HomeEvidenceSelection: Identifiable {
    let id = UUID()
    let title: String
    let chunkIDs: [String]
    let eventIDs: [String]
    let intervals: [HomeEvidenceInterval]
    let note: String?
}

@MainActor
enum HomeEvidenceRouteBuilder {
    static func metric(
        title: String,
        metric: HomeMetricEvidenceKind,
        model: HomeViewModel
    ) -> HomeEvidenceSelection? {
        guard model.snapshot != nil else { return nil }
        let references = model.evidenceReferences(for: metric)
        return HomeEvidenceSelection(
            title: title,
            chunkIDs: references.chunkIDs,
            eventIDs: references.eventIDs,
            intervals: references.intervals,
            note: references.note
        )
    }

    static func total(
        title: String,
        chunkIDs: [String],
        eventIDs: [String]
    ) -> HomeEvidenceSelection {
        HomeEvidenceSelection(
            title: title,
            chunkIDs: chunkIDs,
            eventIDs: eventIDs,
            intervals: [],
            note: "These are the exact identifiers attached to this factual total."
        )
    }
}

private struct HomeEvidenceSheet: View {
    @Environment(\.dismiss) private var dismiss
    let selection: HomeEvidenceSelection
    let displayTimeZone: TimeZone
    let onChunk: (String) -> Void
    let onEvent: (String) -> Void

    var body: some View {
        NavigationStack {
            List {
                if let note = selection.note {
                    Section("How this is supported") {
                        Text(note)
                            .foregroundStyle(.secondary)
                    }
                }
                if !selection.intervals.isEmpty {
                    Section("Factual intervals") {
                        ForEach(selection.intervals) { interval in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(interval.state.capitalized)
                                Text(
                                    "\(HomeReportFormatter.clockTime(interval.start, timeZone: displayTimeZone))–\(HomeReportFormatter.clockTime(interval.end, timeZone: displayTimeZone))"
                                )
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                                if interval.supportingEventIDs.isEmpty {
                                    Text("No source event exists for this interval.")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                } else {
                                    Text("\(interval.supportingEventIDs.count) supporting event ID(s)")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
                if !selection.chunkIDs.isEmpty {
                    Section("Supporting five-minute intervals") {
                        ForEach(selection.chunkIDs, id: \.self) { chunkID in
                            Button {
                                dismiss()
                                onChunk(chunkID)
                            } label: {
                                Label(chunkID, systemImage: "clock")
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                if !selection.eventIDs.isEmpty {
                    Section("Supporting events") {
                        ForEach(selection.eventIDs, id: \.self) { eventID in
                            Button {
                                dismiss()
                                onEvent(eventID)
                            } label: {
                                Label(eventID, systemImage: "doc.text.magnifyingglass")
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                if selection.chunkIDs.isEmpty && selection.eventIDs.isEmpty &&
                    selection.intervals.isEmpty
                {
                    Text("No supporting identifiers are available for this aggregate.")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle(selection.title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .frame(minWidth: 520, minHeight: 420)
    }
}

private struct HomeEmptyState: View {
    let title: String
    let detail: String
    var actionTitle: String?
    var action: (() -> Void)?

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "clock.badge.questionmark")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text(title)
                .font(.headline)
            Text(detail)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            if let actionTitle, let action {
                Button(actionTitle, action: action)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 190)
        .padding()
        .accessibilityElement(children: .contain)
    }
}

private func formatDuration(_ seconds: UInt64) -> String {
    let hours = seconds / 3_600
    let minutes = (seconds % 3_600) / 60
    if hours > 0 { return "\(hours)h \(minutes)m" }
    if minutes > 0 { return "\(minutes)m" }
    return "\(seconds)s"
}

private func formatPercent(_ value: Double) -> String {
    value.formatted(.percent.precision(.fractionLength(0)))
}

private func reportColor(_ key: String) -> Color {
    let value = key.unicodeScalars.reduce(0) { ($0 + Int($1.value)) % 360 }
    return Color(hue: Double(value) / 360, saturation: 0.58, brightness: 0.78)
}
