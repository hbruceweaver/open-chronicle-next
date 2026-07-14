import XCTest
@testable import OpenChronicle

final class TimelineAnalysisTests: XCTestCase {
    func testUnicodeHighlightRangesProduceInertSegments() {
        let text = "🧠 Ignore <script>alert('x')</script> café"
        let cafeIndex = try! XCTUnwrap(text.range(of: "café")?.lowerBound)
        let cafeStart = text.unicodeScalars.distance(
            from: text.unicodeScalars.startIndex,
            to: cafeIndex
        )
        let snippet = TimelineSnippet(
            text: text,
            highlights: [
                TimelineHighlightRange(start: 0, length: 1),
                TimelineHighlightRange(start: UInt32(cafeStart), length: 4),
                TimelineHighlightRange(start: 999, length: 10),
            ]
        )

        let segments = snippet.segments

        XCTAssertEqual(segments.first, .init(text: "🧠", highlighted: true))
        XCTAssertTrue(segments.map(\.text).joined().contains("<script>"))
        XCTAssertEqual(segments.map(\.text).joined(), snippet.text)
        XCTAssertEqual(segments.last, .init(text: "café", highlighted: true))
    }

    func testUnicodeHighlightOffsetsCountDecomposedScalarsNotGraphemeClusters() {
        let text = "Caf\u{65}\u{301} noir"
        let snippet = TimelineSnippet(
            text: text,
            highlights: [TimelineHighlightRange(start: 3, length: 2)]
        )

        XCTAssertEqual(snippet.segments, [
            .init(text: "Caf", highlighted: false),
            .init(text: "e\u{301}", highlighted: true),
            .init(text: " noir", highlighted: false),
        ])
        XCTAssertEqual(snippet.segments.map(\.text).joined(), text)
    }

    func testUnicodeHighlightOffsetsCountZWJEmojiScalars() {
        let text = "A👨‍👩‍👧‍👦B"
        let snippet = TimelineSnippet(
            text: text,
            highlights: [TimelineHighlightRange(start: 1, length: 7)]
        )

        XCTAssertEqual(snippet.segments, [
            .init(text: "A", highlighted: false),
            .init(text: "👨‍👩‍👧‍👦", highlighted: true),
            .init(text: "B", highlighted: false),
        ])
        XCTAssertEqual(snippet.segments.map(\.text).joined(), text)
    }

    func testAnalysisBoundaryNeverLabelsDerivedClaimsAsEvidence() {
        XCTAssertEqual(DerivedAnalysisBoundary.title, "Derived analysis")
        XCTAssertTrue(DerivedAnalysisBoundary.explanation.contains("never rewrite factual evidence"))
    }

    @MainActor
    func testPaginationEchoesFrozenTokenAndCutoffWithoutDuplicates() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let first = makeBand(chunkID: "chunk-1", revisionID: "revision-1", start: now.addingTimeInterval(-600))
        let second = makeBand(chunkID: "chunk-2", revisionID: "revision-2", start: now.addingTimeInterval(-300))
        let client = TimelineStubClient(pageChunks: [[first], [first, second]])
        let model = TimelineViewModel(client: client, nowProvider: { now })

        await model.load(now: now)
        XCTAssertEqual(model.snapshotToken, "opaque-token")
        XCTAssertEqual(model.chunks.map(\.revisionID), ["revision-1"])
        XCTAssertEqual(model.state, .partial)
        XCTAssertTrue(model.hasNextPage)

        await model.loadNextPage(now: now.addingTimeInterval(30))

        XCTAssertEqual(model.chunks.map(\.revisionID), ["revision-1", "revision-2"])
        XCTAssertFalse(model.hasNextPage)
        let calls = await client.pageCalls
        XCTAssertNil(calls[0].token)
        XCTAssertNil(calls[0].cursor)
        XCTAssertEqual(calls[1].token, "opaque-token")
        XCTAssertEqual(calls[1].cursor, "chunk-1")
        XCTAssertEqual(calls[1].cutoff, calls[0].cutoff)
    }

    @MainActor
    func testRefreshFailurePreservesPriorFrozenData() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let band = makeBand(chunkID: "chunk-1", revisionID: "revision-1", start: now.addingTimeInterval(-300))
        let client = TimelineStubClient(pageChunks: [[band]])
        let model = TimelineViewModel(client: client, nowProvider: { now })
        await model.load(now: now)
        await client.failFuturePages("synthetic refresh failure")

        await model.load(now: now.addingTimeInterval(60))

        XCTAssertEqual(model.chunks.map(\.revisionID), ["revision-1"])
        XCTAssertEqual(
            model.state,
            .failed(message: "synthetic refresh failure", hasPriorData: true)
        )
    }

    @MainActor
    func testNewDataRefreshPreservesLogicalChunkAndMovesToNewRevision() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let first = makeBand(chunkID: "chunk-stable", revisionID: "revision-1", start: now.addingTimeInterval(-300))
        let revised = makeBand(chunkID: "chunk-stable", revisionID: "revision-2", start: now.addingTimeInterval(-300))
        let client = TimelineStubClient(pageChunks: [[first], [revised]])
        let model = TimelineViewModel(client: client, nowProvider: { now })
        await model.load(now: now)
        model.rememberVisible(revisionID: "revision-1")
        model.observe(latestProjectionAt: now.addingTimeInterval(1))
        XCTAssertTrue(model.newDataAvailable)

        await model.refreshNewData(now: now.addingTimeInterval(60))

        XCTAssertEqual(model.scrollAnchorRevisionID, "revision-2")
        XCTAssertEqual(model.chunks.map(\.revisionID), ["revision-2"])
        XCTAssertFalse(model.newDataAvailable)
    }

    @MainActor
    func testSearchNoMatchesIsDistinctFromEmptyTimeline() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let client = TimelineStubClient(pageChunks: [[]], searchHits: [[]])
        let model = TimelineViewModel(client: client, nowProvider: { now })
        model.searchText = "literal query"

        await model.submitSearch(now: now)

        XCTAssertEqual(model.state, .noMatches)
        XCTAssertTrue(model.isSearching)
        let calls = await client.searchCalls
        XCTAssertEqual(calls.first?.query, "literal query")
        XCTAssertNil(calls.first?.token)
    }

    func testCoreClientSendsNullFirstSnapshotSelectorsAndCoverageSpellings() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let range = FactualReportRangePayload(
            start: now.addingTimeInterval(-300),
            end: now
        )
        let filter = TimelineFilter(
            range: range,
            applicationBundleID: nil,
            windowText: nil,
            authorizedDomain: nil,
            coverageStates: [.captured, .missingObservation]
        )
        let response = makePage(
            filter: filter,
            cutoff: now,
            chunks: [],
            nextCursor: nil
        )
        let core = CapturingTimelineCore(response: try encodeEnvelope(response))
        let client = CoreTimelineEvidenceClient(core: core)

        _ = try await client.page(
            filter: filter,
            stableCutoff: now,
            snapshotToken: nil,
            cursor: nil,
            limit: 40,
            now: now
        )

        let capturedRequest = await core.lastRequest()
        let request = try XCTUnwrap(capturedRequest)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: request) as? [String: Any]
        )
        let control = try XCTUnwrap(object["control"] as? [String: Any])
        let page = try XCTUnwrap(control["page"] as? [String: Any])
        let sentFilter = try XCTUnwrap(control["filter"] as? [String: Any])
        XCTAssertTrue(control["snapshot_token"] is NSNull)
        XCTAssertTrue(page["cursor"] is NSNull)
        XCTAssertEqual(
            sentFilter["coverage_states"] as? [String],
            ["captured", "missing-observation"]
        )
    }

    func testCoreClientsMapStaleGenerationSnapshotToTypedExpiry() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let payload = ChronicleErrorPayload(
            code: "snapshot-no-longer-available",
            message: "refresh from the first page",
            retryable: true
        )
        let core = FailingTimelineCore(error: .bridgeStatus(9, payload))
        let range = FactualReportRangePayload(start: now.addingTimeInterval(-300), end: now)
        let filter = TimelineFilter(
            range: range,
            applicationBundleID: nil,
            windowText: nil,
            authorizedDomain: nil,
            coverageStates: []
        )

        do {
            _ = try await CoreTimelineEvidenceClient(core: core).page(
                filter: filter,
                stableCutoff: now,
                snapshotToken: "expired-token",
                cursor: "next",
                limit: 40,
                now: now
            )
            XCTFail("Expected timeline snapshot expiry")
        } catch let error as TimelineQueryError {
            XCTAssertEqual(error, .snapshotExpired)
        }

        do {
            _ = try await CoreAnalysisEvidenceClient(core: core).page(
                range: range,
                stableCutoff: now,
                snapshotToken: "expired-token",
                cursor: "next",
                limit: 40,
                now: now
            )
            XCTFail("Expected analysis snapshot expiry")
        } catch let error as TimelineQueryError {
            XCTAssertEqual(error, .snapshotExpired)
        }
    }

    @MainActor
    func testAppModelFansAuthoritativeProjectionTimestampOutToTimeline() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let band = makeBand(
            chunkID: "chunk-1",
            revisionID: "revision-1",
            start: now.addingTimeInterval(-300)
        )
        let app = AppModel(
            shouldStartCapture: { false },
            notificationService: NotificationService(backend: SilentNotificationBackend())
        )
        app.timelineViewModel.attach(client: TimelineStubClient(pageChunks: [[band]]))
        await app.timelineViewModel.load(now: now)
        XCTAssertFalse(app.timelineViewModel.newDataAvailable)

        await app.applyStorageMonitorUpdate(.snapshot(makeDiagnosticHealth(
            lastProjectionAt: now.addingTimeInterval(1)
        )))

        XCTAssertTrue(app.timelineViewModel.newDataAvailable)
    }

    func testRealRustCoreDecodesEmptyTimelineAndAnalysisSnapshots() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("open-chronicle-timeline-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let start = try timelineDate("2026-07-14T09:00:00Z")
        let end = try timelineDate("2026-07-14T09:05:00Z")
        let now = try timelineDate("2026-07-14T09:07:00Z")
        let core = try InProcessCore(applicationSupportURL: root, now: now)
        let range = FactualReportRangePayload(start: start, end: end)
        let filter = TimelineFilter(
            range: range,
            applicationBundleID: nil,
            windowText: nil,
            authorizedDomain: nil,
            coverageStates: []
        )

        let timeline = try await CoreTimelineEvidenceClient(core: core).page(
            filter: filter,
            stableCutoff: now,
            snapshotToken: nil,
            cursor: nil,
            limit: 40,
            now: now
        )
        let analysis = try await CoreAnalysisEvidenceClient(core: core).page(
            range: range,
            stableCutoff: now,
            snapshotToken: nil,
            cursor: nil,
            limit: 40,
            now: now
        )
        try await core.close()

        XCTAssertTrue(timeline.chunks.isEmpty)
        XCTAssertEqual(timeline.coverage.evidenceSeconds.gap, 300)
        XCTAssertFalse(timeline.snapshotToken.isEmpty)
        XCTAssertTrue(analysis.artifacts.isEmpty)
        XCTAssertEqual(analysis.range, range)
        XCTAssertFalse(analysis.snapshotToken.isEmpty)
    }

    func testScreenshotReadRejectsNonRetainedStateBeforeCoreCall() async throws {
        let core = CapturingTimelineCore(response: Data())
        let client = CoreTimelineEvidenceClient(core: core)
        let expired = TimelineImageMetadata(
            artifactID: "image-1",
            state: "expired",
            expiresAt: nil
        )

        do {
            _ = try await client.image(expired, maxBytes: 1_024)
            XCTFail("Expected expired image to be rejected")
        } catch let error as TimelineQueryError {
            XCTAssertEqual(error, .imageUnavailable("expired"))
        }
        let rejectedReadCount = await core.imageReadCount()
        XCTAssertEqual(rejectedReadCount, 0)

        let retained = TimelineImageMetadata(
            artifactID: "image-1",
            state: "retained",
            expiresAt: nil
        )
        _ = try await client.image(retained, maxBytes: 1_024)
        let retainedReadCount = await core.imageReadCount()
        XCTAssertEqual(retainedReadCount, 1)
    }

    @MainActor
    func testAnalysisUsesRealPageAndExactDetailWithAttributionAndEvidence() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let artifact = makeAnalysisArtifact(createdAt: now.addingTimeInterval(-60))
        let client = AnalysisStubClient(artifact: artifact)
        let model = AnalysisViewModel(client: client, nowProvider: { now })

        await model.load(now: now)

        XCTAssertEqual(model.state, .populated)
        XCTAssertEqual(model.artifacts.map(\.revisionID), ["analysis-revision-1"])
        XCTAssertEqual(model.artifacts[0].author.clientID, "client-codex")
        XCTAssertEqual(model.artifacts[0].author.model, "gpt-5")
        XCTAssertEqual(model.artifacts[0].evidence.chunkIDs, ["chunk-1"])
        XCTAssertEqual(model.artifacts[0].evidence.eventIDs, ["event-1"])

        let detail = try await model.detail(
            artifactID: artifact.artifactID,
            revisionID: artifact.revisionID,
            now: now
        )
        XCTAssertEqual(detail.artifact, artifact)
        let calls = await client.detailCalls
        XCTAssertEqual(calls.first?.token, "analysis-token")
        XCTAssertEqual(calls.first?.artifactID, artifact.artifactID)
        XCTAssertEqual(calls.first?.revisionID, artifact.revisionID)
    }

    @MainActor
    func testStalePaginationCannotOverwriteARefreshedResultSet() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let first = makeBand(chunkID: "old-1", revisionID: "old-revision-1", start: now.addingTimeInterval(-600))
        let stale = makeBand(chunkID: "old-2", revisionID: "old-revision-2", start: now.addingTimeInterval(-300))
        let refreshed = makeBand(chunkID: "new-1", revisionID: "new-revision-1", start: now.addingTimeInterval(-300))
        let client = PaginationRaceClient(first: first, stale: stale, refreshed: refreshed)
        let model = TimelineViewModel(client: client, nowProvider: { now })
        await model.load(now: now)

        let pagination = Task { @MainActor in await model.loadNextPage(now: now) }
        await client.waitUntilPaginationStarted()
        await model.load(now: now.addingTimeInterval(60))
        await client.releasePagination()
        await pagination.value

        XCTAssertEqual(model.chunks.map(\.revisionID), ["new-revision-1"])
        XCTAssertEqual(model.snapshotToken, "new-token")
        XCTAssertNil(model.nextPageError)
    }

    @MainActor
    func testStalePaginationFailureCannotPolluteARefreshedResultSet() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let first = makeBand(chunkID: "old-1", revisionID: "old-revision-1", start: now.addingTimeInterval(-600))
        let stale = makeBand(chunkID: "old-2", revisionID: "old-revision-2", start: now.addingTimeInterval(-300))
        let refreshed = makeBand(chunkID: "new-1", revisionID: "new-revision-1", start: now.addingTimeInterval(-300))
        let client = PaginationRaceClient(
            first: first,
            stale: stale,
            refreshed: refreshed,
            staleThrows: true
        )
        let model = TimelineViewModel(client: client, nowProvider: { now })
        await model.load(now: now)

        let pagination = Task { @MainActor in await model.loadNextPage(now: now) }
        await client.waitUntilPaginationStarted()
        await model.load(now: now.addingTimeInterval(60))
        await client.releasePagination()
        await pagination.value

        XCTAssertEqual(model.chunks.map(\.revisionID), ["new-revision-1"])
        XCTAssertNil(model.nextPageError)
    }

    @MainActor
    func testTimelineSnapshotExpiryReestablishesPaginationAndDetailSnapshots() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let band = makeBand(
            chunkID: "chunk-stable",
            revisionID: "revision-stable",
            start: now.addingTimeInterval(-300)
        )
        let client = ExpiringTimelineClient(band: band)
        let model = TimelineViewModel(client: client, nowProvider: { now })

        await model.load(now: now)
        XCTAssertEqual(model.snapshotToken, "timeline-token-1")
        await model.loadNextPage(now: now.addingTimeInterval(1))
        XCTAssertEqual(model.snapshotToken, "timeline-token-2")
        XCTAssertNil(model.nextPageError)

        let chunk = try await model.chunkDetail(
            reference: .revision(band.revisionID),
            now: now.addingTimeInterval(2)
        )
        XCTAssertEqual(chunk.snapshotToken, "timeline-token-3")

        let event = try await model.eventDetail(
            eventID: "event-1",
            now: now.addingTimeInterval(3)
        )
        XCTAssertEqual(event.snapshotToken, "timeline-token-4")

        let pageCalls = await client.pageCalls
        let chunkTokens = await client.chunkTokens
        let eventTokens = await client.eventTokens
        XCTAssertEqual(pageCalls.map(\.token), [nil, "timeline-token-1", nil, nil, nil])
        XCTAssertEqual(chunkTokens, ["timeline-token-2", "timeline-token-3"])
        XCTAssertEqual(eventTokens, ["timeline-token-3", "timeline-token-4"])
    }

    @MainActor
    func testAnalysisSnapshotExpiryReestablishesPaginationAndDetailSnapshots() async throws {
        let now = try timelineDate("2026-07-14T12:00:00Z")
        let artifact = makeAnalysisArtifact(createdAt: now.addingTimeInterval(-60))
        let client = ExpiringAnalysisClient(artifact: artifact)
        let model = AnalysisViewModel(client: client, nowProvider: { now })

        await model.load(now: now)
        XCTAssertEqual(model.snapshotToken, "analysis-token-1")
        await model.loadNextPage(now: now.addingTimeInterval(1))
        XCTAssertEqual(model.snapshotToken, "analysis-token-2")
        XCTAssertNil(model.nextPageError)

        let detail = try await model.detail(
            artifactID: artifact.artifactID,
            revisionID: artifact.revisionID,
            now: now.addingTimeInterval(2)
        )
        XCTAssertEqual(detail.snapshotToken, "analysis-token-3")
        let detailTokens = await client.detailTokens
        XCTAssertEqual(detailTokens, ["analysis-token-2", "analysis-token-3"])
    }
}

private actor ExpiringTimelineClient: TimelineEvidenceQuerying {
    struct PageCall: Sendable {
        let token: String?
        let cursor: String?
    }

    let band: TimelineChunkBandSnapshot
    private var pageGeneration = 0
    private var chunkExpired = false
    private var eventExpired = false
    private(set) var pageCalls: [PageCall] = []
    private(set) var chunkTokens: [String] = []
    private(set) var eventTokens: [String] = []

    init(band: TimelineChunkBandSnapshot) {
        self.band = band
    }

    func page(
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> TimelinePageSnapshot {
        pageCalls.append(PageCall(token: snapshotToken, cursor: cursor))
        if snapshotToken != nil { throw TimelineQueryError.snapshotExpired }
        pageGeneration += 1
        let token = "timeline-token-\(pageGeneration)"
        return TimelinePageSnapshot(
            schemaVersion: "1.0",
            generatedAt: now,
            stableCutoff: stableCutoff,
            snapshotToken: token,
            storeGeneration: 1,
            filter: filter,
            coverage: makeCoverage(filter.range),
            chunks: [band],
            page: TimelinePageInfo(
                nextCursor: pageGeneration == 1 ? "next" : nil,
                returnedItems: 1,
                truncated: pageGeneration == 1
            ),
            domainContextAvailable: false,
            provenance: makeProvenance()
        )
    }

    func search(
        filter: TimelineFilter,
        query: String,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> TimelineSearchSnapshot {
        throw TimelineStubError(message: "unused")
    }

    func chunkDetail(
        snapshotToken: String,
        reference: TimelineChunkReference,
        now: Date
    ) throws -> TimelineChunkDetailSnapshot {
        chunkTokens.append(snapshotToken)
        if !chunkExpired {
            chunkExpired = true
            throw TimelineQueryError.snapshotExpired
        }
        return makeChunkDetail(band: band, token: snapshotToken, now: now)
    }

    func eventDetail(
        snapshotToken: String,
        eventID: String,
        now: Date
    ) throws -> TimelineEventDetailSnapshot {
        eventTokens.append(snapshotToken)
        if !eventExpired {
            eventExpired = true
            throw TimelineQueryError.snapshotExpired
        }
        return makeEventDetail(eventID: eventID, token: snapshotToken, now: now)
    }

    func image(_ metadata: TimelineImageMetadata, maxBytes: UInt64) -> Data { Data() }
}

private actor ExpiringAnalysisClient: AnalysisEvidenceQuerying {
    let artifact: AnalysisArtifactSnapshot
    private var pageGeneration = 0
    private var detailExpired = false
    private(set) var detailTokens: [String] = []

    init(artifact: AnalysisArtifactSnapshot) {
        self.artifact = artifact
    }

    func page(
        range: FactualReportRangePayload,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> AnalysisPageSnapshot {
        if snapshotToken != nil { throw TimelineQueryError.snapshotExpired }
        pageGeneration += 1
        return AnalysisPageSnapshot(
            schemaVersion: "1.0",
            generatedAt: now,
            stableCutoff: stableCutoff,
            snapshotToken: "analysis-token-\(pageGeneration)",
            storeGeneration: 1,
            range: range,
            artifacts: [artifact],
            page: TimelinePageInfo(
                nextCursor: pageGeneration == 1 ? "next" : nil,
                returnedItems: 1,
                truncated: pageGeneration == 1
            ),
            provenance: makeAnalysisProvenance(artifact: artifact)
        )
    }

    func detail(
        snapshotToken: String,
        artifactID: String,
        revisionID: String?,
        now: Date
    ) throws -> AnalysisDetailSnapshot {
        detailTokens.append(snapshotToken)
        if !detailExpired {
            detailExpired = true
            throw TimelineQueryError.snapshotExpired
        }
        return AnalysisDetailSnapshot(
            schemaVersion: "1.0",
            generatedAt: now,
            stableCutoff: now,
            snapshotToken: snapshotToken,
            storeGeneration: 1,
            artifact: artifact,
            provenance: makeAnalysisProvenance(artifact: artifact)
        )
    }
}

private actor AnalysisStubClient: AnalysisEvidenceQuerying {
    struct DetailCall: Sendable {
        let token: String
        let artifactID: String
        let revisionID: String?
    }

    let artifact: AnalysisArtifactSnapshot
    private(set) var detailCalls: [DetailCall] = []

    init(artifact: AnalysisArtifactSnapshot) {
        self.artifact = artifact
    }

    func page(
        range: FactualReportRangePayload,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) -> AnalysisPageSnapshot {
        AnalysisPageSnapshot(
            schemaVersion: "1.0",
            generatedAt: now,
            stableCutoff: stableCutoff,
            snapshotToken: "analysis-token",
            storeGeneration: 1,
            range: range,
            artifacts: [artifact],
            page: TimelinePageInfo(nextCursor: nil, returnedItems: 1, truncated: false),
            provenance: makeAnalysisProvenance(artifact: artifact)
        )
    }

    func detail(
        snapshotToken: String,
        artifactID: String,
        revisionID: String?,
        now: Date
    ) -> AnalysisDetailSnapshot {
        detailCalls.append(DetailCall(
            token: snapshotToken,
            artifactID: artifactID,
            revisionID: revisionID
        ))
        return AnalysisDetailSnapshot(
            schemaVersion: "1.0",
            generatedAt: now,
            stableCutoff: now,
            snapshotToken: snapshotToken,
            storeGeneration: 1,
            artifact: artifact,
            provenance: makeAnalysisProvenance(artifact: artifact)
        )
    }
}

private actor PaginationRaceClient: TimelineEvidenceQuerying {
    let first: TimelineChunkBandSnapshot
    let stale: TimelineChunkBandSnapshot
    let refreshed: TimelineChunkBandSnapshot
    let staleThrows: Bool
    private var firstPageCount = 0
    private var paginationStarted = false
    private var paginationContinuation: CheckedContinuation<Void, Never>?

    init(
        first: TimelineChunkBandSnapshot,
        stale: TimelineChunkBandSnapshot,
        refreshed: TimelineChunkBandSnapshot,
        staleThrows: Bool = false
    ) {
        self.first = first
        self.stale = stale
        self.refreshed = refreshed
        self.staleThrows = staleThrows
    }

    func page(
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) async throws -> TimelinePageSnapshot {
        if cursor != nil {
            paginationStarted = true
            await withCheckedContinuation { paginationContinuation = $0 }
            if staleThrows { throw TimelineStubError(message: "stale page failed") }
            return pageSnapshot(
                filter: filter,
                cutoff: stableCutoff,
                token: "old-token",
                chunks: [stale],
                next: nil
            )
        }
        firstPageCount += 1
        if firstPageCount == 1 {
            return pageSnapshot(
                filter: filter,
                cutoff: stableCutoff,
                token: "old-token",
                chunks: [first],
                next: first.chunkID
            )
        }
        return pageSnapshot(
            filter: filter,
            cutoff: stableCutoff,
            token: "new-token",
            chunks: [refreshed],
            next: nil
        )
    }

    func waitUntilPaginationStarted() async {
        while !paginationStarted { await Task.yield() }
    }

    func releasePagination() {
        paginationContinuation?.resume()
        paginationContinuation = nil
    }

    func search(
        filter: TimelineFilter,
        query: String,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> TimelineSearchSnapshot { throw TimelineStubError(message: "unused") }

    func chunkDetail(
        snapshotToken: String,
        reference: TimelineChunkReference,
        now: Date
    ) throws -> TimelineChunkDetailSnapshot { throw TimelineStubError(message: "unused") }

    func eventDetail(
        snapshotToken: String,
        eventID: String,
        now: Date
    ) throws -> TimelineEventDetailSnapshot { throw TimelineStubError(message: "unused") }

    func image(_ metadata: TimelineImageMetadata, maxBytes: UInt64) -> Data { Data() }

    private func pageSnapshot(
        filter: TimelineFilter,
        cutoff: Date,
        token: String,
        chunks: [TimelineChunkBandSnapshot],
        next: String?
    ) -> TimelinePageSnapshot {
        TimelinePageSnapshot(
            schemaVersion: "1.0",
            generatedAt: cutoff,
            stableCutoff: cutoff,
            snapshotToken: token,
            storeGeneration: 1,
            filter: filter,
            coverage: makeCoverage(filter.range),
            chunks: chunks,
            page: TimelinePageInfo(
                nextCursor: next,
                returnedItems: UInt32(chunks.count),
                truncated: next != nil
            ),
            domainContextAvailable: false,
            provenance: makeProvenance()
        )
    }
}

private actor TimelineStubClient: TimelineEvidenceQuerying {
    struct PageCall: Sendable {
        let token: String?
        let cursor: String?
        let cutoff: Date
    }

    struct SearchCall: Sendable {
        let query: String
        let token: String?
    }

    private var pageChunks: [[TimelineChunkBandSnapshot]]
    private let searchHitsFixture: [[TimelineSearchHit]]
    private var failure: String?
    private(set) var pageCalls: [PageCall] = []
    private(set) var searchCalls: [SearchCall] = []

    init(
        pageChunks: [[TimelineChunkBandSnapshot]],
        searchHits: [[TimelineSearchHit]] = []
    ) {
        self.pageChunks = pageChunks
        searchHitsFixture = searchHits
    }

    func failFuturePages(_ message: String) {
        failure = message
    }

    func page(
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> TimelinePageSnapshot {
        if let failure { throw TimelineStubError(message: failure) }
        pageCalls.append(PageCall(token: snapshotToken, cursor: cursor, cutoff: stableCutoff))
        let index = min(pageCalls.count - 1, max(0, pageChunks.count - 1))
        let chunks = pageChunks.isEmpty ? [] : pageChunks[index]
        let hasNext = pageCalls.count < pageChunks.count
        return makePage(
            filter: filter,
            cutoff: stableCutoff,
            chunks: chunks,
            nextCursor: hasNext ? chunks.last?.chunkID : nil
        )
    }

    func search(
        filter: TimelineFilter,
        query: String,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) throws -> TimelineSearchSnapshot {
        if let failure { throw TimelineStubError(message: failure) }
        searchCalls.append(SearchCall(query: query, token: snapshotToken))
        let index = min(searchCalls.count - 1, max(0, searchHitsFixture.count - 1))
        let hits = searchHitsFixture.isEmpty ? [] : searchHitsFixture[index]
        return TimelineSearchSnapshot(
            schemaVersion: "1.0",
            generatedAt: stableCutoff,
            stableCutoff: stableCutoff,
            snapshotToken: "opaque-token",
            storeGeneration: 1,
            filter: filter,
            coverage: makeCoverage(filter.range),
            query: query,
            hits: hits,
            page: TimelinePageInfo(nextCursor: nil, returnedItems: UInt32(hits.count), truncated: false),
            provenance: makeProvenance()
        )
    }

    func chunkDetail(
        snapshotToken: String,
        reference: TimelineChunkReference,
        now: Date
    ) throws -> TimelineChunkDetailSnapshot {
        throw TimelineStubError(message: "unused")
    }

    func eventDetail(
        snapshotToken: String,
        eventID: String,
        now: Date
    ) throws -> TimelineEventDetailSnapshot {
        throw TimelineStubError(message: "unused")
    }

    func image(_ metadata: TimelineImageMetadata, maxBytes: UInt64) -> Data { Data() }
}

private struct TimelineStubError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

private actor CapturingTimelineCore: CoreService {
    let response: Data
    private var request: Data?
    private var reads = 0

    init(response: Data) {
        self.response = response
    }

    func openedStoreGeneration() -> UInt64 { 1 }
    func schemaIdentity() throws -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }
    func call(_ request: Data) -> Data {
        self.request = request
        return response
    }
    func ingest(_ request: Data, image: Data?) -> Data { Data() }
    func imageRead(artifactID: String, generation: UInt64, maxBytes: UInt64) -> Data {
        reads += 1
        return Data([0x01])
    }
    func close() {}
    func lastRequest() -> Data? { request }
    func imageReadCount() -> Int { reads }
}

private actor FailingTimelineCore: CoreService {
    let error: ChronicleBridgeError

    init(error: ChronicleBridgeError) {
        self.error = error
    }

    func openedStoreGeneration() -> UInt64 { 1 }
    func schemaIdentity() throws -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }
    func call(_ request: Data) throws -> Data { throw error }
    func ingest(_ request: Data, image: Data?) -> Data { Data() }
    func imageRead(artifactID: String, generation: UInt64, maxBytes: UInt64) -> Data { Data() }
    func close() {}
}

private actor SilentNotificationBackend: ChronicleNotificationDelivering {
    func authorizationState() -> ChronicleNotificationAuthorization { .denied }
    func requestAuthorization() -> Bool { false }
    func deliver(_ message: ChronicleNotificationMessage) {}
}

private func makePage(
    filter: TimelineFilter,
    cutoff: Date,
    chunks: [TimelineChunkBandSnapshot],
    nextCursor: String?
) -> TimelinePageSnapshot {
    TimelinePageSnapshot(
        schemaVersion: "1.0",
        generatedAt: cutoff,
        stableCutoff: cutoff,
        snapshotToken: "opaque-token",
        storeGeneration: 1,
        filter: filter,
        coverage: makeCoverage(filter.range),
        chunks: chunks,
        page: TimelinePageInfo(
            nextCursor: nextCursor,
            returnedItems: UInt32(chunks.count),
            truncated: nextCursor != nil
        ),
        domainContextAvailable: false,
        provenance: makeProvenance()
    )
}

private func makeChunkDetail(
    band: TimelineChunkBandSnapshot,
    token: String,
    now: Date
) -> TimelineChunkDetailSnapshot {
    TimelineChunkDetailSnapshot(
        schemaVersion: "1.0",
        generatedAt: now,
        stableCutoff: now,
        snapshotToken: token,
        storeGeneration: 1,
        chunk: TimelineChunkRevision(
            schemaVersion: "1.0",
            chunkID: band.chunkID,
            revisionID: band.revisionID,
            priorRevisionID: band.priorRevisionID,
            supersedesRevisionID: band.supersedesRevisionID,
            window: TimelineChunkWindow(start: band.start, end: band.end),
            generatedAt: band.generatedAt,
            displayTimezone: band.displayTimezone,
            aggregatorVersion: band.aggregatorVersion,
            inputDigest: band.inputDigest,
            storeGeneration: band.storeGeneration,
            finalizationCadenceSeconds: band.finalizationCadenceSeconds,
            evidenceSeconds: band.evidenceSeconds,
            presenceSeconds: band.presenceSeconds,
            durationEstimates: band.durationEstimates,
            transitions: band.transitions,
            ocrExtracts: [],
            gaps: band.gaps,
            supportingEventIDs: band.supportingEventIDs,
            lateInput: band.lateInput
        ),
        provenance: makeProvenance()
    )
}

private func makeEventDetail(
    eventID: String,
    token: String,
    now: Date
) -> TimelineEventDetailSnapshot {
    TimelineEventDetailSnapshot(
        schemaVersion: "1.0",
        generatedAt: now,
        stableCutoff: now,
        snapshotToken: token,
        storeGeneration: 1,
        event: TimelineEvent(
            eventID: eventID,
            deviceID: "device-1",
            scheduledAt: now,
            observedAt: now,
            recordedAt: now,
            displayTimezone: "UTC",
            source: TimelineEvidenceSource(adapter: "test", version: "1"),
            kind: "observation-attempt",
            payload: TimelineTaggedPayload(type: "observation-attempt", data: .object([:]))
        ),
        provenance: makeProvenance()
    )
}

private func makeDiagnosticHealth(lastProjectionAt: Date) -> DiagnosticHealthSnapshot {
    DiagnosticHealthSnapshot(
        schemaVersion: "1.0",
        observedAt: ChronicleTimestamp.string(lastProjectionAt),
        storeGeneration: 1,
        projection: .current,
        acknowledgement: .durable,
        latest: DiagnosticOperationTimes(
            lastScheduledAttemptAt: nil,
            lastSuccessfulCaptureAt: nil,
            lastSuccessfulOCRAt: nil,
            lastJournalAt: nil,
            lastProjectionAt: ChronicleTimestamp.string(lastProjectionAt),
            lastChunkAt: nil
        ),
        aggregationWatermark: nil,
        aggregationPendingBuckets: 0,
        projectionLagSeconds: 0,
        projectionPendingRecords: 0,
        storage: DiagnosticStorageSummary(
            managedBytes: 0,
            availableBytes: 100 * OperationalStoragePolicy.gibibyte
        ),
        study: DiagnosticStudySummary(
            state: .personal,
            start: nil,
            end: nil,
            expiredAt: nil
        ),
        screenshotRetention: DiagnosticScreenshotRetentionSummary(
            writePending: 0,
            retained: 0,
            deletePending: 0,
            expired: 0,
            userDeleted: 0,
            missing: 0,
            writeFailed: 0,
            nextExpiryAt: nil
        ),
        mcp: DiagnosticMCPHealthSummary(
            activeGrants: 0,
            revokedGrants: 0,
            expiredGrants: 0,
            exhaustedGrants: 0,
            staleGenerationGrants: 0
        ),
        issues: []
    )
}

private func makeCoverage(_ range: FactualReportRangePayload) -> FactualReportCoverage {
    let total = UInt32(range.end.timeIntervalSince(range.start))
    return FactualReportCoverage(
        range: range,
        evidenceSeconds: EvidenceSeconds(
            captured: min(300, total),
            protected: 0,
            paused: 0,
            unavailable: 0,
            error: 0,
            gap: total - min(300, total)
        ),
        presenceSeconds: PresenceSeconds(
            active: min(240, total),
            idle: total > 240 ? min(60, total - 240) : 0,
            unknown: 0
        ),
        gaps: []
    )
}

private func makeBand(
    chunkID: String,
    revisionID: String,
    start: Date
) -> TimelineChunkBandSnapshot {
    TimelineChunkBandSnapshot(
        chunkID: chunkID,
        revisionID: revisionID,
        priorRevisionID: nil,
        supersedesRevisionID: nil,
        start: start,
        end: start.addingTimeInterval(300),
        generatedAt: start.addingTimeInterval(300),
        displayTimezone: "UTC",
        aggregatorVersion: "test",
        inputDigest: "digest",
        storeGeneration: 1,
        finalizationCadenceSeconds: 60,
        evidenceSeconds: EvidenceSeconds(
            captured: 240,
            protected: 0,
            paused: 0,
            unavailable: 0,
            error: 0,
            gap: 60
        ),
        presenceSeconds: PresenceSeconds(active: 180, idle: 60, unknown: 0),
        durationEstimates: [],
        transitions: [],
        extracts: [],
        gaps: [],
        supportingEventIDs: [],
        lateInput: false
    )
}

private func makeProvenance() -> FactualReportProvenance {
    FactualReportProvenance(
        queryEngineVersion: "test",
        projectionBuildID: "test",
        sqliteVersion: "test",
        sqliteSourceID: "test",
        sourceEventIDs: [],
        sourceChunkRevisionIDs: []
    )
}

private func makeAnalysisArtifact(createdAt: Date) -> AnalysisArtifactSnapshot {
    AnalysisArtifactSnapshot(
        artifactID: "analysis-work-pattern",
        revisionID: "analysis-revision-1",
        priorRevisionID: nil,
        artifactType: "report",
        author: AnalysisAuthorIdentity(
            kind: "model",
            displayName: "Codex analysis",
            clientID: "client-codex",
            model: "gpt-5"
        ),
        createdAt: createdAt,
        status: "draft",
        payload: .object([
            "title": .string("Observed coordination pattern"),
            "body": .string("A derived interpretation linked to factual evidence."),
        ]),
        evidence: AnalysisEvidenceReferences(
            eventIDs: ["event-1"],
            chunkIDs: ["chunk-1"]
        ),
        confidence: 0.8,
        storeGeneration: 1
    )
}

private func makeAnalysisProvenance(
    artifact: AnalysisArtifactSnapshot
) -> AnalysisProvenance {
    AnalysisProvenance(
        queryEngineVersion: "test",
        projectionBuildID: "test",
        sqliteVersion: "test",
        sqliteSourceID: "test",
        sourceEventIDs: artifact.evidence.eventIDs,
        sourceChunkIDs: artifact.evidence.chunkIDs,
        sourceArtifactRevisionIDs: [artifact.revisionID]
    )
}

private func encodeEnvelope<Result: Codable & Sendable>(_ result: Result) throws -> Data {
    let encoder = JSONEncoder()
    encoder.dateEncodingStrategy = .custom { date, encoder in
        var container = encoder.singleValueContainer()
        try container.encode(ChronicleTimestamp.string(date))
    }
    return try encoder.encode(ChronicleEnvelope(
        schemaVersion: "1.0",
        ok: true,
        result: result,
        error: nil
    ))
}

private func timelineDate(_ value: String) throws -> Date {
    try XCTUnwrap(ChronicleTimestamp.date(value))
}
