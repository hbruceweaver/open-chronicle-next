import SwiftUI

struct ModeStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            Text("Choose how long observation should run")
                .font(.largeTitle.weight(.semibold))
            Text("You can change cadence later. A study always keeps its explicit end boundary.")
                .foregroundStyle(.secondary)

            Picker("Observation mode", selection: $model.draft.recordingMode) {
                Text("Personal — stays on until paused").tag(OnboardingRecordingMode.personal)
                Text("Study — stops at a chosen time").tag(OnboardingRecordingMode.study)
            }
            .pickerStyle(.radioGroup)

            if model.draft.recordingMode == .study {
                DatePicker(
                    "Study ends",
                    selection: $model.draft.studyEnd,
                    in: Date().addingTimeInterval(60) ... Date.distantFuture,
                    displayedComponents: [.date, .hourAndMinute]
                )
                Text("Open Chronicle will not silently convert an expired study into personal mode.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Picker("Observation cadence", selection: $model.draft.cadenceSeconds) {
                Text("Every 60 seconds — lighter").tag(UInt32(60))
                Text("Every 30 seconds — more detail").tag(UInt32(30))
            }
            .pickerStyle(.radioGroup)
        }
    }
}
