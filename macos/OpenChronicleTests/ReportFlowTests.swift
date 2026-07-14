import Foundation
import XCTest
@testable import OpenChronicle

final class ReportFlowTests: XCTestCase {
    @MainActor
    func testMetricCardsRouteToTypedIntervalsAndTotalsRouteEveryID() async throws {
        let start = try reportFlowDate("2026-07-14T09:00:00Z")
        let end = start.addingTimeInterval(300)
        let range = FactualReportRangePayload(start: start, end: end)
        let gap = FactualReportGap(
            start: start,
            end: end,
            kind: "missing-observation",
            supportingEventIDs: []
        )
        let snapshot = FactualReportSnapshot(
            schemaVersion: "1.0",
            generatedAt: end,
            stableCutoff: end,
            storeGeneration: 1,
            range: range,
            coverage: FactualReportCoverage(
                range: range,
                evidenceSeconds: EvidenceSeconds(
                    captured: 0,
                    protected: 0,
                    paused: 0,
                    unavailable: 0,
                    error: 0,
                    gap: 300
                ),
                presenceSeconds: PresenceSeconds(active: 0, idle: 0, unknown: 0),
                gaps: [gap]
            ),
            factualTotals: [
                FactualReportTotal(
                    dimension: "application",
                    key: "com.example.writer",
                    label: "Writer",
                    parentKey: nil,
                    estimatedSeconds: 120,
                    supportingChunkIDs: ["chunk-1", "chunk-2"],
                    supportingEventIDs: ["event-1", "event-2"]
                ),
            ],
            activityBuckets: [],
            transitions: [],
            domainContextAvailable: false,
            provenance: FactualReportProvenance(
                queryEngineVersion: "flow-test",
                projectionBuildID: "flow-test",
                sqliteVersion: "flow-test",
                sqliteSourceID: "flow-test",
                sourceEventIDs: ["event-1", "event-2"],
                sourceChunkRevisionIDs: ["revision-1", "revision-2"]
            )
        )
        let model = HomeViewModel(
            client: ReportFlowClient(snapshot: snapshot),
            calendar: Calendar(identifier: .gregorian),
            timeZone: try XCTUnwrap(TimeZone(identifier: "UTC")),
            nowProvider: { end.addingTimeInterval(60) }
        )
        await model.load(now: end.addingTimeInterval(60))

        let gapRoute = try XCTUnwrap(HomeEvidenceRouteBuilder.metric(
            title: "Evidence gaps",
            metric: .gap,
            model: model
        ))
        XCTAssertTrue(gapRoute.chunkIDs.isEmpty)
        XCTAssertTrue(gapRoute.eventIDs.isEmpty)
        XCTAssertEqual(gapRoute.intervals.map(\.state), ["gap"])
        XCTAssertEqual(gapRoute.intervals[0].start, start)
        XCTAssertEqual(gapRoute.intervals[0].end, end)

        let observedRoute = try XCTUnwrap(HomeEvidenceRouteBuilder.metric(
            title: "Observed computer time",
            metric: .observedComputerTime,
            model: model
        ))
        XCTAssertEqual(observedRoute.chunkIDs, ["chunk-1", "chunk-2"])
        XCTAssertEqual(observedRoute.eventIDs, ["event-1", "event-2"])

        let totalRoute = HomeEvidenceRouteBuilder.total(
            title: "Writer",
            chunkIDs: ["chunk-1", "chunk-2"],
            eventIDs: ["event-1", "event-2"]
        )
        XCTAssertEqual(totalRoute.chunkIDs.count, 2)
        XCTAssertEqual(totalRoute.eventIDs.count, 2)
    }
}

private actor ReportFlowClient: FactualReportQuerying {
    let snapshot: FactualReportSnapshot

    init(snapshot: FactualReportSnapshot) {
        self.snapshot = snapshot
    }

    func report(range: FactualReportRange, now: Date) -> FactualReportSnapshot {
        snapshot
    }
}

private func reportFlowDate(_ value: String) throws -> Date {
    try XCTUnwrap(ChronicleTimestamp.date(value))
}
