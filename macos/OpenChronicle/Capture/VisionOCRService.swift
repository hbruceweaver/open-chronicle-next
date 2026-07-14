import CoreGraphics
import Foundation
import Vision

struct OCRProvenance: Codable, Equatable, Sendable {
    let engineAdapter: String
    let engineVersion: String
    let automaticLanguageDetection: Bool
    let recognitionLanguages: [String]

    enum CodingKeys: String, CodingKey {
        case engineAdapter = "engine_adapter"
        case engineVersion = "engine_version"
        case automaticLanguageDetection = "automatic_language_detection"
        case recognitionLanguages = "recognition_languages"
    }
}

enum OCRRecognition: Equatable, Sendable {
    case complete(text: String, confidence: Float, provenance: OCRProvenance)
    case empty(provenance: OCRProvenance)
    case partial(text: String, confidence: Float?, provenance: OCRProvenance)
    case failed(provenance: OCRProvenance)

    var recognizedText: String? {
        switch self {
        case let .complete(text, _, _), let .partial(text, _, _): text
        case .empty: ""
        case .failed: nil
        }
    }

    var provenance: OCRProvenance {
        switch self {
        case let .complete(_, _, value), let .empty(value),
             let .partial(_, _, value), let .failed(value):
            value
        }
    }
}

protocol OCRRecognizing: Sendable {
    func recognize(_ image: NormalizedImage) async -> OCRRecognition
}

struct VisionOCRObservation: Equatable, Sendable {
    let text: String?
    let confidence: Float?
    let boundingBox: CGRect
}

struct VisionOCRRawResult: Equatable, Sendable {
    let requestRevision: Int
    let observations: [VisionOCRObservation]
}

protocol VisionRequestPerforming: Sendable {
    func perform(
        image: CGImage,
        configuration: VisionOCRConfiguration
    ) throws -> VisionOCRRawResult
}

struct SystemVisionRequestPerformer: VisionRequestPerforming {
    func perform(
        image: CGImage,
        configuration: VisionOCRConfiguration
    ) throws -> VisionOCRRawResult {
        let request = VNRecognizeTextRequest()
        request.revision = VNRecognizeTextRequestRevision3
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = false
        request.automaticallyDetectsLanguage = configuration.automaticallyDetectsLanguage
        request.recognitionLanguages = configuration.recognitionLanguages
        let handler = VNImageRequestHandler(cgImage: image, options: [:])
        try handler.perform([request])
        return VisionOCRRawResult(
            requestRevision: request.revision,
            observations: (request.results ?? []).map { observation in
                let candidate = observation.topCandidates(1).first
                return VisionOCRObservation(
                    text: candidate?.string,
                    confidence: candidate?.confidence,
                    boundingBox: observation.boundingBox
                )
            }
        )
    }
}

struct VisionOCRConfiguration: Equatable, Sendable {
    let automaticallyDetectsLanguage: Bool
    let recognitionLanguages: [String]

    static let `default` = VisionOCRConfiguration(
        automaticallyDetectsLanguage: true,
        recognitionLanguages: []
    )
}

actor VisionOCRService: OCRRecognizing {
    private static let maximumTextBytes = 64 * 1_024
    private static let rowHeight = 0.001
    let configuration: VisionOCRConfiguration
    private let performer: any VisionRequestPerforming

    init(
        configuration: VisionOCRConfiguration = .default,
        performer: any VisionRequestPerforming = SystemVisionRequestPerformer()
    ) {
        self.configuration = configuration
        self.performer = performer
    }

    func recognize(_ image: NormalizedImage) -> OCRRecognition {
        let raw: VisionOCRRawResult
        do {
            raw = try performer.perform(
                image: image.cgImage,
                configuration: configuration
            )
        } catch {
            return .failed(provenance: provenance(requestRevision: 3))
        }
        let provenance = provenance(requestRevision: raw.requestRevision)
        let observations = raw.observations.sorted(by: Self.readingOrder)
        if observations.isEmpty {
            return .empty(provenance: provenance)
        }
        let candidates = observations.compactMap { observation -> (String, Float)? in
            guard let text = observation.text,
                  let confidence = observation.confidence
            else { return nil }
            return (text, confidence)
        }
        if candidates.isEmpty {
            return .empty(provenance: provenance)
        }
        let rawText = candidates.map(\.0).joined(separator: "\n")
        let (text, wasTruncated) = Self.boundUTF8(rawText)
        let confidence = candidates.map(\.1).reduce(0, +) / Float(candidates.count)
        if text.isEmpty {
            return .empty(provenance: provenance)
        }
        if candidates.count != observations.count || wasTruncated {
            return .partial(text: text, confidence: confidence, provenance: provenance)
        }
        return .complete(text: text, confidence: confidence, provenance: provenance)
    }

    private func provenance(requestRevision: Int) -> OCRProvenance {
        let provenance = OCRProvenance(
            engineAdapter: "apple-vision-vnrecognizetextrequest",
            engineVersion: Self.engineVersion(requestRevision: requestRevision),
            automaticLanguageDetection: configuration.automaticallyDetectsLanguage,
            recognitionLanguages: configuration.recognitionLanguages
        )
        return provenance
    }

    private static func engineVersion(requestRevision: Int) -> String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        return "request-revision-\(requestRevision);macos-\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
    }

    /// Fixed global row buckets form a transitive equivalence relation. The
    /// remaining comparisons provide a strict total order for deterministic OCR.
    private static func readingOrder(
        _ left: VisionOCRObservation,
        _ right: VisionOCRObservation
    ) -> Bool {
        let leftRow = Int(floor(left.boundingBox.midY / rowHeight))
        let rightRow = Int(floor(right.boundingBox.midY / rowHeight))
        if leftRow != rightRow { return leftRow > rightRow }
        if left.boundingBox.minX != right.boundingBox.minX {
            return left.boundingBox.minX < right.boundingBox.minX
        }
        if left.boundingBox.midY != right.boundingBox.midY {
            return left.boundingBox.midY > right.boundingBox.midY
        }
        if left.boundingBox.minY != right.boundingBox.minY {
            return left.boundingBox.minY > right.boundingBox.minY
        }
        if left.boundingBox.maxX != right.boundingBox.maxX {
            return left.boundingBox.maxX < right.boundingBox.maxX
        }
        let leftText = left.text ?? ""
        let rightText = right.text ?? ""
        if leftText != rightText { return leftText < rightText }
        return (left.confidence ?? -1) > (right.confidence ?? -1)
    }

    private static func boundUTF8(_ text: String) -> (String, Bool) {
        guard text.utf8.count > maximumTextBytes else { return (text, false) }
        var result = ""
        var byteCount = 0
        for character in text {
            let width = String(character).utf8.count
            guard byteCount + width <= maximumTextBytes else { break }
            result.append(character)
            byteCount += width
        }
        return (result, true)
    }
}
