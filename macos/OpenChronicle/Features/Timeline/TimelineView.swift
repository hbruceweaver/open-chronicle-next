import SwiftUI

struct TimelineView: View {
    @ObservedObject var model: TimelineViewModel
    let onChunk: (String) -> Void
    let onEvent: (String) -> Void
    @State private var showFilters = false
    @State private var showProvenance = false

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    controls
                    statusBanners
                    content
                    pagination
                }
                .padding(24)
                .frame(maxWidth: 1_000, alignment: .leading)
            }
            .onChange(of: model.scrollAnchorRevisionID) { _, revisionID in
                guard let revisionID else { return }
                withAnimation { proxy.scrollTo(revisionID, anchor: .center) }
            }
        }
        .navigationTitle("Evidence timeline")
        .task {
            if model.coverage == nil { await model.load() }
        }
        .sheet(isPresented: $showProvenance) {
            TimelineProvenanceSheet(
                provenance: model.provenance,
                cutoff: model.stableCutoff
            )
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                TimelineSearchField(model: model)
                Button {
                    showFilters.toggle()
                } label: {
                    Label("Filters", systemImage: "line.3.horizontal.decrease.circle")
                }
                Button {
                    showProvenance = true
                } label: {
                    Label("Provenance", systemImage: "checkmark.seal")
                }
                .disabled(model.provenance == nil)
            }
            if showFilters {
                TimelineFilters(model: model)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
    }

    @ViewBuilder
    private var statusBanners: some View {
        if model.newDataAvailable {
            TimelineBanner(
                title: "New evidence is available",
                detail: "The rendered result remains frozen at its current snapshot until you refresh.",
                symbol: "arrow.clockwise.circle",
                actionTitle: "Refresh"
            ) {
                Task { await model.refreshNewData() }
            }
        }
        switch model.state {
        case let .rebuilding(hasPriorData) where hasPriorData:
            TimelineBanner(
                title: "The local evidence index is rebuilding",
                detail: "Showing the previous frozen result. Captured evidence remains safe.",
                symbol: "hammer",
                actionTitle: "Try again"
            ) { Task { await model.load() } }
        case let .failed(message, hasPriorData) where hasPriorData:
            TimelineBanner(
                title: "Refresh failed",
                detail: message,
                symbol: "exclamationmark.triangle",
                actionTitle: "Retry"
            ) { Task { await model.load() } }
        case .partial:
            Label(
                "This range has partial factual coverage. Protected, paused, unavailable, error, and missing intervals remain visible.",
                systemImage: "circle.lefthalf.filled"
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Partial evidence coverage")
        default:
            EmptyView()
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .detached, .loading:
            TimelineLoadingView()
        case .empty:
            ContentUnavailableView(
                "No factual chunks in this range",
                systemImage: "calendar.badge.clock",
                description: Text("Adjust the range or allow the first complete five-minute interval to form.")
            )
            .frame(maxWidth: .infinity, minHeight: 260)
        case .noMatches:
            ContentUnavailableView.search(text: model.searchText)
                .frame(maxWidth: .infinity, minHeight: 260)
        case let .rebuilding(hasPriorData):
            if hasPriorData { renderedResults } else { rebuildingView }
        case let .failed(message, hasPriorData):
            if hasPriorData {
                renderedResults
            } else {
                RetryUnavailableView(
                    title: "Timeline unavailable",
                    symbol: "exclamationmark.triangle",
                    detail: message,
                    actionTitle: "Retry timeline",
                    accessibilityHint: "Attempts to load a new frozen evidence snapshot."
                ) { Task { await model.load() } }
            }
        case .populated, .partial:
            renderedResults
        }
    }

    @ViewBuilder
    private var renderedResults: some View {
        if model.isSearching {
            ForEach(model.searchHits) { hit in
                TimelineSearchResultRow(hit: hit) { onEvent(hit.eventID) }
                    .id(hit.eventID)
            }
        } else {
            ForEach(model.chunks) { chunk in
                TimelineBand(chunk: chunk) {
                    model.rememberVisible(revisionID: chunk.revisionID)
                    onChunk(chunk.revisionID)
                }
                .id(chunk.revisionID)
                .onAppear { model.rememberVisible(revisionID: chunk.revisionID) }
            }
        }
    }

    @ViewBuilder
    private var pagination: some View {
        if model.nextPageLoading {
            HStack {
                ProgressView()
                Text("Loading the next frozen page…")
            }
            .frame(maxWidth: .infinity)
            .padding()
        } else if let error = model.nextPageError {
            TimelineBanner(
                title: "The next page could not be loaded",
                detail: error,
                symbol: "arrow.clockwise.circle",
                actionTitle: "Retry"
            ) { Task { await model.loadNextPage() } }
        } else if model.hasNextPage {
            Button("Load more from this snapshot") {
                Task { await model.loadNextPage() }
            }
            .frame(maxWidth: .infinity)
            .padding()
        }
    }

    private var rebuildingView: some View {
        RetryUnavailableView(
            title: "Preparing the evidence index",
            symbol: "hammer",
            detail: "Captured evidence remains safe. The timeline will be available when the local projection catches up.",
            actionTitle: "Check timeline again",
            accessibilityHint: "Checks whether the local evidence projection is ready."
        ) { Task { await model.load() } }
    }
}

struct RetryUnavailableView: View {
    let title: String
    let symbol: String
    let detail: String
    let actionTitle: String
    let accessibilityHint: String
    var minimumHeight: CGFloat = 260
    let action: () -> Void
    @FocusState private var actionFocused: Bool

    var body: some View {
        VStack(spacing: 14) {
            ContentUnavailableView(
                title,
                systemImage: symbol,
                description: Text(detail)
            )
            Button(action: action) {
                Label(actionTitle, systemImage: "arrow.clockwise")
            }
            .buttonStyle(.borderedProminent)
            .keyboardShortcut(.defaultAction)
            .focused($actionFocused)
            .accessibilityLabel(actionTitle)
            .accessibilityHint(accessibilityHint)
        }
        .frame(maxWidth: .infinity, minHeight: minimumHeight)
        .onAppear { actionFocused = true }
    }
}

private struct TimelineFilters: View {
    @ObservedObject var model: TimelineViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                DatePicker("From", selection: $model.rangeStart)
                DatePicker("To", selection: $model.rangeEnd)
            }
            HStack {
                TextField("Application bundle ID", text: $model.applicationBundleID)
                TextField("Window title contains", text: $model.windowText)
                TextField("Authorized domain", text: $model.authorizedDomain)
                    .disabled(!model.domainContextAvailable)
                    .help(model.domainContextAvailable
                        ? "Filters domain context supplied by an authorized adapter."
                        : "Authorized domain context is not available in this store.")
            }
            HStack {
                Menu {
                    ForEach(TimelineCoverageState.allCases) { state in
                        Toggle(state.title, isOn: Binding(
                            get: { model.selectedCoverageStates.contains(state) },
                            set: { selected in
                                if selected { model.selectedCoverageStates.insert(state) }
                                else { model.selectedCoverageStates.remove(state) }
                            }
                        ))
                    }
                } label: {
                    Label(coverageLabel, systemImage: "circle.lefthalf.filled")
                }
                Spacer()
                Button("Apply filters") {
                    Task { await model.applyFilters() }
                }
            }
        }
        .textFieldStyle(.roundedBorder)
        .padding(14)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Timeline filters")
    }

    private var coverageLabel: String {
        model.selectedCoverageStates.isEmpty
            ? "All coverage states"
            : "\(model.selectedCoverageStates.count) coverage filters"
    }
}

private struct TimelineLoadingView: View {
    var body: some View {
        HStack(spacing: 10) {
            ProgressView()
            Text("Loading a frozen evidence snapshot…")
        }
        .frame(maxWidth: .infinity, minHeight: 260)
        .accessibilityLabel("Loading evidence timeline")
    }
}

private struct TimelineBanner: View {
    let title: String
    let detail: String
    let symbol: String
    let actionTitle: String
    let action: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.headline)
                Text(detail).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            Button(actionTitle, action: action)
        }
        .padding(12)
        .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }
}

private struct TimelineProvenanceSheet: View {
    @Environment(\.dismiss) private var dismiss
    let provenance: FactualReportProvenance?
    let cutoff: Date?

    var body: some View {
        NavigationStack {
            Group {
                if let provenance {
                    Form {
                        LabeledContent("Frozen cutoff", value: cutoff?.formatted() ?? "Unknown")
                        LabeledContent("Query engine", value: provenance.queryEngineVersion)
                        LabeledContent("Projection build", value: provenance.projectionBuildID)
                        LabeledContent("SQLite", value: provenance.sqliteVersion)
                        LabeledContent("SQLite source", value: provenance.sqliteSourceID)
                        LabeledContent("Source events", value: provenance.sourceEventIDs.count.formatted())
                        LabeledContent("Chunk revisions", value: provenance.sourceChunkRevisionIDs.count.formatted())
                    }
                    .textSelection(.enabled)
                } else {
                    ContentUnavailableView("No provenance loaded", systemImage: "checkmark.seal")
                }
            }
            .padding(20)
            .frame(minWidth: 520, minHeight: 340)
            .navigationTitle("Factual provenance")
            .toolbar {
                Button("Done") { dismiss() }
            }
        }
    }
}
