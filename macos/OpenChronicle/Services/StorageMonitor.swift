import Foundation

enum StorageMonitorUpdate: Equatable, Sendable {
    case snapshot(DiagnosticHealthSnapshot)
    case failed(String)
}

actor StorageMonitor {
    typealias UpdateSink = @Sendable (StorageMonitorUpdate) async -> Void

    private let fetcher: any DiagnosticHealthFetching
    private let intervalNanoseconds: UInt64
    private let updateSink: UpdateSink
    private var loopTask: Task<Void, Never>?

    init(
        fetcher: any DiagnosticHealthFetching,
        intervalNanoseconds: UInt64 = 60_000_000_000,
        updateSink: @escaping UpdateSink
    ) {
        self.fetcher = fetcher
        self.intervalNanoseconds = intervalNanoseconds
        self.updateSink = updateSink
    }

    func start() {
        guard loopTask == nil else { return }
        loopTask = Task { [weak self] in
            await self?.run()
        }
    }

    func stop() async {
        let task = loopTask
        loopTask = nil
        task?.cancel()
        await task?.value
    }

    func refresh(at date: Date = Date()) async {
        do {
            await updateSink(.snapshot(try await fetcher.fetch(at: date)))
        } catch {
            await updateSink(.failed(error.localizedDescription))
        }
    }

    private func run() async {
        while !Task.isCancelled {
            await refresh()
            do {
                try await Task.sleep(nanoseconds: intervalNanoseconds)
            } catch {
                return
            }
        }
    }
}
