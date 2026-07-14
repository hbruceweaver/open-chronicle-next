import Foundation
import UserNotifications

enum ChronicleNotificationAuthorization: Equatable, Sendable {
    case notDetermined
    case denied
    case authorized
}

enum ChronicleNotificationRoute: String, Equatable, Sendable {
    case health
    case settings

    var appRoute: AppRoute {
        switch self {
        case .health: .health
        case .settings: .settings
        }
    }

    init?(userInfo: [AnyHashable: Any]) {
        guard let rawValue = userInfo["route"] as? String else { return nil }
        self.init(rawValue: rawValue)
    }
}

enum ChronicleNotificationKind: String, CaseIterable, Equatable, Hashable, Sendable {
    case studyPreExpiry = "study-pre-expiry"
    case studyExpired = "study-expired"
    case permissionLost = "permission-lost"
    case storageFailure = "storage-failure"
    case recovered
}

struct ChronicleNotificationMessage: Equatable, Sendable {
    let kind: ChronicleNotificationKind
    let title: String
    let body: String
    let route: ChronicleNotificationRoute

    var identifier: String {
        "open-chronicle.\(kind.rawValue)"
    }
}

protocol ChronicleNotificationDelivering: Sendable {
    func authorizationState() async -> ChronicleNotificationAuthorization
    func requestAuthorization() async throws -> Bool
    func deliver(_ message: ChronicleNotificationMessage) async throws
}

actor SystemChronicleNotificationBackend: ChronicleNotificationDelivering {
    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    func authorizationState() async -> ChronicleNotificationAuthorization {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined: return .notDetermined
        case .denied: return .denied
        case .authorized, .provisional, .ephemeral: return .authorized
        @unknown default: return .denied
        }
    }

    func requestAuthorization() async throws -> Bool {
        try await center.requestAuthorization(options: [.alert, .sound])
    }

    func deliver(_ message: ChronicleNotificationMessage) async throws {
        let content = UNMutableNotificationContent()
        content.title = message.title
        content.body = message.body
        content.sound = .default
        content.userInfo = ["route": message.route.rawValue]
        try await center.add(UNNotificationRequest(
            identifier: message.identifier,
            content: content,
            trigger: nil
        ))
    }
}

final class ChronicleNotificationResponseDelegate: NSObject, UNUserNotificationCenterDelegate {
    typealias RouteHandler = (ChronicleNotificationRoute) -> Void

    private let routeHandler: RouteHandler

    init(routeHandler: @escaping RouteHandler) {
        self.routeHandler = routeHandler
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        if let route = ChronicleNotificationRoute(
            userInfo: response.notification.request.content.userInfo
        ) {
            routeHandler(route)
        }
        completionHandler()
    }
}

actor NotificationService {
    static let studyWarningInterval: TimeInterval = 15 * 60
    static let incidentRepeatInterval: TimeInterval = 6 * 60 * 60
    static let recoveryRepeatInterval: TimeInterval = 60 * 60

    private let backend: any ChronicleNotificationDelivering
    private var activeConditions: Set<ChronicleNotificationKind> = []
    private var lastDelivered: [ChronicleNotificationKind: Date] = [:]
    private var studyEnd: String?
    private var permissionBlockedAt: Date?
    private var storageBlockedAt: Date?
    private(set) var lastDeliveryError: String?

    init(backend: any ChronicleNotificationDelivering = SystemChronicleNotificationBackend()) {
        self.backend = backend
    }

    func requestAuthorization() async -> Bool {
        do {
            let granted = try await backend.requestAuthorization()
            lastDeliveryError = nil
            return granted
        } catch {
            lastDeliveryError = error.localizedDescription
            return false
        }
    }

    func evaluate(
        captureStatus: CapturePresentationState,
        health: DiagnosticHealthSnapshot?,
        at date: Date = Date()
    ) async {
        if studyEnd != health?.study.end {
            studyEnd = health?.study.end
            lastDelivered[.studyPreExpiry] = nil
            lastDelivered[.studyExpired] = nil
        }

        updateBlockingConditions(captureStatus: captureStatus, health: health, at: date)
        let current = conditions(captureStatus: captureStatus, health: health, at: date)
        let blocking: Set<ChronicleNotificationKind> = [.permissionLost, .storageFailure]
        let recovered = !activeConditions.intersection(blocking).isEmpty
            && current.intersection(blocking).isEmpty

        for kind in ChronicleNotificationKind.allCases where kind != .recovered {
            guard current.contains(kind), shouldDeliver(kind, at: date) else { continue }
            await deliver(message(for: kind), at: date)
        }
        if recovered, shouldDeliver(.recovered, at: date) {
            await deliver(message(for: .recovered), at: date)
        }
        activeConditions = current
    }

    private func conditions(
        captureStatus: CapturePresentationState,
        health: DiagnosticHealthSnapshot?,
        at date: Date
    ) -> Set<ChronicleNotificationKind> {
        var result: Set<ChronicleNotificationKind> = []
        if let health,
           health.study.state == .active,
           let end = health.study.endDate
        {
            let remaining = end.timeIntervalSince(date)
            if remaining >= 0, remaining <= Self.studyWarningInterval {
                result.insert(.studyPreExpiry)
            }
        }
        if captureStatus == .studyExpired || health?.study.state == .expired {
            result.insert(.studyExpired)
        }
        if permissionBlockedAt != nil {
            result.insert(.permissionLost)
        }
        if storageBlockedAt != nil {
            result.insert(.storageFailure)
        }
        return result
    }

    private func updateBlockingConditions(
        captureStatus: CapturePresentationState,
        health: DiagnosticHealthSnapshot?,
        at date: Date
    ) {
        let permissionDenied = captureStatus == .unavailable(.permissionDenied)
        if permissionDenied, permissionBlockedAt == nil {
            permissionBlockedAt = date
        }

        let storageBlocked = captureStatus == .storageBlocked
            || health.map({ OperationalStoragePolicy.state(for: $0) == .blocked }) == true
        if storageBlocked, storageBlockedAt == nil {
            storageBlockedAt = date
        }

        if !permissionDenied,
           let blockedAt = permissionBlockedAt,
           let successfulCapture = health?.latest.lastSuccessfulCaptureAt
                .flatMap(ChronicleTimestamp.date),
           successfulCapture > blockedAt
        {
            permissionBlockedAt = nil
        }

        if captureStatus != .storageBlocked,
           let blockedAt = storageBlockedAt,
           let health,
           health.screenshotStorage != nil,
           let observedAt = ChronicleTimestamp.date(health.observedAt),
           observedAt >= blockedAt,
           OperationalStoragePolicy.state(for: health) != .blocked
        {
            storageBlockedAt = nil
        }
    }

    private func shouldDeliver(_ kind: ChronicleNotificationKind, at date: Date) -> Bool {
        guard let previous = lastDelivered[kind] else { return true }
        let interval: TimeInterval?
        switch kind {
        case .permissionLost, .storageFailure:
            interval = Self.incidentRepeatInterval
        case .recovered:
            interval = Self.recoveryRepeatInterval
        case .studyPreExpiry, .studyExpired:
            interval = nil
        }
        guard let interval else { return false }
        return date.timeIntervalSince(previous) >= interval
    }

    private func deliver(_ message: ChronicleNotificationMessage, at date: Date) async {
        guard await backend.authorizationState() == .authorized else { return }
        do {
            try await backend.deliver(message)
            lastDelivered[message.kind] = date
            lastDeliveryError = nil
        } catch {
            lastDeliveryError = error.localizedDescription
        }
    }

    private func message(for kind: ChronicleNotificationKind) -> ChronicleNotificationMessage {
        switch kind {
        case .studyPreExpiry:
            ChronicleNotificationMessage(
                kind: kind,
                title: "Open Chronicle study ending soon",
                body: "The current observation study is within 15 minutes of its configured end.",
                route: .health
            )
        case .studyExpired:
            ChronicleNotificationMessage(
                kind: kind,
                title: "Open Chronicle study ended",
                body: "Observation stopped at the configured study boundary.",
                route: .health
            )
        case .permissionLost:
            ChronicleNotificationMessage(
                kind: kind,
                title: "Screen Recording permission unavailable",
                body: "Open Chronicle cannot observe windows until permission is restored.",
                route: .settings
            )
        case .storageFailure:
            ChronicleNotificationMessage(
                kind: kind,
                title: "Open Chronicle observation paused",
                body: "Storage is below the safe capture budget or the managed image quota is full.",
                route: .health
            )
        case .recovered:
            ChronicleNotificationMessage(
                kind: kind,
                title: "Open Chronicle recovered",
                body: "The blocking permission or storage condition has cleared.",
                route: .health
            )
        }
    }
}
