import SwiftUI

struct PermissionStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Allow Screen Recording")
                .font(.largeTitle.weight(.semibold))
            Text(
                "macOS calls this permission Screen Recording even though Open Chronicle "
                    + "captures only the exact foreground window selected by its privacy checks."
            )
            .foregroundStyle(.secondary)

            Label(
                model.permissionGranted ? "Permission available" : "Permission still required",
                systemImage: model.permissionGranted
                    ? "checkmark.circle.fill"
                    : "exclamationmark.circle.fill"
            )
            .foregroundStyle(model.permissionGranted ? .green : .orange)
            .font(.headline)

            if !model.permissionGranted {
                HStack {
                    Button("Request Permission") { model.requestPermission() }
                        .buttonStyle(.borderedProminent)
                    Button("Open System Settings") { model.openPermissionSettings() }
                }
                Text(
                    "After changing the switch in System Settings, return here. "
                        + "The next step performs a real one-time capture and local OCR test."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            } else {
                Text("Permission alone does not complete setup. The next step proves the complete safe-capture path.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}
