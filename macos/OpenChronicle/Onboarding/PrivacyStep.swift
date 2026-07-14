import SwiftUI

struct PrivacyStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Your evidence stays under your control")
                .font(.largeTitle.weight(.semibold))
            PrivacyFact(
                title: "Screenshots remain local",
                detail: "Initial screenshot retention is 24 hours. Expiry removes image bytes without deleting the factual event history."
            )
            PrivacyFact(
                title: "OCR is separate evidence",
                detail: "Recognized text and window metadata can outlive an image so the timeline remains queryable."
            )
            PrivacyFact(
                title: "Sensitive surfaces are skipped",
                detail: "Password managers, secure input, the lock screen, and Open Chronicle itself are denied before pixels are retained."
            )
            PrivacyFact(
                title: "AI access is explicit",
                detail: "An AI client receives nothing until you create a limited disclosure grant. Screenshot bytes are never exposed through MCP."
            )

            Toggle(
                "I understand what is retained and what is excluded",
                isOn: $model.draft.privacyAcknowledged
            )
            .toggleStyle(.checkbox)
            .padding(.top, 8)
        }
    }
}

private struct PrivacyFact: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.headline)
            Text(detail).foregroundStyle(.secondary)
        }
    }
}
