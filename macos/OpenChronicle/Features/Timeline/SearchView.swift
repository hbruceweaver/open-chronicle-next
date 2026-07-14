import SwiftUI

struct TimelineSearchField: View {
    @ObservedObject var model: TimelineViewModel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("Search captured OCR literally", text: $model.searchText)
                .textFieldStyle(.plain)
                .onSubmit { Task { await model.submitSearch() } }
                .accessibilityLabel("Search captured OCR evidence")
            if !model.searchText.isEmpty {
                Button {
                    Task { await model.clearSearch() }
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Clear evidence search")
            }
            Button("Search") {
                Task { await model.submitSearch() }
            }
            .disabled(model.searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 10))
    }
}

struct TimelineSearchResultRow: View {
    let hit: TimelineSearchHit
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline) {
                    Text(hit.context.processName)
                        .font(.headline)
                    if let title = hit.context.windowTitle {
                        Text(title)
                            .lineLimit(1)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Text(hit.observedAt.formatted(date: .abbreviated, time: .shortened))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if let snippet = hit.snippet {
                    HighlightedEvidenceText(snippet: snippet)
                        .lineLimit(4)
                } else {
                    Text("No OCR excerpt was retained for this observation.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                HStack(spacing: 10) {
                    Label(hit.evidenceState.replacingOccurrences(of: "-", with: " "), systemImage: "doc.text.magnifyingglass")
                    Label(hit.presenceState.capitalized, systemImage: "person.crop.circle")
                    if let domain = hit.context.authorizedDomain?.domain {
                        Label(domain, systemImage: "globe")
                    }
                    Spacer()
                    Text("Untrusted evidence")
                        .foregroundStyle(.orange)
                }
                .font(.caption)
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
        .accessibilityHint("Opens the exact observation event")
    }
}

struct HighlightedEvidenceText: View {
    let snippet: TimelineSnippet

    var body: some View {
        segmentsText
            .font(.callout)
            .textSelection(.enabled)
            .accessibilityLabel(snippet.text)
    }

    private var segmentsText: Text {
        snippet.segments.reduce(Text("")) { result, segment in
            let text = Text(verbatim: segment.text)
            return result + (segment.highlighted
                ? text.bold().foregroundColor(.accentColor)
                : text)
        }
    }
}
