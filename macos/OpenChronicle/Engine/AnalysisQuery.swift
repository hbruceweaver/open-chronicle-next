import Foundation

struct AnalysisAuthorIdentity: Codable, Equatable, Sendable {
    let kind: String
    let displayName: String?
    let clientID: String?
    let model: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case displayName = "display_name"
        case clientID = "client_id"
        case model
    }

    var label: String {
        displayName ?? model ?? clientID ?? kind.replacingOccurrences(of: "-", with: " ").capitalized
    }
}

struct AnalysisEvidenceReferences: Codable, Equatable, Sendable {
    let eventIDs: [String]
    let chunkIDs: [String]

    enum CodingKeys: String, CodingKey {
        case eventIDs = "event_ids"
        case chunkIDs = "chunk_ids"
    }
}

struct AnalysisArtifactSnapshot: Codable, Equatable, Identifiable, Sendable {
    let artifactID: String
    let revisionID: String
    let priorRevisionID: String?
    let artifactType: String
    let author: AnalysisAuthorIdentity
    let createdAt: Date
    let status: String
    let payload: TimelineJSONValue
    let evidence: AnalysisEvidenceReferences
    let confidence: Double?
    let storeGeneration: UInt64

    var id: String { revisionID }

    var title: String {
        for key in ["title", "name", "summary"] {
            if let value = payload.object?[key]?.string, !value.isEmpty { return value }
        }
        return artifactType.replacingOccurrences(of: "-", with: " ").capitalized
    }

    var body: String {
        for key in ["body", "summary", "description", "text"] {
            if let value = payload.object?[key]?.string, !value.isEmpty { return value }
        }
        guard let data = try? JSONEncoder().encode(payload),
              let object = try? JSONSerialization.jsonObject(with: data),
              let pretty = try? JSONSerialization.data(
                  withJSONObject: object,
                  options: [.prettyPrinted, .sortedKeys]
              )
        else { return "No displayable payload." }
        return String(data: pretty, encoding: .utf8) ?? "No displayable payload."
    }

    enum CodingKeys: String, CodingKey {
        case artifactID = "artifact_id"
        case revisionID = "revision_id"
        case priorRevisionID = "prior_revision_id"
        case artifactType = "artifact_type"
        case author
        case createdAt = "created_at"
        case status
        case payload
        case evidence
        case confidence
        case storeGeneration = "store_generation"
    }
}

struct AnalysisPageSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let range: FactualReportRangePayload
    let artifacts: [AnalysisArtifactSnapshot]
    let page: TimelinePageInfo
    let provenance: AnalysisProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case range
        case artifacts
        case page
        case provenance
    }
}

struct AnalysisDetailSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let artifact: AnalysisArtifactSnapshot
    let provenance: AnalysisProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case artifact
        case provenance
    }
}

struct AnalysisProvenance: Codable, Equatable, Sendable {
    let queryEngineVersion: String
    let projectionBuildID: String
    let sqliteVersion: String
    let sqliteSourceID: String
    let sourceEventIDs: [String]
    let sourceChunkIDs: [String]
    let sourceArtifactRevisionIDs: [String]

    enum CodingKeys: String, CodingKey {
        case queryEngineVersion = "query_engine_version"
        case projectionBuildID = "projection_build_id"
        case sqliteVersion = "sqlite_version"
        case sqliteSourceID = "sqlite_source_id"
        case sourceEventIDs = "source_event_ids"
        case sourceChunkIDs = "source_chunk_ids"
        case sourceArtifactRevisionIDs = "source_artifact_revision_ids"
    }
}

protocol AnalysisEvidenceQuerying: Sendable {
    func page(
        range: FactualReportRangePayload,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) async throws -> AnalysisPageSnapshot

    func detail(
        snapshotToken: String,
        artifactID: String,
        revisionID: String?,
        now: Date
    ) async throws -> AnalysisDetailSnapshot
}

actor CoreAnalysisEvidenceClient: AnalysisEvidenceQuerying {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func page(
        range: FactualReportRangePayload,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date = Date()
    ) async throws -> AnalysisPageSnapshot {
        let control: [String: Any] = [
            "type": "analysis-page",
            "stable_cutoff": Self.timestamp(stableCutoff),
            "snapshot_token": snapshotToken ?? NSNull(),
            "range": [
                "start": Self.timestamp(range.start),
                "end": Self.timestamp(range.end),
            ],
            "page": [
                "cursor": cursor ?? NSNull(),
                "limit": NSNumber(value: limit),
            ] as [String: Any],
        ]
        let result: AnalysisPageSnapshot = try await call(control: control, now: now)
        try await validate(
            schemaVersion: result.schemaVersion,
            token: result.snapshotToken,
            requestedToken: snapshotToken,
            storeGeneration: result.storeGeneration
        )
        guard result.stableCutoff == Self.wholeSecond(stableCutoff),
              result.range == range,
              result.artifacts.allSatisfy({ $0.storeGeneration == result.storeGeneration })
        else { throw ChronicleBridgeError.malformedResponse }
        return result
    }

    func detail(
        snapshotToken: String,
        artifactID: String,
        revisionID: String?,
        now: Date = Date()
    ) async throws -> AnalysisDetailSnapshot {
        let control: [String: Any] = [
            "type": "analysis-detail",
            "snapshot_token": snapshotToken,
            "artifact_id": artifactID,
            "revision_id": revisionID ?? NSNull(),
        ]
        let result: AnalysisDetailSnapshot = try await call(control: control, now: now)
        try await validate(
            schemaVersion: result.schemaVersion,
            token: result.snapshotToken,
            requestedToken: snapshotToken,
            storeGeneration: result.storeGeneration
        )
        guard result.artifact.artifactID == artifactID,
              revisionID == nil || result.artifact.revisionID == revisionID,
              result.artifact.storeGeneration == result.storeGeneration
        else { throw ChronicleBridgeError.malformedResponse }
        return result
    }

    private func call<Result: Codable & Sendable>(
        control: [String: Any],
        now: Date
    ) async throws -> Result {
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": ChronicleTimestamp.string(now),
            "control": control,
        ])
        do {
            let response = try await core.call(request)
            let envelope = try Self.decoder.decode(ChronicleEnvelope<Result>.self, from: response)
            return try envelope.requireCompatibleMajor()
        } catch let ChronicleBridgeError.bridgeStatus(_, payload)
            where payload?.code == "snapshot-no-longer-available"
        {
            throw TimelineQueryError.snapshotExpired
        } catch let ChronicleBridgeError.bridgeStatus(_, payload)
            where payload?.code == "projection-rebuilding"
        {
            throw TimelineQueryError.projectionRebuilding
        }
    }

    private func validate(
        schemaVersion: String,
        token: String,
        requestedToken: String?,
        storeGeneration: UInt64
    ) async throws {
        let openedGeneration = await core.openedStoreGeneration()
        guard schemaVersion.hasPrefix("1."),
              !token.isEmpty,
              requestedToken == nil || token == requestedToken,
              storeGeneration == openedGeneration
        else { throw ChronicleBridgeError.malformedResponse }
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: wholeSecond(date))
    }

    private static func wholeSecond(_ date: Date) -> Date {
        Date(timeIntervalSince1970: floor(date.timeIntervalSince1970))
    }

    private static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = ChronicleTimestamp.date(value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Chronicle timestamp is invalid."
                )
            }
            return date
        }
        return decoder
    }
}
