import SwiftUI

struct WelcomeStep: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            Image(systemName: "clock.arrow.circlepath")
                .font(.system(size: 44))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(.blue)
            Text("See where your workday actually goes")
                .font(.largeTitle.weight(.semibold))
            Text(
                "Open Chronicle observes the foreground window at a steady cadence, "
                    + "extracts factual text locally, and turns that evidence into a reviewable timeline."
            )
            .font(.title3)
            .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 12) {
                WelcomeBullet(
                    icon: "macwindow",
                    title: "Exact-window capture",
                    detail: "It attempts one foreground window, never the whole desktop."
                )
                WelcomeBullet(
                    icon: "text.viewfinder",
                    title: "Local OCR",
                    detail: "Apple Vision extracts text on this Mac; no model or API key is required."
                )
                WelcomeBullet(
                    icon: "chart.bar.xaxis",
                    title: "Facts before conclusions",
                    detail: "Five-minute evidence chunks remain inspectable before any later analysis."
                )
            }
        }
        .accessibilityElement(children: .contain)
    }
}

private struct WelcomeBullet: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: icon)
                .frame(width: 24)
                .foregroundStyle(.blue)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(detail).foregroundStyle(.secondary)
            }
        }
    }
}
