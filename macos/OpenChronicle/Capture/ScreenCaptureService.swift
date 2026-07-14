import AppKit
import CoreGraphics
import CryptoKit
import CoreVideo
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

protocol ExactWindowCapturing: Sendable {
    func capture(_ window: ResolvedActiveWindow) async throws -> CapturedWindowImage
}

actor ScreenCaptureService: ExactWindowCapturing {
    func capture(_ window: ResolvedActiveWindow) async throws -> CapturedWindowImage {
        let captureWindow: SCWindow
        switch window.handle {
        case let .screenCaptureKit(value): captureWindow = value
        case .testFixture: throw ScreenCaptureServiceError.missingSystemWindow
        }
        let filter = SCContentFilter(
            desktopIndependentWindow: captureWindow
        )
        let configuration = SCStreamConfiguration()
        configuration.showsCursor = false
        configuration.capturesAudio = false
        configuration.scalesToFit = true
        configuration.preservesAspectRatio = true
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        let dimensions = Self.captureDimensions(
            frame: filter.contentRect,
            pointPixelScale: CGFloat(filter.pointPixelScale)
        )
        configuration.width = dimensions.width
        configuration.height = dimensions.height
        let image = try await SCScreenshotManager.captureImage(
            contentFilter: filter,
            configuration: configuration
        )
        return CapturedWindowImage(
            image: image,
            scaleMilli: UInt32(max(1, Int((filter.pointPixelScale * 1_000).rounded())))
        )
    }

    private static func captureDimensions(
        frame: CGRect,
        pointPixelScale: CGFloat
    ) -> (width: Int, height: Int) {
        let sourceWidth = max(1, frame.width * max(1, pointPixelScale))
        let sourceHeight = max(1, frame.height * max(1, pointPixelScale))
        let longEdgeScale = min(1, 2_560 / max(sourceWidth, sourceHeight))
        let pixelScale = min(1, sqrt(8_000_000 / (sourceWidth * sourceHeight)))
        let scale = min(longEdgeScale, pixelScale)
        return (
            max(1, Int(floor(sourceWidth * scale))),
            max(1, Int(floor(sourceHeight * scale)))
        )
    }
}

struct CapturedWindowImage: @unchecked Sendable {
    let image: CGImage
    let scaleMilli: UInt32
}

enum ScreenCaptureServiceError: Error {
    case missingSystemWindow
}

struct NormalizedImage: @unchecked Sendable {
    let cgImage: CGImage
    let pixelBytes: Data
    let width: Int
    let height: Int
    let contentHash: String
}

protocol ImageNormalizing: Sendable {
    func normalize(_ image: CGImage) throws -> NormalizedImage
}

enum ImageNormalizationError: Error {
    case invalidDimensions
    case contextCreationFailed
    case imageCreationFailed
}

struct DeterministicImageNormalizer: ImageNormalizing {
    func normalize(_ image: CGImage) throws -> NormalizedImage {
        guard image.width > 0, image.height > 0 else {
            throw ImageNormalizationError.invalidDimensions
        }
        let sourceWidth = Double(image.width)
        let sourceHeight = Double(image.height)
        let longEdgeScale = min(1, 2_560 / max(sourceWidth, sourceHeight))
        let pixelScale = min(1, sqrt(8_000_000 / (sourceWidth * sourceHeight)))
        let scale = min(longEdgeScale, pixelScale)
        let width = max(1, Int(floor(sourceWidth * scale)))
        let height = max(1, Int(floor(sourceHeight * scale)))
        let bytesPerRow = width * 4
        var bytes = [UInt8](repeating: 0, count: bytesPerRow * height)
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) else {
            throw ImageNormalizationError.contextCreationFailed
        }
        let bitmapInfo = CGBitmapInfo.byteOrder32Little.union(
            CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue)
        )
        let drew = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(
                data: buffer.baseAddress,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: bytesPerRow,
                space: colorSpace,
                bitmapInfo: bitmapInfo.rawValue
            ) else {
                return false
            }
            context.interpolationQuality = .high
            context.setShouldAntialias(false)
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard drew else { throw ImageNormalizationError.contextCreationFailed }

        let pixelBytes = Data(bytes)
        guard let provider = CGDataProvider(data: pixelBytes as CFData),
              let normalized = CGImage(
                  width: width,
                  height: height,
                  bitsPerComponent: 8,
                  bitsPerPixel: 32,
                  bytesPerRow: bytesPerRow,
                  space: colorSpace,
                  bitmapInfo: bitmapInfo,
                  provider: provider,
                  decode: nil,
                  shouldInterpolate: false,
                  intent: .defaultIntent
              )
        else {
            throw ImageNormalizationError.imageCreationFailed
        }
        var hashInput = Data("bgra8-srgb:\(width)x\(height):".utf8)
        hashInput.append(pixelBytes)
        let digest = SHA256.hash(data: hashInput)
            .map { String(format: "%02x", $0) }
            .joined()
        return NormalizedImage(
            cgImage: normalized,
            pixelBytes: pixelBytes,
            width: width,
            height: height,
            contentHash: "sha256-\(digest)"
        )
    }
}

protocol ImageEncoding: Sendable {
    func encodeHEIC(_ image: NormalizedImage) throws -> EncodedHEICImage
}

struct EncodedHEICImage: Equatable, Sendable {
    let data: Data
    let width: UInt32
    let height: UInt32
}

enum HEICEncodingError: Error {
    case unavailable
    case failed
    case exceedsMaximumBytes
}

struct BoundedHEICEncoder: ImageEncoding {
    static let maximumBytes = 4 * 1_024 * 1_024
    private let qualities: [Double] = [0.72, 0.58, 0.44, 0.30]
    private let scales: [Double] = [1.0, 0.85, 0.70, 0.55]

    func encodeHEIC(_ image: NormalizedImage) throws -> EncodedHEICImage {
        for scale in scales {
            let source = try Self.scaledImage(image.cgImage, scale: scale)
            for quality in qualities {
                let data = NSMutableData()
                guard let destination = CGImageDestinationCreateWithData(
                    data,
                    UTType.heic.identifier as CFString,
                    1,
                    nil
                ) else {
                    throw HEICEncodingError.unavailable
                }
                let properties = [
                    kCGImageDestinationLossyCompressionQuality: quality,
                ] as CFDictionary
                CGImageDestinationAddImage(destination, source, properties)
                guard CGImageDestinationFinalize(destination) else {
                    throw HEICEncodingError.failed
                }
                if data.length <= Self.maximumBytes {
                    return EncodedHEICImage(
                        data: Data(referencing: data),
                        width: UInt32(source.width),
                        height: UInt32(source.height)
                    )
                }
            }
        }
        throw HEICEncodingError.exceedsMaximumBytes
    }

    private static func scaledImage(_ image: CGImage, scale: Double) throws -> CGImage {
        guard scale < 1 else { return image }
        let width = max(1, Int(floor(Double(image.width) * scale)))
        let height = max(1, Int(floor(Double(image.height) * scale)))
        let bytesPerRow = width * 4
        var bytes = [UInt8](repeating: 0, count: bytesPerRow * height)
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) else {
            throw HEICEncodingError.failed
        }
        let bitmapInfo = CGBitmapInfo.byteOrder32Little.union(
            CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue)
        )
        let drew = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(
                data: buffer.baseAddress,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: bytesPerRow,
                space: colorSpace,
                bitmapInfo: bitmapInfo.rawValue
            ) else { return false }
            context.interpolationQuality = .high
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard drew else { throw HEICEncodingError.failed }
        let data = Data(bytes)
        guard let provider = CGDataProvider(data: data as CFData),
              let result = CGImage(
                  width: width,
                  height: height,
                  bitsPerComponent: 8,
                  bitsPerPixel: 32,
                  bytesPerRow: bytesPerRow,
                  space: colorSpace,
                  bitmapInfo: bitmapInfo,
                  provider: provider,
                  decode: nil,
                  shouldInterpolate: false,
                  intent: .defaultIntent
              )
        else { throw HEICEncodingError.failed }
        return result
    }
}

struct ApprovedWindowContext: Equatable, Sendable {
    let applicationBundleID: String
    let processName: String
    let windowTitle: String?
}

struct CaptureImageDimensions: Equatable, Sendable {
    let width: UInt32
    let height: UInt32
    let scaleMilli: UInt32
}

enum CaptureIngestRecord: Equatable, Sendable {
    case denied(reason: CaptureDenial, presence: PresenceSample)
    case captureFailed(presence: PresenceSample)
    case unchanged(
        context: ApprovedWindowContext,
        contentHash: String,
        previous: DeduplicatedContentReference,
        presence: PresenceSample
    )
    case changed(
        context: ApprovedWindowContext,
        contentHash: String,
        ocrChange: CaptureOCRChange,
        ocr: OCRRecognition,
        dimensions: CaptureImageDimensions?,
        presence: PresenceSample
    )
}

enum CaptureOCRChange: String, Equatable, Sendable {
    case new
    case changed
}

enum CaptureDurability: Equatable, Sendable {
    case durable
    case journalDurableProjectionPending
    case notDurable

    var isCanonicalDurable: Bool {
        self == .durable || self == .journalDurableProjectionPending
    }
}

struct CaptureIngestAcknowledgement: Equatable, Sendable {
    let durability: CaptureDurability
    let eventID: String?
    let ocrEventID: String?
    let imageArtifactID: String?
}

protocol CaptureIngesting: Sendable {
    func ingest(
        record: CaptureIngestRecord,
        image: Data?,
        context: CaptureAttemptContext,
        observedAt: Date,
        permit: CapturePersistencePermit
    ) async throws -> CaptureIngestAcknowledgement
}

enum CaptureInvalidation: String, Equatable, Sendable {
    case sleep
    case clockChanged = "clock-changed"
    case studyExpired = "study-expired"
    case userPaused = "user-paused"
    case stopping
    case superseded
}

enum CapturePersistenceFailureCategory: Equatable, Sendable {
    case retryableStorage
    case studyBoundary
    case staleGeneration
    case contractRepair
    case unknownFatal
}

struct CapturePersistenceFailure: Equatable, Sendable {
    let category: CapturePersistenceFailureCategory
    let code: String?
    let retryable: Bool

    static let unknown = CapturePersistenceFailure(
        category: .unknownFatal,
        code: nil,
        retryable: false
    )
}

enum CaptureAttemptDirective: Equatable, Sendable {
    case normal
    case forceDenial(CaptureDenial)
}

struct CapturePersistencePermit: Equatable, Sendable {
    let id: UUID
    let executionGeneration: UInt64
}

protocol CaptureAttemptValidityChecking: Sendable {
    func invalidation(for executionGeneration: UInt64) async -> CaptureInvalidation?
    func claimPersistence(
        for executionGeneration: UInt64
    ) async -> CapturePersistencePermit?
    func releasePersistence(_ permit: CapturePersistencePermit) async
}

enum CaptureAttemptResult: Equatable, Sendable {
    case stored(CaptureIngestAcknowledgement)
    case proofSucceeded
    case denied(CaptureDenial)
    case invalidated(CaptureInvalidation)
    case persistenceFailed(CapturePersistenceFailure)
}

actor CaptureAttemptPipeline {
    private let windowProvider: any ActiveWindowProviding
    private let environment: any CaptureEnvironmentProviding
    private let privacyEvaluator: PrivacyEvaluator
    private let policy: CapturePrivacyPolicy
    private let capturer: any ExactWindowCapturing
    private let normalizer: any ImageNormalizing
    private let deduplicator: ContentDeduplicator
    private let recognizer: any OCRRecognizing
    private let encoder: any ImageEncoding
    private let idleState: IdleStateSource
    private let idleThresholdSeconds: UInt32
    private let proofTokens: CaptureProofTokenStore
    private let ingestor: any CaptureIngesting
    private let observationTime: any CaptureRecordingTimeProviding
    private let validity: any CaptureAttemptValidityChecking

    init(
        windowProvider: any ActiveWindowProviding,
        environment: any CaptureEnvironmentProviding,
        privacyEvaluator: PrivacyEvaluator = PrivacyEvaluator(),
        policy: CapturePrivacyPolicy = .default,
        capturer: any ExactWindowCapturing,
        normalizer: any ImageNormalizing = DeterministicImageNormalizer(),
        deduplicator: ContentDeduplicator,
        recognizer: any OCRRecognizing,
        encoder: any ImageEncoding = BoundedHEICEncoder(),
        idleState: IdleStateSource = IdleStateSource(),
        idleThresholdSeconds: UInt32 = 300,
        proofTokens: CaptureProofTokenStore,
        ingestor: any CaptureIngesting,
        observationTime: any CaptureRecordingTimeProviding = SystemCaptureRecordingTimeSource(),
        validity: any CaptureAttemptValidityChecking
    ) {
        self.windowProvider = windowProvider
        self.environment = environment
        self.privacyEvaluator = privacyEvaluator
        self.policy = policy
        self.capturer = capturer
        self.normalizer = normalizer
        self.deduplicator = deduplicator
        self.recognizer = recognizer
        self.encoder = encoder
        self.idleState = idleState
        self.idleThresholdSeconds = idleThresholdSeconds
        self.proofTokens = proofTokens
        self.ingestor = ingestor
        self.observationTime = observationTime
        self.validity = validity
    }

    func attempt(
        context: CaptureAttemptContext,
        directive: CaptureAttemptDirective = .normal,
        proofToken: CaptureProofToken? = nil
    ) async -> CaptureAttemptResult {
        if let invalidation = await validity.invalidation(
            for: context.executionGeneration
        ) {
            return .invalidated(invalidation)
        }
        if case let .forceDenial(reason) = directive {
            return await persistDenial(
                .deny(reason),
                context: context,
                observedAt: await observationTime.now()
            )
        }
        let initialEnvironment = await environment.currentEnvironment()
        if let denial = Self.environmentDenial(initialEnvironment) {
            _ = await proofTokens.beginAttempt(token: proofToken, windowID: nil)
            return await persistDenial(
                .deny(denial),
                context: context,
                observedAt: await observationTime.now()
            )
        }
        let preLookup = await lookupWindow(ifPermitted: initialEnvironment)
        let preWindowID = preLookup.exactWindow?.identity.windowID
        let proof = await proofTokens.beginAttempt(token: proofToken, windowID: preWindowID)
        // Window resolution and proof-token consumption are asynchronous and may
        // outlive a TCC, secure-input, pause, or session-state transition.
        // Authorization uses the final sample immediately before capture.
        let preEnvironment = await environment.currentEnvironment()
        let preSnapshot = snapshot(environment: preEnvironment, lookup: preLookup)
        let preDecision = privacyEvaluator.evaluate(
            snapshot: preSnapshot,
            expectedIdentity: nil,
            proof: proof
        )
        guard case let .allow(preIdentity) = preDecision,
              let resolved = preLookup.exactWindow
        else {
            return await persistDenial(
                preDecision,
                context: context,
                observedAt: await observationTime.now()
            )
        }
        if proofToken != nil,
           (proof == nil || preIdentity.bundleIdentifier != policy.chronicleBundleIdentifier) {
            // An explicit proof request is a separate, pixel-free path. A wrong,
            // reused, or non-Chronicle-scoped token must never become normal capture.
            return await persistDenial(
                .deny(.chronicleSelf),
                context: context,
                observedAt: await observationTime.now()
            )
        }

        let prepared = await captureAndPrepare(
            resolved: resolved,
            expectedIdentity: preIdentity,
            proof: proof,
            context: context
        )
        let normalized: NormalizedImage
        let scaleMilli: UInt32
        let postIdentity: ActiveWindowIdentity
        let observedAt: Date
        switch prepared {
        case let .ready(value, scale, identity, observationTime):
            normalized = value
            scaleMilli = scale
            postIdentity = identity
            observedAt = observationTime
        case .proofSucceeded:
            return .proofSucceeded
        case let .denied(reason):
            // The helper has already ended the CGImage lifetime before persistence.
            return await persistDenial(
                .deny(reason),
                context: context,
                observedAt: await observationTime.now()
            )
        case let .invalidated(reason):
            return .invalidated(reason)
        case .captureFailed:
            return await persistCaptureFailure(
                context: context,
                observedAt: await observationTime.now()
            )
        }
        let windowContext = ApprovedWindowContext(
            applicationBundleID: postIdentity.bundleIdentifier,
            processName: postIdentity.processName,
            windowTitle: postIdentity.windowTitle
        )
        let key = DeduplicationKey(
            contentHash: normalized.contentHash,
            bundleIdentifier: windowContext.applicationBundleID,
            processName: windowContext.processName,
            windowTitle: windowContext.windowTitle
        )
        let presence = idleState.sample(thresholdSeconds: idleThresholdSeconds)

        if let previous = await deduplicator.match(for: key) {
            let result = await persist(
                .unchanged(
                    context: windowContext,
                    contentHash: normalized.contentHash,
                    previous: previous,
                    presence: presence
                ),
                image: nil,
                context: context,
                observedAt: observedAt
            )
            if case let .stored(acknowledgement) = result,
               acknowledgement.durability.isCanonicalDurable,
               let eventID = acknowledgement.eventID {
                await deduplicator.acknowledgeDurable(
                    DeduplicatedContentReference(
                        key: key,
                        eventID: eventID,
                        ocrEventID: previous.ocrEventID,
                        imageArtifactID: previous.imageArtifactID
                    )
                )
            }
            return result
        }

        let ocr = await recognizer.recognize(normalized)

        let encodedImage = try? encoder.encodeHEIC(normalized)
        let ocrChange: CaptureOCRChange = await deduplicator.latest() == nil
            ? .new
            : .changed
        let dimensions = encodedImage.map {
            CaptureImageDimensions(
                width: $0.width,
                height: $0.height,
                scaleMilli: scaleMilli
            )
        }
        let result = await persist(
            .changed(
                context: windowContext,
                contentHash: normalized.contentHash,
                ocrChange: ocrChange,
                ocr: ocr,
                dimensions: dimensions,
                presence: presence
            ),
            image: encodedImage?.data,
            context: context,
            observedAt: observedAt
        )
        if case let .stored(acknowledgement) = result,
           acknowledgement.durability.isCanonicalDurable,
           let eventID = acknowledgement.eventID {
            await deduplicator.acknowledgeDurable(
                DeduplicatedContentReference(
                    key: key,
                    eventID: eventID,
                    ocrEventID: acknowledgement.ocrEventID,
                    imageArtifactID: acknowledgement.imageArtifactID
                )
            )
        }
        return result
    }

    private func lookupWindow(ifPermitted state: CaptureEnvironmentState) async -> ActiveWindowLookup {
        guard state.hasScreenCapturePermission else { return .noExactWindow }
        do {
            return try await windowProvider.resolveActiveWindow()
        } catch {
            return .noExactWindow
        }
    }

    private enum PreparedCapture {
        case ready(NormalizedImage, UInt32, ActiveWindowIdentity, Date)
        case proofSucceeded
        case denied(CaptureDenial)
        case invalidated(CaptureInvalidation)
        case captureFailed
    }

    /// A denial returns no image, so the captured CGImage is released before the
    /// caller appends the coarse outcome.
    private func captureAndPrepare(
        resolved: ResolvedActiveWindow,
        expectedIdentity: ActiveWindowIdentity,
        proof: CaptureProofAuthorization?,
        context: CaptureAttemptContext
    ) async -> PreparedCapture {
        if let invalidation = await validity.invalidation(
            for: context.executionGeneration
        ) {
            return .invalidated(invalidation)
        }
        let captured: CapturedWindowImage
        do {
            captured = try await capturer.capture(resolved)
        } catch {
            let failedEnvironment = await environment.currentEnvironment()
            if let denial = Self.environmentDenial(failedEnvironment) {
                return .denied(denial)
            }
            return .captureFailed
        }

        if let invalidation = await validity.invalidation(
            for: context.executionGeneration
        ) {
            return .invalidated(invalidation)
        }

        let preLookupEnvironment = await environment.currentEnvironment()
        if let denial = Self.environmentDenial(preLookupEnvironment) {
            return .denied(denial)
        }
        let postLookup = await lookupWindow(ifPermitted: preLookupEnvironment)
        // Re-sample after the async lookup. No pixel consumer may run on the older
        // authorization snapshot.
        let postEnvironment = await environment.currentEnvironment()
        let decision = privacyEvaluator.evaluate(
            snapshot: snapshot(environment: postEnvironment, lookup: postLookup),
            expectedIdentity: expectedIdentity,
            proof: proof
        )
        guard case let .allow(postIdentity) = decision else {
            guard case let .deny(reason) = decision else { return .captureFailed }
            return .denied(reason)
        }
        if let invalidation = await validity.invalidation(
            for: context.executionGeneration
        ) {
            return .invalidated(invalidation)
        }
        let observedAt = await observationTime.now()
        guard observedAt >= context.scheduledAt else {
            return .invalidated(.clockChanged)
        }
        guard let normalized = try? normalizer.normalize(captured.image) else {
            return .captureFailed
        }
        if let proof {
            let ocr = await recognizer.recognize(normalized)
            guard let recognizedText = ocr.recognizedText,
                  proof.accepts(
                      windowID: postIdentity.windowID,
                      recognizedText: recognizedText
                  )
            else {
                return .denied(.chronicleSelf)
            }
            return .proofSucceeded
        }
        return .ready(normalized, captured.scaleMilli, postIdentity, observedAt)
    }

    private static func environmentDenial(
        _ state: CaptureEnvironmentState
    ) -> CaptureDenial? {
        if state.isStudyExpired { return .studyExpired }
        if state.isPaused { return .userPaused }
        if state.isLocked { return .locked }
        if state.isAsleep { return .asleep }
        if !state.hasScreenCapturePermission { return .permissionDenied }
        if state.secureInputEnabled { return .secureInput }
        return nil
    }

    private func snapshot(
        environment: CaptureEnvironmentState,
        lookup: ActiveWindowLookup
    ) -> CapturePrivacySnapshot {
        CapturePrivacySnapshot(
            window: lookup.privacyResolution,
            isPaused: environment.isPaused,
            isStudyExpired: environment.isStudyExpired,
            isLocked: environment.isLocked,
            isAsleep: environment.isAsleep,
            hasScreenCapturePermission: environment.hasScreenCapturePermission,
            secureInputEnabled: environment.secureInputEnabled,
            policy: policy
        )
    }

    private func persistDenial(
        _ decision: PrivacyDecision,
        context: CaptureAttemptContext,
        observedAt: Date
    ) async -> CaptureAttemptResult {
        guard case let .deny(reason) = decision else {
            return .persistenceFailed(.unknown)
        }
        let result = await persist(
            .denied(
                reason: reason,
                presence: idleState.sample(thresholdSeconds: idleThresholdSeconds)
            ),
            image: nil,
            context: context,
            observedAt: observedAt
        )
        if case .stored = result { return .denied(reason) }
        return result
    }

    private func persistCaptureFailure(
        context: CaptureAttemptContext,
        observedAt: Date
    ) async -> CaptureAttemptResult {
        await persist(
            .captureFailed(
                presence: idleState.sample(thresholdSeconds: idleThresholdSeconds)
            ),
            image: nil,
            context: context,
            observedAt: observedAt
        )
    }

    private func persist(
        _ record: CaptureIngestRecord,
        image: Data?,
        context: CaptureAttemptContext,
        observedAt: Date
    ) async -> CaptureAttemptResult {
        guard observedAt >= context.scheduledAt else {
            return .invalidated(.clockChanged)
        }
        guard let permit = await validity.claimPersistence(
            for: context.executionGeneration
        ) else {
            return .invalidated(
                await validity.invalidation(for: context.executionGeneration)
                    ?? .superseded
            )
        }
        let result: CaptureAttemptResult
        do {
            let acknowledgement = try await ingestor.ingest(
                record: record,
                image: image.map { Data($0) },
                context: context,
                observedAt: observedAt,
                permit: permit
            )
            result = acknowledgement.durability.isCanonicalDurable
                ? .stored(acknowledgement)
                : .persistenceFailed(.unknown)
        } catch {
            if case CoreCaptureIngestorError.clockDiscontinuity = error {
                result = .invalidated(.clockChanged)
            } else {
                result = .persistenceFailed(Self.persistenceFailure(from: error))
            }
        }
        await validity.releasePersistence(permit)
        return result
    }

    private static func persistenceFailure(from error: Error) -> CapturePersistenceFailure {
        let payload: ChronicleErrorPayload?
        switch error {
        case let CoreCaptureIngestorError.coreRejected(value): payload = value
        case let ChronicleBridgeError.bridgeStatus(_, value): payload = value
        default: payload = nil
        }
        guard let payload else { return .unknown }
        let category: CapturePersistenceFailureCategory
        switch payload.code {
        case "screenshot-free-space", "screenshot-image-quota", "io-error":
            category = .retryableStorage
        case "study-expired", "study-not-started":
            category = .studyBoundary
        case "stale-generation", "invalid-handle", "closed":
            category = .staleGeneration
        case "contract-error", "ingest-contract-error", "event-contract-error", "schema-mismatch",
             "invalid-call-envelope", "malformed-response":
            category = .contractRepair
        default:
            category = payload.retryable ? .retryableStorage : .unknownFatal
        }
        return CapturePersistenceFailure(
            category: category,
            code: payload.code,
            retryable: category == .retryableStorage
        )
    }
}

private extension ActiveWindowLookup {
    var exactWindow: ResolvedActiveWindow? {
        guard case let .exact(window) = self else { return nil }
        return window
    }
}
