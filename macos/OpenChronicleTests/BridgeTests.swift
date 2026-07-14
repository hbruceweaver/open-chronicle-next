import Foundation
import XCTest
@testable import OpenChronicle

final class BridgeTests: XCTestCase {
    func testSchemaIdentityDecodesAndMatchesV1() throws {
        let identity = try ChronicleFFI.schemaIdentity()
        XCTAssertEqual(identity.abiSchemaVersion, "1.0")
        XCTAssertEqual(identity.contractSchemaVersion, "1.0")
    }

    func testSchemaMismatchBecomesRepairHealth() throws {
        let data = Data(#"{"schema_version":"2.0","ok":true,"result":{"abi_schema_version":"2.0","contract_schema_version":"2.0"}}"#.utf8)
        let envelope = try ChronicleFFI.decodeEnvelope(ChronicleSchemaIdentity.self, from: data)
        XCTAssertThrowsError(try envelope.requireCompatibleMajor()) { error in
            XCTAssertEqual(
                error as? ChronicleBridgeError,
                .schemaMismatch(expectedMajor: 1, actual: "2.0")
            )
        }
    }

    func testRealCoreHealthCallPreservesRequestAndGeneration() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let core = try InProcessCore(
            applicationSupportURL: directory,
            now: try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-07-13T09:00:00Z")),
            aggregatorVersion: "swift-bridge-test-1"
        )
        let generation = await core.storeGeneration
        let requestID = "req-swift-health"
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": "2026-07-13T09:00:01Z",
            "request": [
                "schema_version": "1.0",
                "request_id": requestID,
                "store_generation": NSNumber(value: generation),
                "operation": ["type": "health"],
            ],
        ])
        let response = try await core.call(request)
        let envelope = try ChronicleFFI.decodeEnvelope(SharedServiceIdentity.self, from: response)
        let identity = try envelope.requireCompatibleMajor()
        XCTAssertEqual(identity.requestID, requestID)
        XCTAssertEqual(identity.storeGeneration, generation)
        try await core.close()
    }

    @MainActor
    func testConcurrentConnectSharesOneOpenAndRetainsIt() async {
        let factory = CoreFactoryProbe()
        let model = AppModel(coreFactory: { _ in
            try await factory.open()
        })

        let first = Task { await model.connect() }
        let second = Task { await model.connect() }
        await first.value
        await second.value

        await model.connect()
        let openCount = await factory.openCount
        XCTAssertEqual(openCount, 1)
        XCTAssertEqual(model.health.status, .ready)
    }

    @MainActor
    func testConnectClosesAnOpenedCoreWhenIdentityFails() async {
        let core = ProbeCore(identityFails: true)
        let model = AppModel(coreFactory: { _ in core })

        await model.connect()

        let closeCount = await core.closeCount
        XCTAssertEqual(closeCount, 1)
        guard case .repairRequired = model.health.status else {
            return XCTFail("identity failure must surface repair health")
        }
    }
}

private actor CoreFactoryProbe {
    private(set) var openCount = 0
    private let core = ProbeCore()

    func open() async throws -> any CoreService {
        openCount += 1
        try await Task.sleep(nanoseconds: 100_000_000)
        return core
    }
}

private actor ProbeCore: CoreService {
    private let identityFails: Bool
    private(set) var closeCount = 0

    init(identityFails: Bool = false) {
        self.identityFails = identityFails
    }

    func openedStoreGeneration() -> UInt64 { 1 }

    func schemaIdentity() throws -> ChronicleSchemaIdentity {
        if identityFails {
            throw ChronicleBridgeError.malformedResponse
        }
        return ChronicleSchemaIdentity(abiSchemaVersion: "1.0", contractSchemaVersion: "1.0")
    }

    func call(_: Data) throws -> Data { Data() }
    func ingest(_: Data, image _: Data?) throws -> Data { Data() }

    func imageRead(artifactID _: String, generation _: UInt64, maxBytes _: UInt64) throws -> Data {
        Data()
    }

    func close() {
        closeCount += 1
    }
}
