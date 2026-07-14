import Foundation

struct DeduplicationKey: Equatable, Sendable {
    let contentHash: String
    let bundleIdentifier: String
    let processName: String
    let windowTitle: String?
}

struct DeduplicatedContentReference: Equatable, Sendable {
    let key: DeduplicationKey
    let eventID: String
    let ocrEventID: String?
    let imageArtifactID: String?
}

actor ContentDeduplicator {
    private var lastDurableContent: DeduplicatedContentReference?

    func match(for key: DeduplicationKey) -> DeduplicatedContentReference? {
        guard lastDurableContent?.key == key else { return nil }
        return lastDurableContent
    }

    func latest() -> DeduplicatedContentReference? {
        lastDurableContent
    }

    /// Call only after Rust reports journal-durable canonical evidence. A lagging
    /// projection is still durable; only `not-durable` failures must not advance.
    func acknowledgeDurable(_ content: DeduplicatedContentReference) {
        lastDurableContent = content
    }
}
