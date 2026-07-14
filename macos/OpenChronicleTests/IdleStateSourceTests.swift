import XCTest
@testable import OpenChronicle

final class IdleStateSourceTests: XCTestCase {
    func testBelowThresholdIsActive() {
        XCTAssertEqual(
            IdleStateSource(reader: FixedIdleReader(value: 299.9))
                .sample(thresholdSeconds: 300),
            .active
        )
    }

    func testAtThresholdStoresOnlyAggregateFloorSeconds() {
        XCTAssertEqual(
            IdleStateSource(reader: FixedIdleReader(value: 301.9))
                .sample(thresholdSeconds: 300),
            .idle(seconds: 301)
        )
    }

    func testMissingOrInvalidConfigurationIsUnknown() {
        XCTAssertEqual(
            IdleStateSource(reader: FixedIdleReader(value: nil))
                .sample(thresholdSeconds: 300),
            .unknown
        )
        XCTAssertEqual(
            IdleStateSource(reader: FixedIdleReader(value: 10))
                .sample(thresholdSeconds: 0),
            .unknown
        )
    }
}
