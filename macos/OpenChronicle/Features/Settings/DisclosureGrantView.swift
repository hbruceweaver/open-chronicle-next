import SwiftUI

struct DisclosureGrantView: View {
    let clientName: String
    let title: String
    let saveTitle: String
    let onSave: (SettingsGrantDraft) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var draft: SettingsGrantDraft
    @FocusState private var focusedField: Field?

    private enum Field: Hashable {
        case horizon
        case expiry
        case pageLimit
    }

    init(
        clientName: String,
        title: String,
        saveTitle: String,
        draft: SettingsGrantDraft,
        onSave: @escaping (SettingsGrantDraft) -> Void
    ) {
        self.clientName = clientName
        self.title = title
        self.saveTitle = saveTitle
        self.onSave = onSave
        _draft = State(initialValue: draft)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 6) {
                Text(title).font(.title2.weight(.semibold))
                Text("Choose exactly what \(clientName) can read from Chronicle. This grant is revocable and never includes screenshot bytes or local file paths.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Form {
                Picker("History available", selection: $draft.horizon) {
                    ForEach(GrantHorizonOption.allCases) { option in
                        Text(option.label).tag(option)
                    }
                }
                .focused($focusedField, equals: .horizon)

                Picker("Grant expires after", selection: $draft.expiry) {
                    ForEach(GrantExpiryOption.allCases) { option in
                        Text(option.label).tag(option)
                    }
                }
                .focused($focusedField, equals: .expiry)

                Toggle("Allow recognized on-screen text (OCR)", isOn: $draft.allowOCR)
                Text(draft.allowOCR
                    ? "OCR is enabled for this grant. It can contain sensitive text seen in privacy-approved work windows."
                    : "OCR is off. The client receives factual metadata and derived five-minute work chunks only.")
                    .font(.caption)
                    .foregroundStyle(draft.allowOCR ? .orange : .secondary)

                Picker("Maximum items per page", selection: $draft.pageLimit) {
                    ForEach(GrantPageLimitOption.allCases) { option in
                        Text("\(option.rawValue)").tag(option)
                    }
                }
                .focused($focusedField, equals: .pageLimit)

                Picker("Maximum response size", selection: $draft.responseLimit) {
                    ForEach(GrantResponseLimitOption.allCases) { option in
                        Text(ByteCountFormatter.string(
                            fromByteCount: Int64(option.rawValue),
                            countStyle: .binary
                        )).tag(option)
                    }
                }

                Picker("Total disclosure allowance", selection: $draft.cumulativeLimit) {
                    ForEach(GrantCumulativeLimitOption.allCases) { option in
                        Text(ByteCountFormatter.string(
                            fromByteCount: Int64(option.rawValue),
                            countStyle: .binary
                        )).tag(option)
                    }
                }
            }
            .formStyle(.grouped)

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(saveTitle) {
                    onSave(draft)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 560)
        .onAppear { focusedField = .horizon }
    }
}
