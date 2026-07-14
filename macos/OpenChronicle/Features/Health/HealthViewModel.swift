import Foundation

enum OperationalStorageState: Equatable, Sendable {
    case healthy
    case warning
    case blocked
}

@MainActor
final class HealthViewModel: ObservableObject {
    static let gibibyte = UInt64(1_073_741_824)
    static let warningAvailableBytes = 4 * gibibyte
    static let minimumAvailableBytes = 2 * gibibyte
    static let managedImageQuotaBytes = 20 * gibibyte

    @Published private(set) var snapshot: DiagnosticHealthSnapshot?
    @Published private(set) var lastError: String?
    @Published private(set) var isRefreshing = false
    private var fetcher: (any DiagnosticHealthFetching)?

    func attach(fetcher: any DiagnosticHealthFetching) {
        self.fetcher = fetcher
    }

    func refresh(at date: Date = Date()) async {
        guard let fetcher else { return }
        isRefreshing = true
        defer { isRefreshing = false }
        do {
            apply(try await fetcher.fetch(at: date))
        } catch {
            fail(error.localizedDescription)
        }
    }

    func apply(_ snapshot: DiagnosticHealthSnapshot) {
        self.snapshot = snapshot
        lastError = nil
    }

    func fail(_ message: String) {
        lastError = message
    }

    static func storageState(for summary: DiagnosticStorageSummary) -> OperationalStorageState {
        if summary.availableBytes < minimumAvailableBytes
            || summary.managedBytes >= managedImageQuotaBytes
        {
            return .blocked
        }
        if summary.availableBytes < warningAvailableBytes {
            return .warning
        }
        return .healthy
    }
}
