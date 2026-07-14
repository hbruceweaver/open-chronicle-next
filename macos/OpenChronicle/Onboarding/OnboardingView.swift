import AppKit
import SwiftUI

struct OnboardingView: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        Group {
            if let issue = model.restoreIssue {
                VStack(alignment: .leading, spacing: 18) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 42))
                        .foregroundStyle(.orange)
                    Text("Setup progress needs repair")
                        .font(.largeTitle.weight(.semibold))
                    Text(issue).foregroundStyle(.secondary)
                    Text(
                        "Restarting setup removes only the saved onboarding progress. "
                            + "It does not delete Chronicle evidence, grants, or integration receipts."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    Button("Restart Setup Progress") { model.restartSetupProgress() }
                        .buttonStyle(.borderedProminent)
                }
                .frame(maxWidth: 620, alignment: .leading)
                .padding(48)
            } else {
                HStack(spacing: 0) {
                    progressSidebar
                        .frame(width: 220)
                        .background(Color(nsColor: .controlBackgroundColor))
                    Divider()
                    VStack(spacing: 0) {
                        ScrollView {
                            stepContent
                                .frame(maxWidth: 680, alignment: .leading)
                                .padding(44)
                        }
                        Divider()
                        footer
                    }
                }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            model.refreshPermission()
        }
    }

    private var progressSidebar: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Open Chronicle", systemImage: "clock.arrow.circlepath")
                .font(.headline)
                .padding(.bottom, 14)
            ForEach(Array(OnboardingStep.allCases.enumerated()), id: \.element.id) { index, step in
                Button {
                    model.navigate(to: step)
                } label: {
                    HStack(spacing: 9) {
                        Image(systemName: progressSymbol(index: index))
                            .foregroundStyle(progressColor(index: index))
                            .frame(width: 18)
                        Text(step.title)
                            .foregroundStyle(index <= model.furthestStepIndex ? .primary : .tertiary)
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!model.canNavigate(to: step))
                .accessibilityLabel("Setup step \(index + 1): \(step.title)")
                .accessibilityValue(index == model.currentStepIndex ? "Current" : "")
            }
            Spacer()
            Text("Setup progress is saved on this Mac.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(24)
    }

    @ViewBuilder
    private var stepContent: some View {
        switch model.currentStep {
        case .welcome:
            WelcomeStep()
        case .mode:
            ModeStep(model: model)
        case .privacy:
            PrivacyStep(model: model)
        case .permission:
            PermissionStep(model: model)
        case .captureProof:
            CaptureProofStep(model: model)
        case .login:
            LoginStep(model: model)
        case .agent:
            AgentStep(model: model)
        case .completion:
            CompletionStep(model: model)
        }
    }

    private var footer: some View {
        HStack {
            Button("Back") { model.goBack() }
                .disabled(!model.canGoBack)
            Spacer()
            if model.currentStep == .completion {
                Button(model.isFinishing ? "Starting…" : "Start Open Chronicle") {
                    Task { await model.finish() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!model.canAdvance)
            } else {
                Button("Continue") { model.advance() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.canAdvance)
            }
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 18)
    }

    private func progressSymbol(index: Int) -> String {
        if index < model.currentStepIndex { return "checkmark.circle.fill" }
        if index == model.currentStepIndex { return "circle.inset.filled" }
        return "circle"
    }

    private func progressColor(index: Int) -> Color {
        index <= model.currentStepIndex ? .blue : .secondary
    }
}

private struct CaptureProofStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Prove the capture path")
                .font(.largeTitle.weight(.semibold))
            Text(
                "Open Chronicle will bring a synthetic test window to the front, capture only that window, "
                    + "and verify a fixed phrase with Apple Vision. The test image and OCR are discarded."
            )
            .foregroundStyle(.secondary)

            Button(model.proofState == .running ? "Testing…" : "Run Capture Test") {
                Task { await model.runCaptureProof() }
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.proofState == .running || !model.permissionGranted)

            switch model.proofState {
            case .notRun:
                EmptyView()
            case .running:
                ProgressView("Capturing the synthetic window and running local OCR…")
            case .passed:
                Label("Exact-window capture and local OCR succeeded", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            case .failed:
                Label(
                    model.proofFailure?.message ?? "The capture test failed.",
                    systemImage: "exclamationmark.triangle.fill"
                )
                .foregroundStyle(.orange)
            }
        }
    }
}

private struct LoginStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Keep observation available")
                .font(.largeTitle.weight(.semibold))
            Text("Personal use works best when Open Chronicle starts quietly after you sign in. Studies still stop at their exact end time.")
                .foregroundStyle(.secondary)
            Toggle("Launch Open Chronicle when I sign in", isOn: $model.draft.launchAtLogin)
            Text("You can change this later in Settings. macOS may require approval under Login Items.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

private struct AgentStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("AI access is optional")
                .font(.largeTitle.weight(.semibold))
            Text(
                "Recording and reports work without Claude or Codex. When you connect an AI client, "
                    + "Open Chronicle will require a time-limited disclosure grant before returning OCR or event details."
            )
            .foregroundStyle(.secondary)
            Toggle("Finish recording setup now; connect an AI client later", isOn: $model.draft.deferAgentSetup)
                .toggleStyle(.checkbox)
            Text("No global agent configuration is edited during this step.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

private struct CompletionStep: View {
    @ObservedObject var model: OnboardingModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Ready to observe")
                .font(.largeTitle.weight(.semibold))
            Label("Privacy explanation acknowledged", systemImage: "checkmark.circle.fill")
            Label("Screen Recording permission available", systemImage: "checkmark.circle.fill")
            Label("Exact-window capture and local OCR proven", systemImage: "checkmark.circle.fill")
            Text(model.draft.recordingMode == .personal ? "Personal mode" : "Bounded study mode")
                .font(.headline)
            if let warning = model.nonBlockingWarning {
                Label(warning, systemImage: "exclamationmark.circle")
                    .foregroundStyle(.orange)
            }
            if let error = model.finishError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
            Text("Starting commits these choices to the local Chronicle core before observation begins.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
