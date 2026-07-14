import Foundation

enum TimelineCoverageState: String, CaseIterable, Codable, Identifiable, Sendable {
    case captured
    case protected
    case paused
    case unavailable
    case error
    case missingObservation = "missing-observation"

    var id: String { rawValue }

    var title: String {
        switch self {
        case .captured: "Captured"
        case .protected: "Protected"
        case .paused: "Paused"
        case .unavailable: "Unavailable"
        case .error: "Error"
        case .missingObservation: "Missing observation"
        }
    }
}

struct TimelineFilter: Codable, Equatable, Sendable {
    let range: FactualReportRangePayload
    let applicationBundleID: String?
    let windowText: String?
    let authorizedDomain: String?
    let coverageStates: [TimelineCoverageState]

    enum CodingKeys: String, CodingKey {
        case range
        case applicationBundleID = "application_bundle_id"
        case windowText = "window_text"
        case authorizedDomain = "authorized_domain"
        case coverageStates = "coverage_states"
    }
}

struct TimelinePageInfo: Codable, Equatable, Sendable {
    let nextCursor: String?
    let returnedItems: UInt32
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case nextCursor = "next_cursor"
        case returnedItems = "returned_items"
        case truncated
    }
}

struct TimelineChunkExtract: Codable, Equatable, Sendable {
    let sourceEventID: String
    let characterCount: UInt32
    let untrustedEvidence: Bool

    enum CodingKeys: String, CodingKey {
        case sourceEventID = "source_event_id"
        case characterCount = "character_count"
        case untrustedEvidence = "untrusted_evidence"
    }
}

struct TimelineChunkBandSnapshot: Codable, Equatable, Identifiable, Sendable {
    let chunkID: String
    let revisionID: String
    let priorRevisionID: String?
    let supersedesRevisionID: String?
    let start: Date
    let end: Date
    let generatedAt: Date
    let displayTimezone: String
    let aggregatorVersion: String
    let inputDigest: String
    let storeGeneration: UInt64
    let finalizationCadenceSeconds: UInt32
    let evidenceSeconds: EvidenceSeconds
    let presenceSeconds: PresenceSeconds
    let durationEstimates: [FactualReportDurationEstimate]
    let transitions: [FactualReportTransition]
    let extracts: [TimelineChunkExtract]
    let gaps: [FactualReportGap]
    let supportingEventIDs: [String]
    let lateInput: Bool

    var id: String { revisionID }

    enum CodingKeys: String, CodingKey {
        case chunkID = "chunk_id"
        case revisionID = "revision_id"
        case priorRevisionID = "prior_revision_id"
        case supersedesRevisionID = "supersedes_revision_id"
        case start
        case end
        case generatedAt = "generated_at"
        case displayTimezone = "display_timezone"
        case aggregatorVersion = "aggregator_version"
        case inputDigest = "input_digest"
        case storeGeneration = "store_generation"
        case finalizationCadenceSeconds = "finalization_cadence_seconds"
        case evidenceSeconds = "evidence_seconds"
        case presenceSeconds = "presence_seconds"
        case durationEstimates = "duration_estimates"
        case transitions
        case extracts
        case gaps
        case supportingEventIDs = "supporting_event_ids"
        case lateInput = "late_input"
    }
}

struct TimelinePageSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let filter: TimelineFilter
    let coverage: FactualReportCoverage
    let chunks: [TimelineChunkBandSnapshot]
    let page: TimelinePageInfo
    let domainContextAvailable: Bool
    let provenance: FactualReportProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case filter
        case coverage
        case chunks
        case page
        case domainContextAvailable = "domain_context_available"
        case provenance
    }
}

struct TimelineAuthorizedDomainContext: Codable, Equatable, Sendable {
    let adapter: String
    let domain: String
}

struct TimelineWindowContext: Codable, Equatable, Sendable {
    let applicationBundleID: String
    let processName: String
    let windowTitle: String?
    let authorizedDomain: TimelineAuthorizedDomainContext?

    enum CodingKeys: String, CodingKey {
        case applicationBundleID = "application_bundle_id"
        case processName = "process_name"
        case windowTitle = "window_title"
        case authorizedDomain = "authorized_domain"
    }
}

struct TimelineHighlightRange: Codable, Equatable, Sendable {
    let start: UInt32
    let length: UInt32
}

struct TimelineSnippet: Codable, Equatable, Sendable {
    let text: String
    let highlights: [TimelineHighlightRange]

    struct Segment: Equatable, Sendable {
        let text: String
        let highlighted: Bool
    }

    /// Converts Rust Unicode-scalar offsets into inert display segments.
    /// Invalid or overlapping ranges are clipped and merged instead of indexing
    /// Swift strings by UTF-8 bytes or extended grapheme clusters.
    var segments: [Segment] {
        let scalars = Array(text.unicodeScalars)
        guard !scalars.isEmpty else { return [] }
        let bounded = highlights.compactMap { highlight -> Range<Int>? in
            let start = min(Int(highlight.start), scalars.count)
            let end = min(start + Int(highlight.length), scalars.count)
            return start < end ? start ..< end : nil
        }.sorted { lhs, rhs in
            lhs.lowerBound == rhs.lowerBound
                ? lhs.upperBound < rhs.upperBound
                : lhs.lowerBound < rhs.lowerBound
        }
        var merged: [Range<Int>] = []
        for range in bounded {
            if let last = merged.last, range.lowerBound <= last.upperBound {
                merged[merged.count - 1] = last.lowerBound ..< max(last.upperBound, range.upperBound)
            } else {
                merged.append(range)
            }
        }
        var result: [Segment] = []
        var cursor = 0
        for range in merged {
            if cursor < range.lowerBound {
                result.append(Segment(
                    text: String(String.UnicodeScalarView(scalars[cursor ..< range.lowerBound])),
                    highlighted: false
                ))
            }
            result.append(Segment(
                text: String(String.UnicodeScalarView(scalars[range])),
                highlighted: true
            ))
            cursor = range.upperBound
        }
        if cursor < scalars.count {
            result.append(Segment(
                text: String(String.UnicodeScalarView(scalars[cursor...])),
                highlighted: false
            ))
        }
        return result
    }
}

struct TimelineSearchHit: Codable, Equatable, Identifiable, Sendable {
    let eventID: String
    let observedAt: Date
    let context: TimelineWindowContext
    let evidenceState: String
    let presenceState: String
    let ocrState: String
    let snippet: TimelineSnippet?
    let untrustedEvidence: Bool

    var id: String { eventID }

    enum CodingKeys: String, CodingKey {
        case eventID = "event_id"
        case observedAt = "observed_at"
        case context
        case evidenceState = "evidence_state"
        case presenceState = "presence_state"
        case ocrState = "ocr_state"
        case snippet
        case untrustedEvidence = "untrusted_evidence"
    }
}

struct TimelineSearchSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let filter: TimelineFilter
    let coverage: FactualReportCoverage
    let query: String
    let hits: [TimelineSearchHit]
    let page: TimelinePageInfo
    let provenance: FactualReportProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case filter
        case coverage
        case query
        case hits
        case page
        case provenance
    }
}

struct TimelineChunkWindow: Codable, Equatable, Sendable {
    let start: Date
    let end: Date
}

struct TimelineOCRExtract: Codable, Equatable, Sendable {
    let text: String
    let sourceEventID: String
    let untrustedEvidence: Bool

    enum CodingKeys: String, CodingKey {
        case text
        case sourceEventID = "source_event_id"
        case untrustedEvidence = "untrusted_evidence"
    }
}

struct TimelineChunkRevision: Codable, Equatable, Sendable {
    let schemaVersion: String
    let chunkID: String
    let revisionID: String
    let priorRevisionID: String?
    let supersedesRevisionID: String?
    let window: TimelineChunkWindow
    let generatedAt: Date
    let displayTimezone: String
    let aggregatorVersion: String
    let inputDigest: String
    let storeGeneration: UInt64
    let finalizationCadenceSeconds: UInt32
    let evidenceSeconds: EvidenceSeconds
    let presenceSeconds: PresenceSeconds
    let durationEstimates: [FactualReportDurationEstimate]
    let transitions: [FactualReportTransition]
    let ocrExtracts: [TimelineOCRExtract]
    let gaps: [FactualReportGap]
    let supportingEventIDs: [String]
    let lateInput: Bool

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case chunkID = "chunk_id"
        case revisionID = "revision_id"
        case priorRevisionID = "prior_revision_id"
        case supersedesRevisionID = "supersedes_revision_id"
        case window
        case generatedAt = "generated_at"
        case displayTimezone = "display_timezone"
        case aggregatorVersion = "aggregator_version"
        case inputDigest = "input_digest"
        case storeGeneration = "store_generation"
        case finalizationCadenceSeconds = "finalization_cadence_seconds"
        case evidenceSeconds = "evidence_seconds"
        case presenceSeconds = "presence_seconds"
        case durationEstimates = "duration_estimates"
        case transitions
        case ocrExtracts = "ocr_extracts"
        case gaps
        case supportingEventIDs = "supporting_event_ids"
        case lateInput = "late_input"
    }
}

indirect enum TimelineJSONValue: Codable, Equatable, Sendable {
    case object([String: TimelineJSONValue])
    case array([TimelineJSONValue])
    case string(String)
    case number(Double)
    case boolean(Bool)
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .boolean(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([String: TimelineJSONValue].self) {
            self = .object(value)
        } else if let value = try? container.decode([TimelineJSONValue].self) {
            self = .array(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Unsupported timeline JSON value."
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case let .object(value): try container.encode(value)
        case let .array(value): try container.encode(value)
        case let .string(value): try container.encode(value)
        case let .number(value): try container.encode(value)
        case let .boolean(value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    var object: [String: TimelineJSONValue]? {
        guard case let .object(value) = self else { return nil }
        return value
    }

    var string: String? {
        guard case let .string(value) = self else { return nil }
        return value
    }
}

struct TimelineTaggedPayload: Codable, Equatable, Sendable {
    let type: String
    let data: TimelineJSONValue
}

struct TimelineEvidenceSource: Codable, Equatable, Sendable {
    let adapter: String
    let version: String
}

struct TimelineImageMetadata: Codable, Equatable, Identifiable, Sendable {
    let artifactID: String
    let state: String
    let expiresAt: Date?

    var id: String { artifactID }
    var isRetained: Bool { state == "retained" }

    enum CodingKeys: String, CodingKey {
        case artifactID = "artifact_id"
        case state
        case expiresAt = "expires_at"
    }
}

struct TimelineEvent: Codable, Equatable, Sendable {
    let eventID: String
    let deviceID: String
    let scheduledAt: Date?
    let observedAt: Date
    let recordedAt: Date
    let displayTimezone: String
    let source: TimelineEvidenceSource
    let kind: String
    let payload: TimelineTaggedPayload

    enum CodingKeys: String, CodingKey {
        case eventID = "event_id"
        case deviceID = "device_id"
        case scheduledAt = "scheduled_at"
        case observedAt = "observed_at"
        case recordedAt = "recorded_at"
        case displayTimezone = "display_timezone"
        case source
        case kind
        case payload
    }

    var observationData: [String: TimelineJSONValue]? {
        guard payload.type == "observation-attempt" else { return nil }
        return payload.data.object
    }

    var contentData: [String: TimelineJSONValue]? {
        observationData?["content"]?.object?["data"]?.object
    }

    var ocrText: String? {
        contentData?["ocr"]?.object?["text"]?.string
    }

    var image: TimelineImageMetadata? {
        guard let value = contentData?["image"], value != .null,
              let data = try? JSONEncoder().encode(value)
        else { return nil }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = Self.dateDecodingStrategy
        return try? decoder.decode(TimelineImageMetadata.self, from: data)
    }

    var context: TimelineWindowContext? {
        guard let value = contentData?["context"],
              let data = try? JSONEncoder().encode(value)
        else { return nil }
        return try? JSONDecoder().decode(TimelineWindowContext.self, from: data)
    }

    private static var dateDecodingStrategy: JSONDecoder.DateDecodingStrategy {
        .custom { decoder in
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
    }
}

struct TimelineChunkDetailSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let chunk: TimelineChunkRevision
    let provenance: FactualReportProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case chunk
        case provenance
    }
}

struct TimelineEventDetailSnapshot: Codable, Equatable, Sendable {
    let schemaVersion: String
    let generatedAt: Date
    let stableCutoff: Date
    let snapshotToken: String
    let storeGeneration: UInt64
    let event: TimelineEvent
    let provenance: FactualReportProvenance

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case stableCutoff = "stable_cutoff"
        case snapshotToken = "snapshot_token"
        case storeGeneration = "store_generation"
        case event
        case provenance
    }
}

enum TimelineQueryError: Error, LocalizedError, Equatable {
    case projectionRebuilding
    case snapshotExpired
    case missingSnapshot
    case imageUnavailable(String)

    var errorDescription: String? {
        switch self {
        case .projectionRebuilding:
            "The local evidence index is rebuilding. Captured evidence remains safe."
        case .snapshotExpired:
            "This frozen evidence snapshot is no longer available. Refresh to establish a new snapshot."
        case .missingSnapshot:
            "Load a timeline snapshot before opening evidence details."
        case let .imageUnavailable(state):
            "The local screenshot is \(state.replacingOccurrences(of: "-", with: " "))."
        }
    }
}

enum TimelineChunkReference: Hashable, Sendable {
    case revision(String)
    case logicalChunk(String)
}

protocol TimelineEvidenceQuerying: Sendable {
    func page(
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) async throws -> TimelinePageSnapshot

    func search(
        filter: TimelineFilter,
        query: String,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date
    ) async throws -> TimelineSearchSnapshot

    func chunkDetail(
        snapshotToken: String,
        reference: TimelineChunkReference,
        now: Date
    ) async throws -> TimelineChunkDetailSnapshot

    func eventDetail(
        snapshotToken: String,
        eventID: String,
        now: Date
    ) async throws -> TimelineEventDetailSnapshot

    func image(_ metadata: TimelineImageMetadata, maxBytes: UInt64) async throws -> Data
}

actor CoreTimelineEvidenceClient: TimelineEvidenceQuerying {
    private let core: any CoreService

    init(core: any CoreService) {
        self.core = core
    }

    func page(
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date = Date()
    ) async throws -> TimelinePageSnapshot {
        let control = try baseControl(
            type: "timeline-page",
            filter: filter,
            stableCutoff: stableCutoff,
            snapshotToken: snapshotToken,
            cursor: cursor,
            limit: limit
        )
        let snapshot: TimelinePageSnapshot = try await call(control: control, now: now)
        try await validate(
            schemaVersion: snapshot.schemaVersion,
            token: snapshot.snapshotToken,
            requestedToken: snapshotToken,
            stableCutoff: snapshot.stableCutoff,
            requestedCutoff: stableCutoff,
            storeGeneration: snapshot.storeGeneration
        )
        guard snapshot.filter == filter,
              snapshot.coverage.range == filter.range,
              snapshot.chunks.allSatisfy({ chunk in
                  chunk.start < chunk.end &&
                      chunk.storeGeneration == snapshot.storeGeneration &&
                      chunk.evidenceSeconds.total == UInt64(chunk.end.timeIntervalSince(chunk.start)) &&
                      chunk.presenceSeconds.total == UInt64(chunk.evidenceSeconds.captured) &&
                      chunk.extracts.allSatisfy(\.untrustedEvidence)
              })
        else { throw ChronicleBridgeError.malformedResponse }
        return snapshot
    }

    func search(
        filter: TimelineFilter,
        query: String,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32,
        now: Date = Date()
    ) async throws -> TimelineSearchSnapshot {
        var control = try baseControl(
            type: "timeline-search",
            filter: filter,
            stableCutoff: stableCutoff,
            snapshotToken: snapshotToken,
            cursor: cursor,
            limit: limit
        )
        control["query"] = query
        let snapshot: TimelineSearchSnapshot = try await call(control: control, now: now)
        try await validate(
            schemaVersion: snapshot.schemaVersion,
            token: snapshot.snapshotToken,
            requestedToken: snapshotToken,
            stableCutoff: snapshot.stableCutoff,
            requestedCutoff: stableCutoff,
            storeGeneration: snapshot.storeGeneration
        )
        guard snapshot.filter == filter,
              snapshot.coverage.range == filter.range,
              snapshot.query == query,
              snapshot.hits.allSatisfy({ $0.untrustedEvidence })
        else { throw ChronicleBridgeError.malformedResponse }
        return snapshot
    }

    func chunkDetail(
        snapshotToken: String,
        reference: TimelineChunkReference,
        now: Date = Date()
    ) async throws -> TimelineChunkDetailSnapshot {
        let revisionID: Any
        let chunkID: Any
        switch reference {
        case let .revision(value):
            revisionID = value
            chunkID = NSNull()
        case let .logicalChunk(value):
            revisionID = NSNull()
            chunkID = value
        }
        let control: [String: Any] = [
            "type": "timeline-chunk-detail",
            "snapshot_token": snapshotToken,
            "revision_id": revisionID,
            "chunk_id": chunkID,
        ]
        let snapshot: TimelineChunkDetailSnapshot = try await call(control: control, now: now)
        try await validateDetail(
            schemaVersion: snapshot.schemaVersion,
            token: snapshot.snapshotToken,
            requestedToken: snapshotToken,
            storeGeneration: snapshot.storeGeneration
        )
        guard (reference == .revision(snapshot.chunk.revisionID) ||
                reference == .logicalChunk(snapshot.chunk.chunkID)),
              snapshot.chunk.storeGeneration == snapshot.storeGeneration,
              snapshot.chunk.ocrExtracts.allSatisfy(\.untrustedEvidence)
        else { throw ChronicleBridgeError.malformedResponse }
        return snapshot
    }

    func eventDetail(
        snapshotToken: String,
        eventID: String,
        now: Date = Date()
    ) async throws -> TimelineEventDetailSnapshot {
        let control: [String: Any] = [
            "type": "timeline-event-detail",
            "snapshot_token": snapshotToken,
            "event_id": eventID,
        ]
        let snapshot: TimelineEventDetailSnapshot = try await call(control: control, now: now)
        try await validateDetail(
            schemaVersion: snapshot.schemaVersion,
            token: snapshot.snapshotToken,
            requestedToken: snapshotToken,
            storeGeneration: snapshot.storeGeneration
        )
        guard snapshot.event.eventID == eventID else {
            throw ChronicleBridgeError.malformedResponse
        }
        return snapshot
    }

    func image(_ metadata: TimelineImageMetadata, maxBytes: UInt64) async throws -> Data {
        guard metadata.isRetained else { throw TimelineQueryError.imageUnavailable(metadata.state) }
        guard maxBytes > 0, maxBytes <= 4 * 1_024 * 1_024 else {
            throw ChronicleBridgeError.malformedResponse
        }
        return try await core.imageRead(
            artifactID: metadata.artifactID,
            generation: await core.openedStoreGeneration(),
            maxBytes: maxBytes
        )
    }

    private func baseControl(
        type: String,
        filter: TimelineFilter,
        stableCutoff: Date,
        snapshotToken: String?,
        cursor: String?,
        limit: UInt32
    ) throws -> [String: Any] {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(Self.timestamp(date))
        }
        let filterData = try encoder.encode(filter)
        let filterObject = try JSONSerialization.jsonObject(with: filterData)
        return [
            "type": type,
            "stable_cutoff": Self.timestamp(stableCutoff),
            "snapshot_token": snapshotToken ?? NSNull(),
            "filter": filterObject,
            "page": [
                "cursor": cursor ?? NSNull(),
                "limit": NSNumber(value: limit),
            ] as [String: Any],
        ]
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
            where payload?.code == "projection-rebuilding" || payload?.code == "projection-stale"
        {
            throw TimelineQueryError.projectionRebuilding
        }
    }

    private func validate(
        schemaVersion: String,
        token: String,
        requestedToken: String?,
        stableCutoff: Date,
        requestedCutoff: Date,
        storeGeneration: UInt64
    ) async throws {
        let openedGeneration = await core.openedStoreGeneration()
        guard schemaVersion.hasPrefix("1."),
              !token.isEmpty,
              requestedToken == nil || requestedToken == token,
              stableCutoff == requestedCutoff,
              storeGeneration == openedGeneration
        else { throw ChronicleBridgeError.malformedResponse }
    }

    private func validateDetail(
        schemaVersion: String,
        token: String,
        requestedToken: String,
        storeGeneration: UInt64
    ) async throws {
        let openedGeneration = await core.openedStoreGeneration()
        guard schemaVersion.hasPrefix("1."),
              token == requestedToken,
              storeGeneration == openedGeneration
        else { throw ChronicleBridgeError.malformedResponse }
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: Date(
            timeIntervalSince1970: floor(date.timeIntervalSince1970)
        ))
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
