import Foundation

protocol CoreService: Sendable {
    func openedStoreGeneration() async -> UInt64
    func schemaIdentity() async throws -> ChronicleSchemaIdentity
    func call(_ request: Data) async throws -> Data
    func ingest(_ request: Data, image: Data?) async throws -> Data
    func imageRead(artifactID: String, generation: UInt64, maxBytes: UInt64) async throws -> Data
    func close() async throws
}

actor InProcessCore: CoreService {
    private var handle: UInt64
    let storeGeneration: UInt64

    init(
        applicationSupportURL: URL,
        now: Date = Date(),
        aggregatorVersion: String = "chronicle-core-1",
        maxCadenceSeconds: UInt32 = 60
    ) throws {
        let request = OpenRequest(
            schemaVersion: "1.0",
            applicationSupportPath: applicationSupportURL.path,
            now: Self.timestamp(now),
            aggregatorVersion: aggregatorVersion,
            maxCadenceSeconds: maxCadenceSeconds
        )
        let encoded = try JSONEncoder().encode(request)
        let (handle, opened) = try ChronicleFFI.open(request: encoded)
        self.handle = handle
        storeGeneration = opened.storeGeneration
    }

    func openedStoreGeneration() -> UInt64 {
        storeGeneration
    }

    func schemaIdentity() throws -> ChronicleSchemaIdentity {
        _ = try requireOpen()
        return try ChronicleFFI.schemaIdentity()
    }

    func call(_ request: Data) throws -> Data {
        try ChronicleFFI.call(handle: requireOpen(), request: request)
    }

    func ingest(_ request: Data, image: Data?) throws -> Data {
        try ChronicleFFI.ingest(handle: requireOpen(), request: request, image: image)
    }

    func imageRead(
        artifactID: String,
        generation: UInt64,
        maxBytes: UInt64
    ) throws -> Data {
        let request = ImageRequest(
            schemaVersion: "1.0",
            artifactID: artifactID,
            storeGeneration: generation,
            maxBytes: maxBytes
        )
        return try ChronicleFFI.imageRead(
            handle: requireOpen(),
            request: JSONEncoder().encode(request)
        )
    }

    func close() throws {
        let closing = try requireOpen()
        try ChronicleFFI.close(handle: closing)
        handle = 0
    }

    private func requireOpen() throws -> UInt64 {
        guard handle != 0 else { throw ChronicleBridgeError.closed }
        return handle
    }

    private static func timestamp(_ date: Date) -> String {
        ISO8601DateFormatter().string(from: date)
    }
}

private struct OpenRequest: Encodable {
    let schemaVersion: String
    let applicationSupportPath: String
    let now: String
    let aggregatorVersion: String
    let maxCadenceSeconds: UInt32

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case applicationSupportPath = "application_support_path"
        case now
        case aggregatorVersion = "aggregator_version"
        case maxCadenceSeconds = "max_cadence_seconds"
    }
}

private struct ImageRequest: Encodable {
    let schemaVersion: String
    let artifactID: String
    let storeGeneration: UInt64
    let maxBytes: UInt64

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case artifactID = "artifact_id"
        case storeGeneration = "store_generation"
        case maxBytes = "max_bytes"
    }
}
