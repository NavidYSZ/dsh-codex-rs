import SwiftUI

/// The pending approval: tool, summary, justification, "Einmal erlauben" / "Ablehnen".
struct ApprovalBanner: View {
    let approval: ApprovalMessage
    var onDecide: (String) -> Void

    private var title: String { L.lookup("chat.approval.title.\(approval.tool)") ?? L.chatApprovalTitleOther }
    private var icon: String {
        switch approval.tool {
        case "bash": return "terminal"
        case "edit", "write": return "pencil"
        default: return "wrench.and.screwdriver"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "shield.lefthalf.filled").foregroundStyle(Theme.warning)
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
            }
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: icon).font(.footnote).foregroundStyle(Theme.textMuted).padding(.top, 2)
                Text(approval.summary)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !approval.justification.isEmpty {
                Text(approval.justification).font(.footnote).foregroundStyle(Theme.textMuted).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(L.chatApprovalDeny) { Haptics.light(); onDecide("rejected") }
                    .buttonStyle(.bordered)
                Button(L.chatApprovalAllow) { Haptics.success(); onDecide("allowed-once") }
                    .buttonStyle(.borderedProminent)
            }
            .controlSize(.regular)
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .padding(12)
        .background(Theme.warning.opacity(0.12), in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous).stroke(Theme.warning.opacity(0.4)))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
    }
}
