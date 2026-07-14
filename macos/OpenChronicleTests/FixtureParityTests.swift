import Foundation
import XCTest
@testable import OpenChronicle

final class FixtureParityTests: XCTestCase {
    func testRepresentativeV1ResponsesRetainExactIDsAndMetrics() throws {
        let url = try XCTUnwrap(
            Bundle(for: Self.self).url(
                forResource: "shared-response-v1",
                withExtension: "json"
            )
        )
        let decoded = try JSONDecoder().decode(
            FixtureResponseSet.self,
            from: Data(contentsOf: url)
        )

        let statistics = decoded.statisticsResponse
        XCTAssertEqual(statistics.schemaVersion, "1.0")
        XCTAssertEqual(statistics.requestID, "request-statistics-001")
        XCTAssertEqual(statistics.operation, "statistics")
        XCTAssertEqual(statistics.storeGeneration, 1)
        XCTAssertEqual(statistics.result.type, "statistics")
        XCTAssertEqual(
            statistics.result.data.factualTotals,
            [
                FactualTotalSummary(
                    dimension: "application",
                    key: "com.example.writer",
                    estimatedSeconds: 300,
                    supportingChunkIDs: ["ae4-chunk-0900"]
                ),
            ]
        )

        let chunks = decoded.chunkResponse
        XCTAssertEqual(chunks.schemaVersion, "1.0")
        XCTAssertEqual(chunks.requestID, "request-chunks-001")
        XCTAssertEqual(chunks.operation, "list-chunks")
        XCTAssertEqual(chunks.page?.returnedItems, 1)
        XCTAssertEqual(chunks.result.type, "chunk-list")
        let chunk = try XCTUnwrap(chunks.result.data.chunks.first)
        XCTAssertEqual(chunk.chunkID, "ae4-chunk-0900")
        XCTAssertEqual(chunk.revisionID, "ae4-chunk-rev-001")
        XCTAssertEqual(chunk.evidenceSeconds.captured, 300)
        XCTAssertEqual(chunk.presenceSeconds.active, 300)
        XCTAssertFalse(chunk.lateInput)

        let search = decoded.searchResponse
        XCTAssertEqual(search.schemaVersion, "1.0")
        XCTAssertEqual(search.requestID, "request-search-001")
        XCTAssertEqual(search.operation, "search-activity")
        XCTAssertEqual(search.page?.returnedItems, 1)
        XCTAssertEqual(search.result.type, "search")
        XCTAssertEqual(search.result.data.events.map(\.eventID), ["evt-090015"])
    }
}

private struct FixtureResponseSet: Decodable {
    let statisticsResponse: SharedQueryResponse<FactualStatisticsResult>
    let chunkResponse: SharedQueryResponse<ChunkListResult>
    let searchResponse: SharedQueryResponse<SearchResult>

    enum CodingKeys: String, CodingKey {
        case statisticsResponse = "statistics_response"
        case chunkResponse = "chunk_response"
        case searchResponse = "search_response"
    }
}
