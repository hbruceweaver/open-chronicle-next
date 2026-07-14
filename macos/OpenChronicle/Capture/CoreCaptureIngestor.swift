import Foundation

struct CaptureEventMetadata: Equatable, Sendable {
    let eventID: String
    let lifecycleEventID: String
    let imageArtifactID: String
    let deviceID: String
    let scheduledAt: Date
    let observedAt: Date
    let recordedAt: Date
    let displayTimezone: String
    let sourceVersion: String
    let cadenceSeconds: UInt32
    let bootSequence: String
    let monotonicTick: UInt64
    let screenshotExpiresAt: Date
}

protocol CaptureEventMetadataProviding: Sendable {
    func nextMetadata() async -> CaptureEventMetadata
}

actor SystemCaptureEventMetadataSource: CaptureEventMetadataProviding {
    private let deviceID: String
    private let cadenceSeconds: UInt32
    private let retentionSeconds: TimeInterval
    private let bootSequence = "boot-\(UUID().uuidString.lowercased())"
    private var monotonicTick: UInt64 = 0

    init(
        deviceID: String,
        cadenceSeconds: UInt32,
        retentionSeconds: TimeInterval
    ) {
        self.deviceID = deviceID
        self.cadenceSeconds = cadenceSeconds
        self.retentionSeconds = retentionSeconds
    }

    func nextMetadata() -> CaptureEventMetadata {
        monotonicTick &+= 1
        let now = Date()
        let token = UUID().uuidString.lowercased()
        return CaptureEventMetadata(
            eventID: "event-\(token)",
            lifecycleEventID: "lifecycle-\(UUID().uuidString.lowercased())",
            imageArtifactID: "image-\(UUID().uuidString.lowercased())",
            deviceID: deviceID,
            scheduledAt: now,
            observedAt: now,
            recordedAt: now,
            displayTimezone: TimeZone.current.identifier,
            sourceVersion: "macos-capture-1",
            cadenceSeconds: cadenceSeconds,
            bootSequence: bootSequence,
            monotonicTick: monotonicTick,
            screenshotExpiresAt: now.addingTimeInterval(retentionSeconds)
        )
    }
}

enum CoreCaptureIngestorError: Error {
    case malformedRecord
    case coreRejected(ChronicleErrorPayload?)
}

actor CoreCaptureIngestor: CaptureIngesting {
    private let core: any CoreService
    private let metadata: any CaptureEventMetadataProviding
    private let privacyPolicyVersion: String

    init(
        core: any CoreService,
        metadata: any CaptureEventMetadataProviding,
        privacyPolicyVersion: String = CapturePrivacyPolicy.default.policyVersion
    ) {
        self.core = core
        self.metadata = metadata
        self.privacyPolicyVersion = privacyPolicyVersion
    }

    func ingest(
        record: CaptureIngestRecord,
        image: Data?
    ) async throws -> CaptureIngestAcknowledgement {
        let metadata = await metadata.nextMetadata()
        let built = try Self.build(
            record: record,
            image: image,
            metadata: metadata,
            privacyPolicyVersion: privacyPolicyVersion
        )
        let envelope: [String: Any] = [
            "schema_version": "1.0",
            "now": Self.timestamp(metadata.recordedAt),
            "cadence": [
                "boot_sequence": metadata.bootSequence,
                "monotonic_tick": metadata.monotonicTick,
            ],
            "event": built.event,
            "completion": built.completion ?? NSNull(),
        ]
        let request = try JSONSerialization.data(
            withJSONObject: envelope,
            options: [.sortedKeys]
        )
        let response = try await core.ingest(request, image: image.map { Data($0) })
        let decoded = try JSONDecoder().decode(CoreIngestEnvelope.self, from: response)
        guard decoded.ok, let result = decoded.result else {
            throw CoreCaptureIngestorError.coreRejected(decoded.error)
        }
        let durability: CaptureDurability
        switch result.acknowledgement {
        case "durable": durability = .durable
        case "journal-durable-projection-pending":
            durability = .journalDurableProjectionPending
        default: durability = .notDurable
        }
        return CaptureIngestAcknowledgement(
            durability: durability,
            eventID: metadata.eventID,
            ocrEventID: built.hasOCR ? metadata.eventID : nil,
            imageArtifactID: image == nil ? nil : metadata.imageArtifactID
        )
    }

    private struct BuiltRecord {
        let event: [String: Any]
        let completion: [String: Any]?
        let hasOCR: Bool
    }

    private static func build(
        record: CaptureIngestRecord,
        image: Data?,
        metadata: CaptureEventMetadata,
        privacyPolicyVersion: String
    ) throws -> BuiltRecord {
        let attempt: [String: Any]
        var completion: [String: Any]?
        var hasOCR = false
        switch record {
        case let .denied(reason, presence):
            let axes = deniedAxes(reason)
            let content: [String: Any]
            if axes.protected {
                content = [
                    "type": "protected",
                    "data": [
                        "reason": reason.rawValue,
                        "privacy_policy_version": privacyPolicyVersion,
                    ],
                ]
            } else {
                content = [
                    "type": "no-evidence",
                    "data": ["reason": reason.rawValue],
                ]
            }
            attempt = attemptObject(
                metadata: metadata,
                attemptStatus: "skipped",
                evidenceState: axes.evidenceState,
                presence: presenceForDenial(reason, fallback: presence),
                ocrState: "not-run",
                content: content
            )
        case let .captureFailed(presence):
            attempt = attemptObject(
                metadata: metadata,
                attemptStatus: "failed",
                evidenceState: "capture-failed",
                presence: presence,
                ocrState: "not-run",
                content: [
                    "type": "no-evidence",
                    "data": ["reason": "capture-api-failure"],
                ]
            )
        case let .unchanged(context, contentHash, previous, presence):
            attempt = attemptObject(
                metadata: metadata,
                attemptStatus: "completed",
                evidenceState: "captured-unchanged",
                presence: presence,
                ocrState: "not-run",
                content: [
                    "type": "unchanged",
                    "data": [
                        "context": contextObject(context),
                        "content_hash": contentHash,
                        "previous_event_id": previous.eventID,
                        "reused_ocr_event_id": nullable(previous.ocrEventID),
                        "image_artifact_id": nullable(previous.imageArtifactID),
                    ],
                ]
            )
        case let .changed(context, contentHash, ocrChange, ocr, dimensions, presence):
            let ocrObject = ocrPayload(ocr, change: ocrChange)
            hasOCR = ocrObject.payload != nil
            var imageObject: Any = NSNull()
            if let image, let dimensions {
                guard image.count <= BoundedHEICEncoder.maximumBytes else {
                    throw CoreCaptureIngestorError.malformedRecord
                }
                let managedPath = managedImagePath(
                    artifactID: metadata.imageArtifactID,
                    date: metadata.recordedAt
                )
                imageObject = [
                    "artifact_id": metadata.imageArtifactID,
                    "managed_relative_path": managedPath,
                    "content_hash": contentHash,
                    "dimensions": [
                        "width": dimensions.width,
                        "height": dimensions.height,
                        "scale_milli": dimensions.scaleMilli,
                    ],
                    "expires_at": timestamp(metadata.screenshotExpiresAt),
                    "intent_state": "pending",
                ]
                completion = lifecycleCompletion(metadata: metadata)
            } else if image != nil || dimensions != nil {
                throw CoreCaptureIngestorError.malformedRecord
            }
            attempt = attemptObject(
                metadata: metadata,
                attemptStatus: "completed",
                evidenceState: "captured-new",
                presence: presence,
                ocrState: ocrObject.state,
                content: [
                    "type": "captured",
                    "data": [
                        "context": contextObject(context),
                        "content_hash": contentHash,
                        "ocr": ocrObject.payload ?? NSNull(),
                        "image": imageObject,
                    ],
                ]
            )
        }
        return BuiltRecord(
            event: eventEnvelope(metadata: metadata, payload: attempt),
            completion: completion,
            hasOCR: hasOCR
        )
    }

    private static func deniedAxes(
        _ reason: CaptureDenial
    ) -> (evidenceState: String, protected: Bool) {
        switch reason {
        case .userPaused, .studyExpired: ("paused", false)
        case .permissionDenied, .locked, .asleep, .noExactWindow, .ambiguousWindow:
            ("unavailable", false)
        case .secureInput, .applicationExcluded, .titleExcluded, .chronicleSelf,
             .foregroundChanged:
            ("protected", true)
        }
    }

    private static func presenceForDenial(
        _ reason: CaptureDenial,
        fallback: PresenceSample
    ) -> PresenceSample {
        switch reason {
        case .locked: .locked
        case .asleep: .asleep
        default: fallback
        }
    }

    private static func attemptObject(
        metadata: CaptureEventMetadata,
        attemptStatus: String,
        evidenceState: String,
        presence: PresenceSample,
        ocrState: String,
        content: [String: Any]
    ) -> [String: Any] {
        let mappedPresence: String
        let idleSeconds: Any
        switch presence {
        case .active:
            mappedPresence = "active"
            idleSeconds = NSNull()
        case let .idle(seconds):
            mappedPresence = "idle"
            idleSeconds = seconds
        case .locked:
            mappedPresence = "locked"
            idleSeconds = NSNull()
        case .asleep:
            mappedPresence = "asleep"
            idleSeconds = NSNull()
        case .unknown:
            mappedPresence = "unknown"
            idleSeconds = NSNull()
        }
        return [
            "type": "observation-attempt",
            "data": [
                "cadence_seconds": metadata.cadenceSeconds,
                "attempt_status": attemptStatus,
                "evidence_state": evidenceState,
                "presence_state": mappedPresence,
                "idle_seconds": idleSeconds,
                "ocr_state": ocrState,
                "content": content,
            ],
        ]
    }

    private static func eventEnvelope(
        metadata: CaptureEventMetadata,
        payload: [String: Any]
    ) -> [String: Any] {
        [
            "schema_version": "1.0",
            "event_id": metadata.eventID,
            "device_id": metadata.deviceID,
            "scheduled_at": timestamp(metadata.scheduledAt),
            "observed_at": timestamp(metadata.observedAt),
            "recorded_at": timestamp(metadata.recordedAt),
            "display_timezone": metadata.displayTimezone,
            "source": ["adapter": "macos-exact-window", "version": metadata.sourceVersion],
            "kind": "observation-attempt",
            "payload": payload,
        ]
    }

    private static func lifecycleCompletion(
        metadata: CaptureEventMetadata
    ) -> [String: Any] {
        let when = timestamp(metadata.recordedAt)
        return [
            "schema_version": "1.0",
            "event_id": metadata.lifecycleEventID,
            "device_id": metadata.deviceID,
            "scheduled_at": NSNull(),
            "observed_at": when,
            "recorded_at": when,
            "display_timezone": metadata.displayTimezone,
            "source": ["adapter": "macos-exact-window", "version": metadata.sourceVersion],
            "kind": "screenshot-lifecycle",
            "payload": [
                "type": "screenshot-lifecycle",
                "data": [
                    "artifact_id": metadata.imageArtifactID,
                    "action": "write-completed",
                    "deletion_cause": NSNull(),
                    "projected_state": "retained",
                    "requested_at": NSNull(),
                    "completed_at": when,
                    "source_event_id": metadata.eventID,
                ],
            ],
        ]
    }

    private static func contextObject(_ context: ApprovedWindowContext) -> [String: Any] {
        [
            "application_bundle_id": context.applicationBundleID,
            "process_name": context.processName,
            "window_title": nullable(context.windowTitle),
            "authorized_domain": NSNull(),
        ]
    }

    private static func ocrPayload(
        _ ocr: OCRRecognition,
        change: CaptureOCRChange
    ) -> (state: String, payload: [String: Any]?) {
        let state: String
        let text: String
        let confidence: Any
        switch ocr {
        case let .complete(value, valueConfidence, _):
            state = "complete"
            text = value
            confidence = valueConfidence
        case .empty:
            state = "empty"
            text = ""
            confidence = NSNull()
        case let .partial(value, valueConfidence, _):
            state = "partial"
            text = value
            confidence = valueConfidence.map { $0 as Any } ?? NSNull()
        case .failed:
            return ("failed", nil)
        }
        let provenance = ocr.provenance
        return (
            state,
            [
                "text": text,
                "change": change.rawValue,
                "confidence": confidence,
                "engine": [
                    "adapter": provenance.engineAdapter,
                    "version": provenance.engineVersion,
                ],
                "automatic_language_detection": provenance.automaticLanguageDetection,
                "recognition_languages": provenance.recognitionLanguages,
                "untrusted_evidence": true,
            ]
        )
    }

    private static func managedImagePath(artifactID: String, date: Date) -> String {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .iso8601)
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyyy-MM-dd"
        return "screenshots/\(formatter.string(from: date))/\(artifactID).heic"
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private static func nullable(_ value: String?) -> Any {
        if let value { return value }
        return NSNull()
    }
}

private struct CoreIngestEnvelope: Decodable {
    let ok: Bool
    let result: CoreIngestResult?
    let error: ChronicleErrorPayload?
}

private struct CoreIngestResult: Decodable {
    let acknowledgement: String
}
