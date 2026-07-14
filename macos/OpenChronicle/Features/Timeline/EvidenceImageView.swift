import AppKit
import SwiftUI

struct EvidenceImageView: View {
    private enum ImageLoadState {
        case idle
        case loading
        case loaded(NSImage)
        case failed(String)
    }

    @ObservedObject var model: TimelineViewModel
    let metadata: TimelineImageMetadata
    @State private var state: ImageLoadState = .idle

    init(model: TimelineViewModel, metadata: TimelineImageMetadata) {
        self.model = model
        self.metadata = metadata
    }

    var body: some View {
        Group {
            if !metadata.isRetained {
                unavailableState
            } else {
                switch state {
                case .idle, .loading:
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Loading the retained local screenshot…")
                    }
                    .frame(maxWidth: .infinity, minHeight: 160)
                case let .loaded(image):
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFit()
                        .frame(maxHeight: 520)
                        .accessibilityLabel("Locally retained evidence screenshot")
                case let .failed(message):
                    RetryUnavailableView(
                        title: "Screenshot unavailable",
                        symbol: "photo.badge.exclamationmark",
                        detail: message,
                        actionTitle: "Retry local screenshot",
                        accessibilityHint: "Retries the bounded read from local retained storage.",
                        minimumHeight: 160
                    ) { Task { await load() } }
                }
            }
        }
        .padding(12)
        .background(.black.opacity(0.04), in: RoundedRectangle(cornerRadius: 12))
        .task(id: metadata.id) {
            await load()
        }
    }

    private var unavailableState: some View {
        ContentUnavailableView(
            imageTitle,
            systemImage: imageSymbol,
            description: Text(imageDescription)
        )
        .frame(maxWidth: .infinity, minHeight: 160)
        .accessibilityLabel("Screenshot \(metadata.state.replacingOccurrences(of: "-", with: " "))")
    }

    private var imageTitle: String {
        switch metadata.state {
        case "expired": "Screenshot expired"
        case "user-deleted": "Screenshot deleted"
        case "delete-pending": "Screenshot deletion pending"
        case "missing": "Screenshot missing"
        case "write-failed": "Screenshot was not saved"
        default: "Screenshot unavailable"
        }
    }

    private var imageSymbol: String {
        switch metadata.state {
        case "expired": "clock.badge.xmark"
        case "user-deleted", "delete-pending": "trash"
        case "missing": "questionmark.folder"
        default: "photo.badge.exclamationmark"
        }
    }

    private var imageDescription: String {
        switch metadata.state {
        case "expired": "Only factual text and metadata remain after local retention expiry."
        case "user-deleted": "The user removed these local screenshot bytes."
        case "delete-pending": "The local screenshot is being removed and will not be opened."
        case "missing": "The evidence record exists, but the managed screenshot artifact is missing."
        case "write-failed": "Capture metadata was retained, but the screenshot write did not complete."
        default: "No local screenshot bytes are available for this event."
        }
    }

    private func load() async {
        guard metadata.isRetained else { return }
        state = .loading
        do {
            let data = try await model.image(metadata)
            guard let image = NSImage(data: data) else {
                state = .failed("The bounded artifact bytes were not a supported image.")
                return
            }
            state = .loaded(image)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }
}
