import CoreGraphics
import Foundation

enum PresenceSample: Equatable, Sendable {
    case active
    case idle(seconds: UInt32)
    case locked
    case asleep
    case unknown
}

protocol IdleSecondsReading: Sendable {
    func idleSeconds() -> TimeInterval?
}

struct SystemIdleSecondsReader: IdleSecondsReading {
    func idleSeconds() -> TimeInterval? {
        let value = CGEventSource.secondsSinceLastEventType(
            .combinedSessionState,
            eventType: CGEventType(rawValue: UInt32.max)!
        )
        return value.isFinite && value >= 0 ? value : nil
    }
}

struct IdleStateSource: Sendable {
    let reader: any IdleSecondsReading

    init(reader: any IdleSecondsReading = SystemIdleSecondsReader()) {
        self.reader = reader
    }

    func sample(thresholdSeconds: UInt32) -> PresenceSample {
        guard thresholdSeconds > 0,
              let raw = reader.idleSeconds(),
              raw.isFinite,
              raw >= 0
        else {
            return .unknown
        }
        let bounded = UInt32(min(raw.rounded(.down), Double(UInt32.max)))
        return bounded >= thresholdSeconds ? .idle(seconds: bounded) : .active
    }
}
