import Foundation

enum AnalysisPresentationState: Equatable, Sendable {
    case detached
    case loading
    case empty
    case populated
    case rebuilding(hasPriorData: Bool)
    case failed(message: String, hasPriorData: Bool)
}

@MainActor
final class AnalysisViewModel: ObservableObject {
    static let pageSize: UInt32 = 40

    @Published private(set) var state: AnalysisPresentationState = .detached
    @Published private(set) var artifacts: [AnalysisArtifactSnapshot] = []
    @Published private(set) var provenance: AnalysisProvenance?
    @Published private(set) var stableCutoff: Date?
    @Published private(set) var snapshotToken: String?
    @Published private(set) var nextPageLoading = false
    @Published private(set) var nextPageError: String?
    @Published var rangeStart: Date
    @Published var rangeEnd: Date

    private let nowProvider: @Sendable () -> Date
    private var client: (any AnalysisEvidenceQuerying)?
    private var activeRange: FactualReportRangePayload?
    private var nextCursor: String?
    private var activeLoadID: UUID?

    init(
        client: (any AnalysisEvidenceQuerying)? = nil,
        nowProvider: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.client = client
        self.nowProvider = nowProvider
        let end = Self.alignDown(nowProvider())
        rangeEnd = end
        rangeStart = end.addingTimeInterval(-30 * 24 * 60 * 60)
        if client != nil { state = .loading }
    }

    var hasNextPage: Bool { nextCursor != nil }

    func attach(client: any AnalysisEvidenceQuerying) {
        self.client = client
        if state == .detached { state = .loading }
    }

    func detach() {
        activeLoadID = nil
        client = nil
        state = .detached
        artifacts = []
        provenance = nil
        stableCutoff = nil
        snapshotToken = nil
        nextCursor = nil
    }

    func load(now: Date? = nil) async {
        guard let client else {
            state = .detached
            return
        }
        let range = FactualReportRangePayload(
            start: Self.alignDown(rangeStart),
            end: Self.alignDown(rangeEnd)
        )
        guard range.start < range.end else {
            state = .failed(
                message: "The analysis range start must precede its end.",
                hasPriorData: !artifacts.isEmpty
            )
            return
        }
        let loadID = UUID()
        activeLoadID = loadID
        let prior = !artifacts.isEmpty
        if !prior { state = .loading }
        do {
            let now = now ?? nowProvider()
            let response = try await client.page(
                range: range,
                stableCutoff: Self.wholeSecond(now),
                snapshotToken: nil,
                cursor: nil,
                limit: Self.pageSize,
                now: now
            )
            guard activeLoadID == loadID else { return }
            apply(response, replacing: true)
            activeRange = range
        } catch TimelineQueryError.projectionRebuilding {
            guard activeLoadID == loadID else { return }
            state = .rebuilding(hasPriorData: prior)
        } catch {
            guard activeLoadID == loadID else { return }
            state = .failed(message: error.localizedDescription, hasPriorData: prior)
        }
    }

    func loadNextPage(now: Date? = nil) async {
        guard !nextPageLoading,
              let client,
              let range = activeRange,
              let cutoff = stableCutoff,
              let token = snapshotToken,
              let cursor = nextCursor
        else { return }
        let loadID = activeLoadID
        nextPageLoading = true
        nextPageError = nil
        defer { nextPageLoading = false }
        do {
            let response = try await client.page(
                range: range,
                stableCutoff: cutoff,
                snapshotToken: token,
                cursor: cursor,
                limit: Self.pageSize,
                now: now ?? nowProvider()
            )
            guard activeLoadID == loadID,
                  activeRange == range,
                  stableCutoff == cutoff,
                  snapshotToken == token,
                  nextCursor == cursor
            else { return }
            apply(response, replacing: false)
        } catch TimelineQueryError.snapshotExpired {
            guard activeLoadID == loadID,
                  activeRange == range,
                  stableCutoff == cutoff,
                  snapshotToken == token,
                  nextCursor == cursor
            else { return }
            invalidateSnapshot()
            await load(now: now)
        } catch {
            guard activeLoadID == loadID,
                  activeRange == range,
                  stableCutoff == cutoff,
                  snapshotToken == token,
                  nextCursor == cursor
            else { return }
            nextPageError = error.localizedDescription
        }
    }

    func detail(
        artifactID: String,
        revisionID: String,
        now: Date? = nil
    ) async throws -> AnalysisDetailSnapshot {
        guard let client, let snapshotToken else { throw TimelineQueryError.missingSnapshot }
        let requestNow = now ?? nowProvider()
        do {
            return try await client.detail(
                snapshotToken: snapshotToken,
                artifactID: artifactID,
                revisionID: revisionID,
                now: requestNow
            )
        } catch TimelineQueryError.snapshotExpired {
            invalidateSnapshot()
            await load(now: requestNow)
            guard let refreshedToken = self.snapshotToken else {
                throw TimelineQueryError.snapshotExpired
            }
            return try await client.detail(
                snapshotToken: refreshedToken,
                artifactID: artifactID,
                revisionID: revisionID,
                now: requestNow
            )
        }
    }

    private func apply(_ response: AnalysisPageSnapshot, replacing: Bool) {
        stableCutoff = response.stableCutoff
        snapshotToken = response.snapshotToken
        provenance = response.provenance
        artifacts = replacing
            ? response.artifacts
            : deduplicated(artifacts + response.artifacts)
        nextCursor = response.page.nextCursor
        state = artifacts.isEmpty ? .empty : .populated
    }

    private func deduplicated(_ values: [AnalysisArtifactSnapshot]) -> [AnalysisArtifactSnapshot] {
        var seen = Set<String>()
        return values.filter { seen.insert($0.revisionID).inserted }
    }

    private func invalidateSnapshot() {
        snapshotToken = nil
        nextCursor = nil
        nextPageError = nil
    }

    private static func wholeSecond(_ date: Date) -> Date {
        Date(timeIntervalSince1970: floor(date.timeIntervalSince1970))
    }

    private static func alignDown(_ date: Date) -> Date {
        Date(timeIntervalSince1970: floor(date.timeIntervalSince1970 / 300) * 300)
    }
}
