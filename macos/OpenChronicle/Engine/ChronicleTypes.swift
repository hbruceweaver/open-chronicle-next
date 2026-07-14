import Foundation

enum ChronicleBridgeError: Error, LocalizedError, Equatable {
    case bridgeStatus(UInt32, ChronicleErrorPayload?)
    case malformedResponse
    case schemaMismatch(expectedMajor: Int, actual: String)
    case closed

    var errorDescription: String? {
        switch self {
        case let .bridgeStatus(_, payload):
            payload?.message ?? "The Chronicle core rejected the operation."
        case .malformedResponse:
            "The Chronicle core returned a malformed response."
        case let .schemaMismatch(expected, actual):
            "Chronicle schema mismatch: expected major \(expected), received \(actual)."
        case .closed:
            "The Chronicle core is closed."
        }
    }
}

struct ChronicleErrorPayload: Codable, Equatable, Sendable {
    let code: String
    let message: String
    let retryable: Bool
}

struct ChronicleEnvelope<Result: Codable & Sendable>: Codable, Sendable {
    let schemaVersion: String
    let ok: Bool
    let result: Result?
    let error: ChronicleErrorPayload?

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case ok
        case result
        case error
    }

    func requireCompatibleMajor(_ expected: Int = 1) throws -> Result {
        guard Int(schemaVersion.split(separator: ".").first ?? "") == expected else {
            throw ChronicleBridgeError.schemaMismatch(
                expectedMajor: expected,
                actual: schemaVersion
            )
        }
        guard ok, let result else {
            throw ChronicleBridgeError.malformedResponse
        }
        return result
    }
}

struct ChronicleSchemaIdentity: Codable, Equatable, Sendable {
    let abiSchemaVersion: String
    let contractSchemaVersion: String

    enum CodingKeys: String, CodingKey {
        case abiSchemaVersion = "abi_schema_version"
        case contractSchemaVersion = "contract_schema_version"
    }
}

struct ChronicleOpenResult: Codable, Equatable, Sendable {
    let storeGeneration: UInt64

    enum CodingKeys: String, CodingKey {
        case storeGeneration = "store_generation"
    }
}

struct SharedServiceIdentity: Codable, Equatable, Sendable {
    let schemaVersion: String
    let requestID: String
    let storeGeneration: UInt64

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case requestID = "request_id"
        case storeGeneration = "store_generation"
    }
}

struct SharedQueryResponse<ResultData: Decodable & Sendable>: Decodable, Sendable {
    let schemaVersion: String
    let requestID: String
    let operation: String
    let storeGeneration: UInt64
    let page: SharedQueryPage?
    let result: SharedQueryResult<ResultData>

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case requestID = "request_id"
        case operation
        case storeGeneration = "store_generation"
        case page
        case result
    }
}

struct SharedQueryResult<ResultData: Decodable & Sendable>: Decodable, Sendable {
    let type: String
    let data: ResultData
}

struct SharedQueryPage: Decodable, Equatable, Sendable {
    let nextCursor: String?
    let returnedItems: UInt32
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case nextCursor = "next_cursor"
        case returnedItems = "returned_items"
        case truncated
    }
}

struct FactualStatisticsResult: Decodable, Equatable, Sendable {
    let factualTotals: [FactualTotalSummary]

    enum CodingKeys: String, CodingKey {
        case factualTotals = "factual_totals"
    }
}

struct FactualTotalSummary: Decodable, Equatable, Sendable {
    let dimension: String
    let key: String
    let estimatedSeconds: UInt32
    let supportingChunkIDs: [String]

    enum CodingKeys: String, CodingKey {
        case dimension
        case key
        case estimatedSeconds = "estimated_seconds"
        case supportingChunkIDs = "supporting_chunk_ids"
    }
}

struct ChunkListResult: Decodable, Equatable, Sendable {
    let chunks: [ChunkSummary]
}

struct ChunkSummary: Decodable, Equatable, Sendable {
    let chunkID: String
    let revisionID: String
    let evidenceSeconds: EvidenceSeconds
    let presenceSeconds: PresenceSeconds
    let lateInput: Bool

    enum CodingKeys: String, CodingKey {
        case chunkID = "chunk_id"
        case revisionID = "revision_id"
        case evidenceSeconds = "evidence_seconds"
        case presenceSeconds = "presence_seconds"
        case lateInput = "late_input"
    }
}

struct EvidenceSeconds: Decodable, Equatable, Sendable {
    let captured: UInt32
    let protected: UInt32
    let paused: UInt32
    let unavailable: UInt32
    let error: UInt32
    let gap: UInt32
}

struct PresenceSeconds: Decodable, Equatable, Sendable {
    let active: UInt32
    let idle: UInt32
    let unknown: UInt32
}

struct SearchResult: Decodable, Equatable, Sendable {
    let events: [QueryEventIdentity]
}

struct QueryEventIdentity: Decodable, Equatable, Sendable {
    let eventID: String

    enum CodingKeys: String, CodingKey {
        case eventID = "event_id"
    }
}

struct ChronicleHealthState: Equatable, Sendable {
    enum Status: Equatable, Sendable {
        case connecting
        case ready
        case repairRequired(String)
    }

    let status: Status
}
