import SwiftUI

struct TimelineBand: View {
    let chunk: TimelineChunkBandSnapshot
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .firstTextBaseline) {
                    Text(chunk.start.formatted(date: .omitted, time: .shortened))
                        .font(.headline.monospacedDigit())
                    Text("to \(chunk.end.formatted(date: .omitted, time: .shortened))")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    if chunk.lateInput {
                        Label("Revised", systemImage: "clock.arrow.circlepath")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Spacer()
                    Image(systemName: "chevron.right")
                        .foregroundStyle(.tertiary)
                }
                TimelineCoverageBar(evidence: chunk.evidenceSeconds)
                    .frame(height: 12)
                HStack(spacing: 8) {
                    ForEach(primaryLabels, id: \.self) { label in
                        Text(label)
                            .font(.caption)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(.quaternary, in: Capsule())
                    }
                    if chunk.presenceSeconds.idle > 0 {
                        Label(
                            "\(TimelineFormat.duration(chunk.presenceSeconds.idle)) idle",
                            systemImage: "moon"
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                }
                if !gapLabels.isEmpty {
                    Text(gapLabels.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(.separator.opacity(0.35), lineWidth: 1)
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilitySummary)
        .accessibilityHint("Opens factual chunk evidence")
    }

    private var primaryLabels: [String] {
        chunk.durationEstimates
            .filter { $0.dimension == "application" }
            .sorted { $0.estimatedSeconds > $1.estimatedSeconds }
            .prefix(3)
            .map { "\($0.label) \(TimelineFormat.duration($0.estimatedSeconds))" }
    }

    private var gapLabels: [String] {
        var labels: [String] = []
        let evidence = chunk.evidenceSeconds
        if evidence.protected > 0 { labels.append("Protected \(TimelineFormat.duration(evidence.protected))") }
        if evidence.paused > 0 { labels.append("Paused \(TimelineFormat.duration(evidence.paused))") }
        if evidence.unavailable > 0 { labels.append("Unavailable \(TimelineFormat.duration(evidence.unavailable))") }
        if evidence.error > 0 { labels.append("Error \(TimelineFormat.duration(evidence.error))") }
        if evidence.gap > 0 { labels.append("Missing \(TimelineFormat.duration(evidence.gap))") }
        return labels
    }

    private var accessibilitySummary: String {
        let captured = TimelineFormat.duration(chunk.evidenceSeconds.captured)
        let nonCaptured = gapLabels.isEmpty ? "complete coverage" : gapLabels.joined(separator: ", ")
        return "Five-minute interval at \(chunk.start.formatted(date: .omitted, time: .shortened)), \(captured) captured, \(nonCaptured)"
    }
}

struct TimelineCoverageBar: View {
    let evidence: EvidenceSeconds

    var body: some View {
        GeometryReader { geometry in
            HStack(spacing: 1) {
                ForEach(parts, id: \.label) { part in
                    Rectangle()
                        .fill(part.color)
                        .frame(width: max(1, geometry.size.width * CGFloat(part.seconds) / 300))
                        .overlay {
                            if part.hatched {
                                Image(systemName: "line.diagonal")
                                    .resizable(resizingMode: .tile)
                                    .foregroundStyle(.primary.opacity(0.22))
                            }
                        }
                        .accessibilityLabel(part.label)
                        .accessibilityValue(TimelineFormat.duration(part.seconds))
                }
            }
            .clipShape(Capsule())
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Evidence coverage")
    }

    private var parts: [(label: String, seconds: UInt32, color: Color, hatched: Bool)] {
        [
            ("Captured", evidence.captured, .blue, false),
            ("Protected", evidence.protected, .purple, true),
            ("Paused", evidence.paused, .gray, true),
            ("Unavailable", evidence.unavailable, .yellow, true),
            ("Error", evidence.error, .red, true),
            ("Missing observation", evidence.gap, .secondary.opacity(0.35), true),
        ].filter { $0.seconds > 0 }
    }
}

enum TimelineFormat {
    static func duration(_ seconds: UInt32) -> String {
        duration(UInt64(seconds))
    }

    static func duration(_ seconds: UInt64) -> String {
        if seconds < 60 { return "\(seconds)s" }
        let hours = seconds / 3_600
        let minutes = (seconds % 3_600) / 60
        return hours > 0 ? "\(hours)h \(minutes)m" : "\(minutes)m"
    }
}
