import Foundation

struct FactualReportRange: Equatable, Sendable {
    let start: Date
    let end: Date

    var durationSeconds: UInt64 {
        UInt64(max(0, end.timeIntervalSince(start).rounded(.towardZero)))
    }
}

struct FactualReportSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let storeGeneration: UInt64
    let range: FactualReportRangePayload
    let coverage: FactualReportCoverage
    let factualTotals: [FactualReportTotal]
    let activityBuckets: [FactualReportActivityBucket]
    let transitions: [FactualReportTransition]
    let domainContextAvailable: Bool
    let provenance: FactualReportProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case storeGeneration = "store_generation"
        case range
        case coverage
        case factualTotals = "factual_totals"
        case activityBuckets = "activity_buckets"
        case transitions
        case domainContextAvailable = "domain_context_available"
        case provenance
    }
}

struct FactualReportRangePayload: Codable, Equatable, Sendable {
    let start: Date
    let end: Date

    var value: FactualReportRange { FactualReportRange(start: start, end: end) }
}

struct FactualReportCoverage: Codable, Equatable, Sendable {
    let range: FactualReportRangePayload
    let evidenceSeconds: EvidenceSeconds
    let presenceSeconds: PresenceSeconds
    let gaps: [FactualReportGap]

    enum CodingKeys: String, CodingKey {
        case range
        case evidenceSeconds = "evidence_seconds"
        case presenceSeconds = "presence_seconds"
        case gaps
    }
}

struct FactualReportGap: Codable, Equatable, Sendable, Identifiable {
    let start: Date
    let end: Date
    let kind: String
    let supportingEventIDs: [String]

    var id: String {
        "\(kind)-\(start.timeIntervalSince1970)-\(end.timeIntervalSince1970)"
    }

    enum CodingKeys: String, CodingKey {
        case start
        case end
        case kind
        case supportingEventIDs = "supporting_event_ids"
    }
}

struct FactualReportTotal: Codable, Equatable, Sendable, Identifiable {
    let dimension: String
    let key: String
    let label: String
    let parentKey: String?
    let estimatedSeconds: UInt32
    let supportingChunkIDs: [String]
    let supportingEventIDs: [String]

    var id: String { "\(dimension):\(key)" }

    enum CodingKeys: String, CodingKey {
        case dimension
        case key
        case label
        case parentKey = "parent_key"
        case estimatedSeconds = "estimated_seconds"
        case supportingChunkIDs = "supporting_chunk_ids"
        case supportingEventIDs = "supporting_event_ids"
    }
}

struct FactualReportActivityBucket: Codable, Equatable, Sendable, Identifiable {
    let chunkID: String
    let revisionID: String
    let start: Date
    let end: Date
    let evidenceSeconds: EvidenceSeconds
    let presenceSeconds: PresenceSeconds
    let durationEstimates: [FactualReportDurationEstimate]
    let gaps: [FactualReportGap]
    let transitions: [FactualReportTransition]
    let lateInput: Bool

    var id: String { chunkID }

    enum CodingKeys: String, CodingKey {
        case chunkID = "chunk_id"
        case revisionID = "revision_id"
        case start
        case end
        case evidenceSeconds = "evidence_seconds"
        case presenceSeconds = "presence_seconds"
        case durationEstimates = "duration_estimates"
        case gaps
        case transitions
        case lateInput = "late_input"
    }
}

struct FactualReportDurationEstimate: Codable, Equatable, Sendable, Identifiable {
    let dimension: String
    let key: String
    let label: String
    let estimatedSeconds: UInt32
    let supportingEventIDs: [String]

    var id: String { "\(dimension):\(key)" }

    enum CodingKeys: String, CodingKey {
        case dimension
        case key
        case label
        case estimatedSeconds = "estimated_seconds"
        case supportingEventIDs = "supporting_event_ids"
    }
}

struct FactualReportTransition: Codable, Equatable, Sendable, Identifiable {
    let at: Date
    let fromKey: String?
    let toKey: String
    let supportingEventID: String

    var id: String { supportingEventID }

    enum CodingKeys: String, CodingKey {
        case at
        case fromKey = "from_key"
        case toKey = "to_key"
        case supportingEventID = "supporting_event_id"
    }
}

struct FactualReportProvenance: Codable, Equatable, Sendable {
    let queryEngineVersion: String
    let projectionBuildID: String
    let sqliteVersion: String
    let sqliteSourceID: String
    let sourceEventIDs: [String]
    let sourceChunkRevisionIDs: [String]

    enum CodingKeys: String, CodingKey {
        case queryEngineVersion = "query_engine_version"
        case projectionBuildID = "projection_build_id"
        case sqliteVersion = "sqlite_version"
        case sqliteSourceID = "sqlite_source_id"
        case sourceEventIDs = "source_event_ids"
        case sourceChunkRevisionIDs = "source_chunk_revision_ids"
    }
}

protocol FactualReportQuerying: Sendable {
    func report(range: FactualReportRange, now: Date) async throws -> FactualReportSnapshot
}

actor CoreFactualReportClient: FactualReportQuerying {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func report(
        range: FactualReportRange,
        now: Date = Date()
    ) async throws -> FactualReportSnapshot {
        let generation = await core.openedStoreGeneration()
        let request = try JSONSerialization.data(withJSONObject: [
            "schema_version": "1.0",
            "now": ChronicleTimestamp.string(now),
            "control": [
                "type": "factual-report",
                "range": [
                    "start": ChronicleTimestamp.string(range.start),
                    "end": ChronicleTimestamp.string(range.end),
                ],
            ],
        ])
        let response = try await core.call(request)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let value = try decoder.singleValueContainer().decode(String.self)
            guard let date = ChronicleTimestamp.date(value) else {
                throw DecodingError.dataCorruptedError(
                    in: try decoder.singleValueContainer(),
                    debugDescription: "Chronicle timestamp is invalid."
                )
            }
            return date
        }
        let envelope = try decoder.decode(
            ChronicleEnvelope<FactualReportSnapshot>.self,
            from: response
        )
        let snapshot = try envelope.requireCompatibleMajor()
        try Self.validate(snapshot, requestedRange: range, generation: generation)
        return snapshot
    }

    private static func validate(
        _ snapshot: FactualReportSnapshot,
        requestedRange: FactualReportRange,
        generation: UInt64
    ) throws {
        let evidence = snapshot.coverage.evidenceSeconds
        let presence = snapshot.coverage.presenceSeconds
        let rangeSeconds = requestedRange.durationSeconds
        guard snapshot.schemaVersion.hasPrefix("1."),
              snapshot.storeGeneration == generation,
              snapshot.range.value == requestedRange,
              snapshot.coverage.range.value == requestedRange,
              snapshot.stableCutoff == snapshot.generatedAt,
              evidence.total == rangeSeconds,
              presence.total == UInt64(evidence.captured),
              snapshot.activityBuckets.allSatisfy({ bucket in
                  bucket.start >= requestedRange.start &&
                      bucket.end <= requestedRange.end &&
                      bucket.start < bucket.end &&
                      bucket.evidenceSeconds.total == UInt64(bucket.end.timeIntervalSince(bucket.start)) &&
                      bucket.presenceSeconds.total == UInt64(bucket.evidenceSeconds.captured)
              })
        else { throw ChronicleBridgeError.malformedResponse }
    }
}

extension EvidenceSeconds {
    var total: UInt64 {
        UInt64(captured) + UInt64(protected) + UInt64(paused) + UInt64(unavailable) +
            UInt64(error) + UInt64(gap)
    }
}

extension PresenceSeconds {
    var total: UInt64 {
        UInt64(active) + UInt64(idle) + UInt64(unknown)
    }
}
