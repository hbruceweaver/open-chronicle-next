import XCTest
@testable import OpenChronicle

final class HomeViewModelTests: XCTestCase {
    @MainActor
    func testMetricsAndBreakdownsPreserveRustFactsAndSupportingIDs() async throws {
        let snapshot = try makeSnapshot(domainAvailable: false)
        let client = StubFactualReportClient(snapshot: snapshot)
        let now = try date("2026-07-14T09:10:00Z")
        let model = HomeViewModel(
            client: client,
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { now }
        )

        await model.load(now: now)

        guard case let .loaded(loaded, partial) = model.state else {
            return XCTFail("Expected loaded report")
        }
        XCTAssertEqual(loaded, snapshot)
        XCTAssertTrue(partial)
        let metrics = try XCTUnwrap(model.metrics)
        XCTAssertEqual(metrics.expectedSeconds, 600)
        XCTAssertEqual(metrics.accountedSeconds, 420)
        XCTAssertEqual(metrics.capturedSeconds, 300)
        XCTAssertEqual(metrics.observedComputerSeconds, 240)
        XCTAssertEqual(metrics.idleSeconds, 60)
        XCTAssertEqual(metrics.coverageFraction, 0.7, accuracy: 0.0001)
        XCTAssertEqual(metrics.captureFraction, 0.5, accuracy: 0.0001)
        XCTAssertEqual(metrics.transitionCount, 1)
        XCTAssertEqual(metrics.dailyAverageSeconds, 240)
        XCTAssertEqual(model.applicationBreakdown.count, 1)
        XCTAssertEqual(model.applicationBreakdown[0].application.supportingChunkIDs, ["chunk-1"])
        XCTAssertEqual(model.applicationBreakdown[0].windows.map(\.parentKey), ["com.example.writer"])
        XCTAssertEqual(model.applicationBreakdown[0].windows[0].supportingEventIDs, ["event-1"])
        XCTAssertTrue(model.domainBreakdown.isEmpty)
        XCTAssertEqual(model.recentBuckets.map(\.chunkID), ["chunk-1"])
    }

    @MainActor
    func testFreshnessDoesNotRewriteRenderedSnapshotUntilRefresh() async throws {
        let snapshot = try makeSnapshot(domainAvailable: false)
        let now = try date("2026-07-14T09:10:00Z")
        let model = HomeViewModel(
            client: StubFactualReportClient(snapshot: snapshot),
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { now }
        )
        await model.load(now: now)
        let before = model.metrics

        model.observe(latestProjectionAt: snapshot.stableCutoff.addingTimeInterval(1))

        XCTAssertTrue(model.newDataAvailable)
        XCTAssertEqual(model.metrics, before)
        XCTAssertEqual(model.snapshot, snapshot)
        XCTAssertEqual(model.selectedRange, .today)
    }

    @MainActor
    func testEmptyPartialCoverageIsNotPresentedAsZeroEvidence() async throws {
        let populated = try makeSnapshot(domainAvailable: false)
        let empty = FactualReportSnapshot(
            schemaVersion: populated.schemaVersion,
            generatedAt: populated.generatedAt,
            stableCutoff: populated.stableCutoff,
            storeGeneration: populated.storeGeneration,
            range: populated.range,
            coverage: FactualReportCoverage(
                range: populated.range,
                evidenceSeconds: EvidenceSeconds(
                    captured: 0,
                    protected: 0,
                    paused: 0,
                    unavailable: 0,
                    error: 0,
                    gap: 600
                ),
                presenceSeconds: PresenceSeconds(active: 0, idle: 0, unknown: 0),
                gaps: populated.coverage.gaps
            ),
            factualTotals: [],
            activityBuckets: [],
            transitions: [],
            domainContextAvailable: false,
            provenance: populated.provenance
        )
        let now = try date("2026-07-14T09:10:00Z")
        let model = HomeViewModel(
            client: StubFactualReportClient(snapshot: empty),
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { now }
        )

        await model.load(now: now)

        guard case .empty = model.state else { return XCTFail("Expected explicit empty state") }
        XCTAssertEqual(model.metrics?.expectedSeconds, 600)
        XCTAssertEqual(model.metrics?.gapSeconds, 600)
        XCTAssertEqual(model.metrics?.coverageFraction, 0)
    }

    func testYesterdayUsesLocalCalendarDayAcrossDST() throws {
        let zone = try XCTUnwrap(TimeZone(identifier: "Europe/Zurich"))
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let now = try XCTUnwrap(calendar.date(from: DateComponents(
            year: 2026,
            month: 3,
            day: 30,
            hour: 12
        )))

        let range = try XCTUnwrap(HomeReportRangeBuilder.range(
            preset: .yesterday,
            customStart: now,
            customEnd: now,
            now: now,
            calendar: calendar,
            timeZone: zone
        ))

        XCTAssertEqual(range.durationSeconds, 23 * 60 * 60)
        XCTAssertEqual(Int(range.start.timeIntervalSince1970) % 300, 0)
        XCTAssertEqual(Int(range.end.timeIntervalSince1970) % 300, 0)
        XCTAssertEqual(calendar.component(.day, from: range.start), 29)
        XCTAssertEqual(calendar.component(.day, from: range.end), 30)
    }

    func testActivityPartitionShowsEveryEvidenceStateAndSumsToBucket() throws {
        let start = try date("2026-07-14T09:00:00Z")
        let bucket = FactualReportActivityBucket(
            chunkID: "chunk-complete-partition",
            revisionID: "revision-complete-partition",
            start: start,
            end: start.addingTimeInterval(300),
            evidenceSeconds: EvidenceSeconds(
                captured: 150,
                protected: 30,
                paused: 10,
                unavailable: 20,
                error: 10,
                gap: 80
            ),
            presenceSeconds: PresenceSeconds(active: 100, idle: 30, unknown: 20),
            durationEstimates: [
                FactualReportDurationEstimate(
                    dimension: "application",
                    key: "com.example.writer",
                    label: "Writer",
                    estimatedSeconds: 60,
                    supportingEventIDs: ["event-writer"]
                ),
            ],
            gaps: [],
            transitions: [],
            lateInput: false
        )

        let parts = HomeActivityPartition.parts(bucket)

        XCTAssertEqual(parts.reduce(UInt32(0)) { $0 + $1.seconds }, 300)
        XCTAssertEqual(seconds(for: .application("com.example.writer"), in: parts), 60)
        XCTAssertEqual(seconds(for: .idle, in: parts), 30)
        XCTAssertEqual(seconds(for: .capturedUnattributed, in: parts), 60)
        XCTAssertEqual(seconds(for: .protected, in: parts), 30)
        XCTAssertEqual(seconds(for: .paused, in: parts), 10)
        XCTAssertEqual(seconds(for: .unavailable, in: parts), 20)
        XCTAssertEqual(seconds(for: .error, in: parts), 10)
        XCTAssertEqual(seconds(for: .gap, in: parts), 80)
    }

    @MainActor
    func testEvidenceReferencesNeverCollapseMultiChunkOrEventTotals() {
        let total = FactualReportTotal(
            dimension: "application",
            key: "com.example.writer",
            label: "Writer",
            parentKey: nil,
            estimatedSeconds: 600,
            supportingChunkIDs: ["chunk-1", "chunk-2"],
            supportingEventIDs: ["event-1", "event-2", "event-3"]
        )
        let model = HomeViewModel()

        XCTAssertEqual(
            model.evidenceReferences(for: total),
            HomeEvidenceReferences(
                chunkIDs: ["chunk-1", "chunk-2"],
                eventIDs: ["event-1", "event-2", "event-3"]
            )
        )
    }

    @MainActor
    func testMetricEvidenceUsesTypedContributorsAndRepresentsIDLessGaps() async throws {
        let snapshot = try makeSnapshot(domainAvailable: false)
        let now = try date("2026-07-14T09:10:00Z")
        let model = HomeViewModel(
            client: StubFactualReportClient(snapshot: snapshot),
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { now }
        )
        await model.load(now: now)

        let observed = model.evidenceReferences(for: .observedComputerTime)
        XCTAssertEqual(observed.chunkIDs, ["chunk-1"])
        XCTAssertEqual(observed.eventIDs, ["event-1"])
        XCTAssertTrue(observed.intervals.isEmpty)

        let transitions = model.evidenceReferences(for: .transitions)
        XCTAssertEqual(transitions.chunkIDs, ["chunk-1"])
        XCTAssertEqual(transitions.eventIDs, ["event-transition"])

        let gaps = model.evidenceReferences(for: .gap)
        XCTAssertTrue(gaps.chunkIDs.isEmpty)
        XCTAssertTrue(gaps.eventIDs.isEmpty)
        XCTAssertEqual(gaps.intervals.count, 1)
        XCTAssertEqual(gaps.intervals[0].state, "gap")
        XCTAssertTrue(gaps.intervals[0].supportingEventIDs.isEmpty)
    }

    @MainActor
    func testSystemTimezoneChangeForcesRangeReloadEvenForSameZoneIdentity() async throws {
        let snapshot = try makeSnapshot(domainAvailable: false)
        let client = CountingFactualReportClient(snapshot: snapshot)
        let now = try date("2026-07-14T09:10:00Z")
        let utc = try XCTUnwrap(TimeZone(identifier: "UTC"))
        let model = HomeViewModel(
            client: client,
            calendar: Calendar(identifier: .gregorian),
            timeZone: utc,
            nowProvider: { now }
        )
        await model.load(now: now)
        let before = await client.requestCount()
        XCTAssertEqual(before, 1)

        await model.systemTimeZoneDidChange(to: utc)

        let after = await client.requestCount()
        XCTAssertEqual(after, 2)
        XCTAssertEqual(model.displayTimeZone.secondsFromGMT(for: now), 0)
    }

    func testCoreClientUsesAppPrivateControlWithoutGrantIdentity() async throws {
        let core = RequestCapturingCore()
        let client = CoreFactualReportClient(core: core)
        let range = FactualReportRange(
            start: try date("2026-07-14T09:00:00Z"),
            end: try date("2026-07-14T09:10:00Z")
        )

        do {
            _ = try await client.report(range: range, now: range.end)
            XCTFail("The deliberately incomplete response should fail")
        } catch {
            // The request shape is the assertion target.
        }

        let captured = await core.lastRequest()
        let request = try XCTUnwrap(captured)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: request) as? [String: Any]
        )
        let control = try XCTUnwrap(object["control"] as? [String: Any])
        XCTAssertEqual(control["type"] as? String, "factual-report")
        XCTAssertNotNil(control["range"])
        XCTAssertNil(object["client_id"])
        XCTAssertNil(object["grant_id"])
        XCTAssertNil(object["request"])
    }

    func testCoreClientDecodesRealFactualReportSnapshot() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("open-chronicle-home-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let start = try date("2026-07-14T09:00:00Z")
        let end = try date("2026-07-14T09:05:00Z")
        let now = try date("2026-07-14T09:07:00Z")
        let core = try InProcessCore(applicationSupportURL: root, now: now)
        let client = CoreFactualReportClient(core: core)

        let snapshot = try await client.report(
            range: FactualReportRange(start: start, end: end),
            now: now
        )
        try await core.close()

        XCTAssertEqual(snapshot.range.start, start)
        XCTAssertEqual(snapshot.range.end, end)
        XCTAssertEqual(snapshot.coverage.evidenceSeconds.total, 300)
        XCTAssertEqual(snapshot.coverage.evidenceSeconds.gap, 300)
        XCTAssertEqual(snapshot.coverage.presenceSeconds.total, 0)
        XCTAssertTrue(snapshot.factualTotals.isEmpty)
        XCTAssertTrue(snapshot.activityBuckets.isEmpty)
        XCTAssertFalse(snapshot.domainContextAvailable)
    }

    private func makeSnapshot(domainAvailable: Bool) throws -> FactualReportSnapshot {
        let start = try date("2026-07-14T09:00:00Z")
        let middle = try date("2026-07-14T09:05:00Z")
        let end = try date("2026-07-14T09:10:00Z")
        let range = FactualReportRangePayload(start: start, end: end)
        let eventIDs = ["event-1"]
        var totals = [
            FactualReportTotal(
                dimension: "application",
                key: "com.example.writer",
                label: "Writer",
                parentKey: nil,
                estimatedSeconds: 240,
                supportingChunkIDs: ["chunk-1"],
                supportingEventIDs: eventIDs
            ),
            FactualReportTotal(
                dimension: "window",
                key: "Draft",
                label: "Draft",
                parentKey: "com.example.writer",
                estimatedSeconds: 120,
                supportingChunkIDs: ["chunk-1"],
                supportingEventIDs: eventIDs
            ),
        ]
        totals.append(FactualReportTotal(
            dimension: "authorized-domain",
            key: "example.com",
            label: "example.com",
            parentKey: nil,
            estimatedSeconds: 30,
            supportingChunkIDs: ["chunk-1"],
            supportingEventIDs: eventIDs
        ))
        let gap = FactualReportGap(
            start: middle,
            end: end,
            kind: "missing-observation",
            supportingEventIDs: []
        )
        let transition = FactualReportTransition(
            at: start.addingTimeInterval(120),
            fromKey: nil,
            toKey: "com.example.writer",
            supportingEventID: "event-transition"
        )
        let bucket = FactualReportActivityBucket(
            chunkID: "chunk-1",
            revisionID: "revision-1",
            start: start,
            end: middle,
            evidenceSeconds: EvidenceSeconds(
                captured: 300,
                protected: 0,
                paused: 0,
                unavailable: 0,
                error: 0,
                gap: 0
            ),
            presenceSeconds: PresenceSeconds(active: 210, idle: 60, unknown: 30),
            durationEstimates: [
                FactualReportDurationEstimate(
                    dimension: "application",
                    key: "com.example.writer",
                    label: "Writer",
                    estimatedSeconds: 240,
                    supportingEventIDs: eventIDs
                ),
            ],
            gaps: [],
            transitions: [transition],
            lateInput: false
        )
        return FactualReportSnapshot(
            schemaVersion: "1.0",
            generatedAt: end,
            stableCutoff: end,
            storeGeneration: 1,
            range: range,
            coverage: FactualReportCoverage(
                range: range,
                evidenceSeconds: EvidenceSeconds(
                    captured: 300,
                    protected: 60,
                    paused: 30,
                    unavailable: 30,
                    error: 0,
                    gap: 180
                ),
                presenceSeconds: PresenceSeconds(active: 210, idle: 60, unknown: 30),
                gaps: [gap]
            ),
            factualTotals: totals,
            activityBuckets: [bucket],
            transitions: [transition],
            domainContextAvailable: domainAvailable,
            provenance: FactualReportProvenance(
                queryEngineVersion: "test",
                projectionBuildID: "test",
                sqliteVersion: "test",
                sqliteSourceID: "test",
                sourceEventIDs: ["event-1", "event-transition"],
                sourceChunkRevisionIDs: ["revision-1"]
            )
        )
    }

    private func seconds(
        for kind: HomeActivityPartKind,
        in parts: [HomeActivityPart]
    ) -> UInt32? {
        parts.first { $0.kind == kind }?.seconds
    }
}

private actor StubFactualReportClient: FactualReportQuerying {
    let snapshot: FactualReportSnapshot

    init(snapshot: FactualReportSnapshot) {
        self.snapshot = snapshot
    }

    func report(range: FactualReportRange, now: Date) -> FactualReportSnapshot {
        snapshot
    }
}

private actor CountingFactualReportClient: FactualReportQuerying {
    let snapshot: FactualReportSnapshot
    private var count = 0

    init(snapshot: FactualReportSnapshot) {
        self.snapshot = snapshot
    }

    func report(range: FactualReportRange, now: Date) -> FactualReportSnapshot {
        count += 1
        return snapshot
    }

    func requestCount() -> Int { count }
}

private actor RequestCapturingCore: CoreService {
    private var request: Data?

    func openedStoreGeneration() -> UInt64 { 1 }

    func schemaIdentity() throws -> ChronicleSchemaIdentity {
        ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }

    func call(_ request: Data) -> Data {
        self.request = request
        return Data("{\"schema_version\":\"1.0\",\"ok\":false}".utf8)
    }

    func ingest(_ request: Data, image: Data?) throws -> Data { Data() }

    func imageRead(
        artifactID: String,
        generation: UInt64,
        maxBytes: UInt64
    ) throws -> Data { Data() }

    func close() {}

    func lastRequest() -> Data? { request }
}

private func date(_ value: String) throws -> Date {
    try XCTUnwrap(ChronicleTimestamp.date(value))
}
