import SwiftUI

/// The chat's current plan above the composer: the latest to-do list as one line with the item in progress and
/// "1 von 3 erledigt", unfolding into the checklist. It stays while items are left; a finished list goes when its turn ends.
struct PlanBar: View {
    let transcript: Transcript
    let running: Bool
    @State private var open = false

    private var plan: (todos: [Todo], done: Int)? {
        guard let todos = transcript.todos, !todos.isEmpty else { return nil }
        let done = todos.filter { $0.status == "completed" }.count
        if done < todos.count { return (todos, done) }
        guard running, let last = transcript.turns.last, transcript.todosTurn == last.turn else { return nil }
        return (todos, done)
    }

    var body: some View {
        if let plan {
            let now = plan.todos.first { $0.status == "in_progress" } ?? plan.todos.first { $0.status == "pending" }
            VStack(alignment: .leading, spacing: 8) {
                Button { withAnimation(.snappy) { open.toggle() } } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "list.bullet").font(.footnote).foregroundStyle(Theme.accent)
                        Text(L.chatPlan).font(.caption.weight(.semibold)).foregroundStyle(Theme.text)
                        Text(now?.content ?? L.chatPlanAllDone).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1)
                        Spacer(minLength: 4)
                        Text(L.chatPlanDone(plan.done, plan.todos.count)).font(.caption).foregroundStyle(Theme.textMuted).monospacedDigit()
                        Image(systemName: "chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(open ? 180 : 0))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(L.chatPlan): \(now?.content ?? L.chatPlanAllDone), \(L.chatPlanDone(plan.done, plan.todos.count))")
                if open { TodoList(todos: plan.todos) }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous))
        }
    }
}
