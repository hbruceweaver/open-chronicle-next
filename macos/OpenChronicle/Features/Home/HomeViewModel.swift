import Foundation

enum HomeRangePreset: String, CaseIterable, Identifiable, Sendable {
    case today
    case yesterday
    case last7Days
    case custom

    var id: String { rawValue }

    var title: String {
        switch self {
        case .today: "Today"
        case .yesterday: "Yesterday"
        case .last7Days: "Last 7 Days"
        case .custom: "Custom"
        }
    }
}

enum HomeLoadState {
    case detached
    case loading
    case waitingForFirstBucket
    case empty(FactualReportSnapshot)
    case loaded(FactualReportSnapshot, partialCoverage: Bool)
    case permissionBlocked(FactualReportSnapshot?)
    case failed(String)

    var snapshot: FactualReportSnapshot? {
        switch self {
        case let .empty(snapshot), let .loaded(snapshot, _): snapshot
        case let .permissionBlocked(snapshot): snapshot
        case .detached, .loading, .waitingForFirstBucket, .failed: nil
        }
    }
}

struct HomeReportMetrics: Equatable, Sendable {
    let expectedSeconds: UInt64
    let accountedSeconds: UInt64
    let capturedSeconds: UInt64
    let observedComputerSeconds: UInt64
    let idleSeconds: UInt64
    let protectedSeconds: UInt64
    let pausedSeconds: UInt64
    let unavailableSeconds: UInt64
    let errorSeconds: UInt64
    let gapSeconds: UInt64
    let transitionCount: Int
    let dailyAverageSeconds: UInt64

    var coverageFraction: Double {
        expectedSeconds == 0 ? 0 : Double(accountedSeconds) / Double(expectedSeconds)
    }

    var captureFraction: Double {
        expectedSeconds == 0 ? 0 : Double(capturedSeconds) / Double(expectedSeconds)
    }
}

struct HomeApplicationBreakdown: Identifiable, Equatable, Sendable {
    let application: FactualReportTotal
    let windows: [FactualReportTotal]

    var id: String { application.id }
}

struct HomeEvidenceReferences: Equatable, Sendable {
    let chunkIDs: [String]
    let eventIDs: [String]
    let intervals: [HomeEvidenceInterval]
    let note: String?

    init(
        chunkIDs: [String],
        eventIDs: [String],
        intervals: [HomeEvidenceInterval] = [],
        note: String? = nil
    ) {
        self.chunkIDs = chunkIDs
        self.eventIDs = eventIDs
        self.intervals = intervals
        self.note = note
    }
}

struct HomeEvidenceInterval: Equatable, Identifiable, Sendable {
    let start: Date
    let end: Date
    let state: String
    let supportingEventIDs: [String]

    var id: String { "\(state):\(start.timeIntervalSince1970):\(end.timeIntervalSince1970)" }
}

enum HomeMetricEvidenceKind: Sendable {
    case observedComputerTime
    case evidenceAccounted
    case captured
    case idle
    case transitions
    case protected
    case paused
    case unavailable
    case error
    case gap
}

enum HomeActivityPartKind: Equatable, Sendable {
    case application(String)
    case idle
    case capturedUnattributed
    case protected
    case paused
    case unavailable
    case error
    case gap
}

struct HomeActivityPart: Equatable, Identifiable, Sendable {
    let id: String
    let kind: HomeActivityPartKind
    let label: String
    let seconds: UInt32
}

enum HomeActivityPartition {
    static func parts(_ bucket: FactualReportActivityBucket) -> [HomeActivityPart] {
        var parts = bucket.durationEstimates
            .filter { $0.dimension == "application" && $0.estimatedSeconds > 0 }
            .map {
                HomeActivityPart(
                    id: "application:\($0.key)",
                    kind: .application($0.key),
                    label: $0.label,
                    seconds: $0.estimatedSeconds
                )
            }
        let captured = bucket.evidenceSeconds.captured
        let attributed = UInt32(min(
            UInt64(captured),
            parts.reduce(UInt64(0)) { $0 + UInt64($1.seconds) }
        ))
        let idle = min(bucket.presenceSeconds.idle, captured - attributed)
        let capturedOther = captured - attributed - idle
        parts.append(contentsOf: [
            HomeActivityPart(
                id: "idle",
                kind: .idle,
                label: "Idle",
                seconds: idle
            ),
            HomeActivityPart(
                id: "captured-other",
                kind: .capturedUnattributed,
                label: "Captured, unattributed",
                seconds: capturedOther
            ),
            HomeActivityPart(
                id: "protected",
                kind: .protected,
                label: "Protected",
                seconds: bucket.evidenceSeconds.protected
            ),
            HomeActivityPart(
                id: "paused",
                kind: .paused,
                label: "Paused",
                seconds: bucket.evidenceSeconds.paused
            ),
            HomeActivityPart(
                id: "unavailable",
                kind: .unavailable,
                label: "Unavailable",
                seconds: bucket.evidenceSeconds.unavailable
            ),
            HomeActivityPart(
                id: "error",
                kind: .error,
                label: "Error",
                seconds: bucket.evidenceSeconds.error
            ),
            HomeActivityPart(
                id: "gap",
                kind: .gap,
                label: "Gap",
                seconds: bucket.evidenceSeconds.gap
            ),
        ])
        return parts.filter { $0.seconds > 0 }
    }
}

enum HomeReportRangeBuilder {
    static func range(
        preset: HomeRangePreset,
        customStart: Date,
        customEnd: Date,
        now: Date,
        calendar sourceCalendar: Calendar,
        timeZone: TimeZone
    ) -> FactualReportRange? {
        var calendar = sourceCalendar
        calendar.timeZone = timeZone
        let todayStart = calendar.startOfDay(for: now)
        let lastCompleteBucket = alignDown(now)
        let raw: (start: Date, end: Date)?
        switch preset {
        case .today:
            raw = (todayStart, lastCompleteBucket)
        case .yesterday:
            guard let start = calendar.date(byAdding: .day, value: -1, to: todayStart) else {
                return nil
            }
            raw = (start, todayStart)
        case .last7Days:
            guard let start = calendar.date(byAdding: .day, value: -6, to: todayStart) else {
                return nil
            }
            raw = (start, lastCompleteBucket)
        case .custom:
            raw = (customStart, min(customEnd, lastCompleteBucket))
        }
        guard let raw else { return nil }
        let start = alignUp(raw.start)
        let end = alignDown(raw.end)
        guard start < end else { return nil }
        return FactualReportRange(start: start, end: end)
    }

    private static func alignDown(_ value: Date) -> Date {
        let epoch = value.timeIntervalSince1970
        return Date(timeIntervalSince1970: floor(epoch / 300) * 300)
    }

    private static func alignUp(_ value: Date) -> Date {
        let epoch = value.timeIntervalSince1970
        return Date(timeIntervalSince1970: ceil(epoch / 300) * 300)
    }
}

@MainActor
final class HomeViewModel: ObservableObject {
    @Published private(set) var selectedRange: HomeRangePreset = .today
    @Published private(set) var customStart: Date
    @Published private(set) var customEnd: Date
    @Published private(set) var state: HomeLoadState = .detached
    @Published private(set) var newDataAvailable = false
    @Published private(set) var displayTimeZone: TimeZone

    private let calendar: Calendar
    private let nowProvider: @Sendable () -> Date
    private var client: (any FactualReportQuerying)?
    private var activeLoadID: UUID?

    init(
        client: (any FactualReportQuerying)? = nil,
        calendar: Calendar = .autoupdatingCurrent,
        timeZone: TimeZone = .current,
        nowProvider: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.client = client
        self.calendar = calendar
        displayTimeZone = timeZone
        self.nowProvider = nowProvider
        let now = nowProvider()
        customEnd = now
        customStart = calendar.date(byAdding: .day, value: -1, to: now) ?? now
        if client != nil { state = .loading }
    }

    var displayTimeZoneName: String {
        displayTimeZone.localizedName(for: .standard, locale: .current) ?? displayTimeZone.identifier
    }

    var snapshot: FactualReportSnapshot? { state.snapshot }

    var metrics: HomeReportMetrics? {
        guard let snapshot else { return nil }
        let evidence = snapshot.coverage.evidenceSeconds
        let expected = evidence.total
        let observed = snapshot.factualTotals
            .filter { $0.dimension == "application" }
            .reduce(UInt64(0)) { $0 + UInt64($1.estimatedSeconds) }
        let activeDays = Set(snapshot.activityBuckets.compactMap { bucket -> Date? in
            guard bucket.durationEstimates.contains(where: {
                $0.dimension == "application" && $0.estimatedSeconds > 0
            }) else { return nil }
            var calendar = self.calendar
            calendar.timeZone = displayTimeZone
            return calendar.startOfDay(for: bucket.start)
        }).count
        return HomeReportMetrics(
            expectedSeconds: expected,
            accountedSeconds: expected.saturatingSubtract(UInt64(evidence.gap)),
            capturedSeconds: UInt64(evidence.captured),
            observedComputerSeconds: observed,
            idleSeconds: UInt64(snapshot.coverage.presenceSeconds.idle),
            protectedSeconds: UInt64(evidence.protected),
            pausedSeconds: UInt64(evidence.paused),
            unavailableSeconds: UInt64(evidence.unavailable),
            errorSeconds: UInt64(evidence.error),
            gapSeconds: UInt64(evidence.gap),
            transitionCount: snapshot.transitions.count,
            dailyAverageSeconds: activeDays == 0 ? 0 : observed / UInt64(activeDays)
        )
    }

    var applicationBreakdown: [HomeApplicationBreakdown] {
        guard let snapshot else { return [] }
        let windows = Dictionary(grouping: snapshot.factualTotals.filter {
            $0.dimension == "window" && $0.parentKey != nil
        }, by: { $0.parentKey ?? "" })
        return sortedTotals(dimension: "application").map { application in
            HomeApplicationBreakdown(
                application: application,
                windows: (windows[application.key] ?? []).sorted(by: Self.totalOrder)
            )
        }
    }

    var domainBreakdown: [FactualReportTotal] {
        guard snapshot?.domainContextAvailable == true else { return [] }
        return sortedTotals(dimension: "authorized-domain")
    }

    var recentBuckets: [FactualReportActivityBucket] {
        Array((snapshot?.activityBuckets ?? []).sorted { $0.start > $1.start }.prefix(8))
    }

    func evidenceReferences(for metric: HomeMetricEvidenceKind) -> HomeEvidenceReferences {
        guard let snapshot else {
            return HomeEvidenceReferences(chunkIDs: [], eventIDs: [])
        }
        switch metric {
        case .observedComputerTime:
            let totals = snapshot.factualTotals.filter { $0.dimension == "application" }
            return HomeEvidenceReferences(
                chunkIDs: unique(totals.flatMap(\.supportingChunkIDs)),
                eventIDs: unique(totals.flatMap(\.supportingEventIDs)),
                note: "Application-attributed estimates link to their exact contributing chunks and events."
            )
        case .evidenceAccounted:
            let buckets = snapshot.activityBuckets.filter {
                $0.evidenceSeconds.total > UInt64($0.evidenceSeconds.gap)
            }
            return HomeEvidenceReferences(
                chunkIDs: buckets.map(\.chunkID),
                eventIDs: [],
                note: "These chunks contain captured, protected, paused, unavailable, or error coverage. Event IDs are omitted because this metric spans several evidence states."
            )
        case .captured:
            return bucketLevelReferences(
                snapshot.activityBuckets.filter { $0.evidenceSeconds.captured > 0 },
                note: "These chunks contain captured coverage. Event-level presence filtering is intentionally not inferred from aggregate chunks."
            )
        case .idle:
            return bucketLevelReferences(
                snapshot.activityBuckets.filter { $0.presenceSeconds.idle > 0 },
                note: "These chunks contain idle presence inside captured coverage. Chronicle does not guess which individual source event represents the aggregate duration."
            )
        case .transitions:
            let buckets = snapshot.activityBuckets.filter { !$0.transitions.isEmpty }
            return HomeEvidenceReferences(
                chunkIDs: buckets.map(\.chunkID),
                eventIDs: unique(buckets.flatMap { $0.transitions.map(\.supportingEventID) }),
                note: "Every transition links to its exact supporting event."
            )
        case .protected:
            return coverageReferences(snapshot, state: "protected")
        case .paused:
            return coverageReferences(snapshot, state: "paused")
        case .unavailable:
            return coverageReferences(snapshot, state: "unavailable")
        case .error:
            return coverageReferences(snapshot, state: "error")
        case .gap:
            return coverageReferences(snapshot, state: "missing-observation")
        }
    }

    func evidenceReferences(for total: FactualReportTotal) -> HomeEvidenceReferences {
        HomeEvidenceReferences(
            chunkIDs: total.supportingChunkIDs,
            eventIDs: total.supportingEventIDs
        )
    }

    private func bucketLevelReferences(
        _ buckets: [FactualReportActivityBucket],
        note: String
    ) -> HomeEvidenceReferences {
        HomeEvidenceReferences(
            chunkIDs: buckets.map(\.chunkID),
            eventIDs: [],
            note: note
        )
    }

    private func coverageReferences(
        _ snapshot: FactualReportSnapshot,
        state: String
    ) -> HomeEvidenceReferences {
        let gaps = snapshot.coverage.gaps.filter { $0.kind == state }
        let buckets = snapshot.activityBuckets.filter { bucket in
            switch state {
            case "protected": bucket.evidenceSeconds.protected > 0
            case "paused": bucket.evidenceSeconds.paused > 0
            case "unavailable": bucket.evidenceSeconds.unavailable > 0
            case "error": bucket.evidenceSeconds.error > 0
            case "missing-observation": bucket.evidenceSeconds.gap > 0
            default: false
            }
        }
        let label = state == "missing-observation" ? "gap" : state
        return HomeEvidenceReferences(
            chunkIDs: buckets.map(\.chunkID),
            eventIDs: unique(gaps.flatMap(\.supportingEventIDs)),
            intervals: gaps.map {
                HomeEvidenceInterval(
                    start: $0.start,
                    end: $0.end,
                    state: label,
                    supportingEventIDs: $0.supportingEventIDs
                )
            },
            note: "Intervals are authoritative even when no source event ID exists; a missing-observation interval may represent the absence of an event."
        )
    }

    private func unique(_ values: [String]) -> [String] {
        var seen = Set<String>()
        return values.filter { seen.insert($0).inserted }
    }

    func attach(client: any FactualReportQuerying) {
        self.client = client
        Task { await load() }
    }

    func detach() {
        activeLoadID = nil
        client = nil
        state = .detached
        newDataAvailable = false
    }

    func select(_ range: HomeRangePreset) {
        selectedRange = range
        Task { await load() }
    }

    func setCustomRange(start: Date, end: Date) {
        customStart = start
        customEnd = end
    }

    func applyCustomRange() {
        selectedRange = .custom
        Task { await load() }
    }

    func systemTimeZoneDidChange(to timeZone: TimeZone = .current) async {
        displayTimeZone = timeZone
        await load()
    }

    func load(now: Date? = nil) async {
        guard let client else {
            state = .detached
            return
        }
        let current = now ?? nowProvider()
        guard let range = HomeReportRangeBuilder.range(
            preset: selectedRange,
            customStart: customStart,
            customEnd: customEnd,
            now: current,
            calendar: calendar,
            timeZone: displayTimeZone
        ) else {
            state = .waitingForFirstBucket
            return
        }
        let loadID = UUID()
        activeLoadID = loadID
        state = .loading
        do {
            let snapshot = try await client.report(range: range, now: current)
            guard activeLoadID == loadID else { return }
            newDataAvailable = false
            if snapshot.activityBuckets.isEmpty && snapshot.factualTotals.isEmpty {
                state = .empty(snapshot)
            } else {
                state = .loaded(snapshot, partialCoverage: Self.isPartial(snapshot))
            }
        } catch {
            guard activeLoadID == loadID else { return }
            state = .failed(error.localizedDescription)
        }
    }

    func observe(latestProjectionAt: Date?) {
        guard let cutoff = snapshot?.stableCutoff,
              let latestProjectionAt,
              latestProjectionAt > cutoff
        else { return }
        newDataAvailable = true
    }

    private func sortedTotals(dimension: String) -> [FactualReportTotal] {
        (snapshot?.factualTotals ?? [])
            .filter { $0.dimension == dimension }
            .sorted(by: Self.totalOrder)
    }

    private static func totalOrder(_ left: FactualReportTotal, _ right: FactualReportTotal) -> Bool {
        if left.estimatedSeconds != right.estimatedSeconds {
            return left.estimatedSeconds > right.estimatedSeconds
        }
        return left.key < right.key
    }

    private static func isPartial(_ snapshot: FactualReportSnapshot) -> Bool {
        let evidence = snapshot.coverage.evidenceSeconds
        return evidence.protected > 0 || evidence.paused > 0 || evidence.unavailable > 0 ||
            evidence.error > 0 || evidence.gap > 0
    }
}

enum HomeReportFormatter {
    static func clockTime(_ date: Date, timeZone: TimeZone, locale: Locale = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

private extension UInt64 {
    func saturatingSubtract(_ value: UInt64) -> UInt64 {
        value > self ? 0 : self - value
    }
}
