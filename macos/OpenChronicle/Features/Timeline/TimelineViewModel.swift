import Foundation

enum TimelinePresentationState: Equatable, Sendable {
    case detached
    case loading
    case empty
    case noMatches
    case populated
    case partial
    case rebuilding(hasPriorData: Bool)
    case failed(message: String, hasPriorData: Bool)
}

@MainActor
final class TimelineViewModel: ObservableObject {
    static let pageSize: UInt32 = 40
    static let maximumImageBytes: UInt64 = 4 * 1_024 * 1_024

    @Published private(set) var state: TimelinePresentationState = .detached
    @Published private(set) var chunks: [TimelineChunkBandSnapshot] = []
    @Published private(set) var searchHits: [TimelineSearchHit] = []
    @Published private(set) var coverage: FactualReportCoverage?
    @Published private(set) var provenance: FactualReportProvenance?
    @Published private(set) var domainContextAvailable = false
    @Published private(set) var stableCutoff: Date?
    @Published private(set) var snapshotToken: String?
    @Published private(set) var nextPageLoading = false
    @Published private(set) var nextPageError: String?
    @Published private(set) var newDataAvailable = false
    @Published private(set) var scrollAnchorRevisionID: String?
    @Published var rangeStart: Date
    @Published var rangeEnd: Date
    @Published var applicationBundleID = ""
    @Published var windowText = ""
    @Published var authorizedDomain = ""
    @Published var selectedCoverageStates: Set<TimelineCoverageState> = []
    @Published var searchText = ""

    private let nowProvider: @Sendable () -> Date
    private var client: (any TimelineEvidenceQuerying)?
    private var activeFilter: TimelineFilter?
    private var appliedSearch = ""
    private var nextCursor: String?
    private var activeLoadID: UUID?
    private var selectedLogicalChunkID: String?

    init(
        client: (any TimelineEvidenceQuerying)? = nil,
        nowProvider: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.client = client
        self.nowProvider = nowProvider
        let end = Self.alignDown(nowProvider())
        rangeEnd = end
        rangeStart = end.addingTimeInterval(-24 * 60 * 60)
        if client != nil { state = .loading }
    }

    var isSearching: Bool { !appliedSearch.isEmpty }
    var hasNextPage: Bool { nextCursor != nil }

    func attach(client: any TimelineEvidenceQuerying) {
        self.client = client
        if state == .detached { state = .loading }
    }

    func detach() {
        activeLoadID = nil
        client = nil
        state = .detached
        chunks = []
        searchHits = []
        coverage = nil
        provenance = nil
        stableCutoff = nil
        snapshotToken = nil
        nextCursor = nil
        nextPageError = nil
        newDataAvailable = false
    }

    func load(now: Date? = nil) async {
        guard let client else {
            state = .detached
            return
        }
        let loadID = UUID()
        activeLoadID = loadID
        let hadPriorData = coverage != nil
        if !hadPriorData { state = .loading }
        nextPageError = nil
        let now = now ?? nowProvider()
        let cutoff = Self.wholeSecond(now)
        let filter = makeFilter()
        let query = appliedSearch
        do {
            if query.isEmpty {
                let response = try await client.page(
                    filter: filter,
                    stableCutoff: cutoff,
                    snapshotToken: nil,
                    cursor: nil,
                    limit: Self.pageSize,
                    now: now
                )
                guard activeLoadID == loadID else { return }
                apply(response, replacing: true)
            } else {
                let response = try await client.search(
                    filter: filter,
                    query: query,
                    stableCutoff: cutoff,
                    snapshotToken: nil,
                    cursor: nil,
                    limit: Self.pageSize,
                    now: now
                )
                guard activeLoadID == loadID else { return }
                apply(response, replacing: true)
            }
            activeFilter = filter
            newDataAvailable = false
            restoreScrollAnchor()
        } catch TimelineQueryError.projectionRebuilding {
            guard activeLoadID == loadID else { return }
            state = .rebuilding(hasPriorData: hadPriorData)
        } catch {
            guard activeLoadID == loadID else { return }
            state = .failed(message: error.localizedDescription, hasPriorData: hadPriorData)
        }
    }

    func applyFilters(now: Date? = nil) async {
        rangeStart = Self.alignDown(rangeStart)
        rangeEnd = Self.alignDown(rangeEnd)
        if rangeStart >= rangeEnd {
            state = .failed(
                message: "The timeline range must contain at least one complete five-minute interval.",
                hasPriorData: coverage != nil
            )
            return
        }
        await load(now: now)
    }

    func submitSearch(now: Date? = nil) async {
        appliedSearch = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        await load(now: now)
    }

    func clearSearch(now: Date? = nil) async {
        searchText = ""
        appliedSearch = ""
        await load(now: now)
    }

    func loadNextPage(now: Date? = nil) async {
        guard !nextPageLoading,
              let client,
              let cursor = nextCursor,
              let filter = activeFilter,
              let cutoff = stableCutoff,
              let token = snapshotToken
        else { return }
        let loadID = activeLoadID
        let query = appliedSearch
        nextPageLoading = true
        nextPageError = nil
        defer { nextPageLoading = false }
        do {
            if appliedSearch.isEmpty {
                let response = try await client.page(
                    filter: filter,
                    stableCutoff: cutoff,
                    snapshotToken: token,
                    cursor: cursor,
                    limit: Self.pageSize,
                    now: now ?? nowProvider()
                )
                guard activeLoadID == loadID,
                      activeFilter == filter,
                      stableCutoff == cutoff,
                      snapshotToken == token,
                      appliedSearch == query,
                      nextCursor == cursor
                else { return }
                apply(response, replacing: false)
            } else {
                let response = try await client.search(
                    filter: filter,
                    query: appliedSearch,
                    stableCutoff: cutoff,
                    snapshotToken: token,
                    cursor: cursor,
                    limit: Self.pageSize,
                    now: now ?? nowProvider()
                )
                guard activeLoadID == loadID,
                      activeFilter == filter,
                      stableCutoff == cutoff,
                      snapshotToken == token,
                      appliedSearch == query,
                      nextCursor == cursor
                else { return }
                apply(response, replacing: false)
            }
        } catch TimelineQueryError.snapshotExpired {
            guard activeLoadID == loadID,
                  activeFilter == filter,
                  stableCutoff == cutoff,
                  snapshotToken == token,
                  appliedSearch == query,
                  nextCursor == cursor
            else { return }
            invalidateSnapshot()
            await load(now: now)
        } catch {
            guard activeLoadID == loadID,
                  activeFilter == filter,
                  stableCutoff == cutoff,
                  snapshotToken == token,
                  appliedSearch == query,
                  nextCursor == cursor
            else { return }
            nextPageError = error.localizedDescription
        }
    }

    func observe(latestProjectionAt: Date?) {
        guard let latestProjectionAt, let stableCutoff,
              latestProjectionAt > stableCutoff
        else { return }
        newDataAvailable = true
    }

    func rememberVisible(revisionID: String?) {
        scrollAnchorRevisionID = revisionID
        guard let revisionID else { return }
        selectedLogicalChunkID = chunks.first(where: { $0.revisionID == revisionID })?.chunkID
    }

    func refreshNewData(now: Date? = nil) async {
        let priorAnchor = scrollAnchorRevisionID
        let priorLogicalID = selectedLogicalChunkID
        await load(now: now)
        if let priorLogicalID,
           let current = chunks.first(where: { $0.chunkID == priorLogicalID })
        {
            scrollAnchorRevisionID = current.revisionID
        } else if let priorAnchor, chunks.contains(where: { $0.revisionID == priorAnchor }) {
            scrollAnchorRevisionID = priorAnchor
        }
    }

    func chunkDetail(
        reference: TimelineChunkReference,
        now: Date? = nil
    ) async throws -> TimelineChunkDetailSnapshot {
        let client = try requireClient()
        let token = try await ensureSnapshot(now: now)
        let requestNow = now ?? nowProvider()
        do {
            return try await client.chunkDetail(
                snapshotToken: token,
                reference: reference,
                now: requestNow
            )
        } catch TimelineQueryError.snapshotExpired {
            let refreshedToken = try await refreshExpiredSnapshot(now: requestNow)
            return try await client.chunkDetail(
                snapshotToken: refreshedToken,
                reference: reference,
                now: requestNow
            )
        }
    }

    func eventDetail(eventID: String, now: Date? = nil) async throws -> TimelineEventDetailSnapshot {
        let client = try requireClient()
        let token = try await ensureSnapshot(now: now)
        let requestNow = now ?? nowProvider()
        do {
            return try await client.eventDetail(
                snapshotToken: token,
                eventID: eventID,
                now: requestNow
            )
        } catch TimelineQueryError.snapshotExpired {
            let refreshedToken = try await refreshExpiredSnapshot(now: requestNow)
            return try await client.eventDetail(
                snapshotToken: refreshedToken,
                eventID: eventID,
                now: requestNow
            )
        }
    }

    func image(_ metadata: TimelineImageMetadata) async throws -> Data {
        try await requireClient().image(metadata, maxBytes: Self.maximumImageBytes)
    }

    private func ensureSnapshot(now: Date?) async throws -> String {
        if let snapshotToken { return snapshotToken }
        await load(now: now)
        guard let snapshotToken else { throw TimelineQueryError.missingSnapshot }
        return snapshotToken
    }

    private func refreshExpiredSnapshot(now: Date) async throws -> String {
        invalidateSnapshot()
        await load(now: now)
        guard let snapshotToken else { throw TimelineQueryError.snapshotExpired }
        return snapshotToken
    }

    private func invalidateSnapshot() {
        snapshotToken = nil
        nextCursor = nil
        nextPageError = nil
    }

    private func requireClient() throws -> any TimelineEvidenceQuerying {
        guard let client else { throw TimelineQueryError.missingSnapshot }
        return client
    }

    private func makeFilter() -> TimelineFilter {
        TimelineFilter(
            range: FactualReportRangePayload(start: rangeStart, end: rangeEnd),
            applicationBundleID: normalized(applicationBundleID),
            windowText: normalized(windowText),
            authorizedDomain: domainContextAvailable ? normalized(authorizedDomain) : nil,
            coverageStates: selectedCoverageStates.sorted { $0.rawValue < $1.rawValue }
        )
    }

    private func normalized(_ value: String) -> String? {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }

    private func apply(_ response: TimelinePageSnapshot, replacing: Bool) {
        stableCutoff = response.stableCutoff
        snapshotToken = response.snapshotToken
        coverage = response.coverage
        provenance = response.provenance
        domainContextAvailable = response.domainContextAvailable
        chunks = replacing ? response.chunks : deduplicated(chunks + response.chunks, by: \ .revisionID)
        if replacing { searchHits = [] }
        nextCursor = response.page.nextCursor
        state = response.chunks.isEmpty && chunks.isEmpty
            ? .empty
            : presentation(for: response.coverage)
    }

    private func apply(_ response: TimelineSearchSnapshot, replacing: Bool) {
        stableCutoff = response.stableCutoff
        snapshotToken = response.snapshotToken
        coverage = response.coverage
        provenance = response.provenance
        searchHits = replacing
            ? response.hits
            : deduplicated(searchHits + response.hits, by: \ .eventID)
        if replacing { chunks = [] }
        nextCursor = response.page.nextCursor
        state = response.hits.isEmpty && searchHits.isEmpty
            ? .noMatches
            : presentation(for: response.coverage)
    }

    private func presentation(for coverage: FactualReportCoverage) -> TimelinePresentationState {
        coverage.evidenceSeconds.captured == coverage.evidenceSeconds.total
            ? .populated
            : .partial
    }

    private func deduplicated<Value, Key: Hashable>(
        _ values: [Value],
        by keyPath: KeyPath<Value, Key>
    ) -> [Value] {
        var seen = Set<Key>()
        return values.filter { seen.insert($0[keyPath: keyPath]).inserted }
    }

    private func restoreScrollAnchor() {
        guard let selectedLogicalChunkID else { return }
        scrollAnchorRevisionID = chunks.first(where: { $0.chunkID == selectedLogicalChunkID })?.revisionID
    }

    private static func alignDown(_ date: Date) -> Date {
        Date(timeIntervalSince1970: floor(date.timeIntervalSince1970 / 300) * 300)
    }

    private static func wholeSecond(_ date: Date) -> Date {
        Date(timeIntervalSince1970: floor(date.timeIntervalSince1970))
    }
}
