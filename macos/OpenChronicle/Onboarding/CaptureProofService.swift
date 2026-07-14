import AppKit
import CoreGraphics
import Foundation
import SwiftUI

@MainActor
final class CaptureProofService: OnboardingCaptureProofRunning {
    private let permission: any ScreenRecordingPermissionServicing

    init(permission: (any ScreenRecordingPermissionServicing)? = nil) {
        self.permission = permission ?? ScreenRecordingPermissionService()
    }

    func run() async -> OnboardingCaptureProofResult {
        guard permission.isGranted() else { return .failed(.permissionDenied) }
        guard let window = makeProofWindow() else { return .failed(.proofWindowUnavailable) }
        defer {
            window.orderOut(nil)
            window.contentView = nil
        }

        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        guard window.windowNumber > 0 else { return .failed(.proofWindowUnavailable) }
        let expectedWindowID = CGWindowID(window.windowNumber)
        let provider = SystemActiveWindowProvider()
        guard await awaitExactWindow(expectedWindowID, provider: provider) else {
            return .failed(.exactWindowUnavailable)
        }

        let tokens = CaptureProofTokenStore()
        let token = await tokens.mint(forTestWindowID: expectedWindowID)
        let validity = CaptureProofAttemptValidity()
        let pipeline = CaptureAttemptPipeline(
            windowProvider: provider,
            environment: SystemCaptureEnvironmentSource(),
            capturer: ScreenCaptureService(),
            deduplicator: ContentDeduplicator(),
            recognizer: VisionOCRService(),
            proofTokens: tokens,
            ingestor: CaptureProofDiscardingIngestor(),
            validity: validity
        )
        let now = Date()
        let result = await pipeline.attempt(
            context: CaptureAttemptContext(
                eventID: "proof-event-\(UUID().uuidString.lowercased())",
                lifecycleEventID: "proof-lifecycle-\(UUID().uuidString.lowercased())",
                imageArtifactID: "proof-image-\(UUID().uuidString.lowercased())",
                deviceID: "onboarding-proof",
                scheduledAt: now,
                displayTimezone: TimeZone.current.identifier,
                sourceVersion: "macos-onboarding-proof-1",
                cadenceSeconds: 60,
                bootSequence: "proof-\(UUID().uuidString.lowercased())",
                monotonicTick: 1,
                retentionSeconds: 0,
                executionGeneration: 1
            ),
            proofToken: token
        )
        return Self.map(result)
    }

    private func makeProofWindow() -> NSWindow? {
        guard NSApp != nil else { return nil }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 260),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.title = "Open Chronicle Capture Test"
        window.level = .normal
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.moveToActiveSpace]
        window.backgroundColor = .white
        window.contentView = NSHostingView(rootView: CaptureProofContent())
        window.center()
        return window
    }

    private func awaitExactWindow(
        _ expectedWindowID: CGWindowID,
        provider: SystemActiveWindowProvider
    ) async -> Bool {
        for _ in 0 ..< 20 {
            if let lookup = try? await provider.resolveActiveWindow(),
               case let .exact(window) = lookup,
               window.identity.windowID == expectedWindowID,
               window.identity.bundleIdentifier == CapturePrivacyPolicy.default.chronicleBundleIdentifier
            {
                return true
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        return false
    }

    private static func map(_ result: CaptureAttemptResult) -> OnboardingCaptureProofResult {
        switch result {
        case .proofSucceeded:
            .passed
        case let .denied(reason):
            switch reason {
            case .permissionDenied: .failed(.permissionDenied)
            case .noExactWindow, .ambiguousWindow: .failed(.exactWindowUnavailable)
            case .foregroundChanged: .failed(.foregroundChanged)
            case .chronicleSelf: .failed(.textMismatch)
            default: .failed(.captureFailed)
            }
        case .stored, .invalidated, .persistenceFailed:
            .failed(.captureFailed)
        }
    }
}

struct CaptureProofContent: View {
    static let ocrVisibleText = CaptureProofTokenStore.fixedText

    var body: some View {
        Text(verbatim: Self.ocrVisibleText)
            .font(.system(size: 32, weight: .semibold, design: .monospaced))
            .foregroundStyle(.black)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.white)
        .accessibilityIdentifier("onboarding.capture-proof-window")
    }
}

private actor CaptureProofAttemptValidity: CaptureAttemptValidityChecking {
    func invalidation(for executionGeneration: UInt64) -> CaptureInvalidation? {
        executionGeneration == 1 ? nil : .superseded
    }

    func claimPersistence(
        for executionGeneration: UInt64
    ) -> CapturePersistencePermit? {
        guard executionGeneration == 1 else { return nil }
        return CapturePersistencePermit(id: UUID(), executionGeneration: executionGeneration)
    }

    func releasePersistence(_ permit: CapturePersistencePermit) {}
}

private actor CaptureProofDiscardingIngestor: CaptureIngesting {
    func ingest(
        record: CaptureIngestRecord,
        image: Data?,
        context: CaptureAttemptContext,
        observedAt: Date,
        permit: CapturePersistencePermit
    ) -> CaptureIngestAcknowledgement {
        CaptureIngestAcknowledgement(
            durability: .durable,
            eventID: nil,
            ocrEventID: nil,
            imageArtifactID: nil
        )
    }
}
