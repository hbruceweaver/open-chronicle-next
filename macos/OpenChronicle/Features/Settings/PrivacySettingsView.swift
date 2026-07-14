import SwiftUI

struct PrivacySettingsView: View {
    @ObservedObject var model: SettingsViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SettingsCard(title: "What stays local", symbol: "internaldrive") {
                PrivacyStatement(
                    title: "Screenshots remain on this Mac",
                    detail: "Chronicle stores retained image bytes only in its local managed storage. MCP cannot return screenshot bytes or managed paths."
                )
                PrivacyStatement(
                    title: "OCR can outlive a screenshot",
                    detail: "Screenshot expiry removes image bytes. On-device recognized text, factual events, and five-minute chunks remain until you delete Chronicle evidence."
                )
                PrivacyStatement(
                    title: "AI access requires a separate grant",
                    detail: "Installing or detecting Claude or Codex grants nothing. Each client receives only the time, content classes, expiry, and volume you approve. OCR starts off."
                )
            }

            if let exclusions = model.snapshot?.exclusions {
                SettingsCard(title: "Sensitive-surface exclusions", symbol: "eye.slash") {
                    Label(
                        "Built-in policy \(exclusions.policyVersion) is active",
                        systemImage: "checkmark.shield"
                    )
                    Text("These application identifiers are denied before pixels or OCR are retained:")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    ForEach(exclusions.builtInBundleIdentifiers, id: \.self) { identifier in
                        Label(identifier, systemImage: "app.badge.checkmark")
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                    }
                    if !exclusions.builtInTitleFragments.isEmpty {
                        Divider()
                        Text("Built-in title fragments")
                            .font(.subheadline.weight(.semibold))
                        ForEach(exclusions.builtInTitleFragments, id: \.self) { fragment in
                            Text(verbatim: fragment).font(.caption.monospaced())
                        }
                    }
                    Divider()
                    Label(
                        "Custom exclusions are not yet available in this MVP build",
                        systemImage: "hammer"
                    )
                    .font(.callout.weight(.semibold))
                    Text("No editable exclusion field is shown because the current capture runtime has no durable custom-exclusion control. Chronicle will not pretend an unsupported value was saved.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .accessibilityLabel(
                            "Custom exclusions unavailable. Only the listed built-in exclusions are active."
                        )
                }
            }

            SettingsCard(title: "What Chronicle never captures", symbol: "keyboard.badge.ellipsis") {
                Text("Chronicle does not record keystrokes, clipboard contents, click coordinates, hidden windows, microphone audio, or a raw input-event stream.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text("Secure input, lock screen state, Chronicle itself, and ambiguous foreground windows produce protected or unavailable evidence states without sensitive pixels or text.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct PrivacyStatement: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.subheadline.weight(.semibold))
            Text(detail).font(.callout).foregroundStyle(.secondary)
        }
    }
}
