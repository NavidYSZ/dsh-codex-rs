import SwiftUI
import UIKit

/// The design tokens of SPEC §1. Values come from `brand/colors.json` when the brand package has been copied
/// into the bundle as `Resources/Brand/colors.json`; otherwise the values below (a copy of `brand/colors.json`) apply.
/// Typography is the system font; the wordmark is the system font at weight 650 with tight tracking.
enum Theme {
    enum Token: String, CaseIterable {
        case accent, accentSoft, online, warning, danger, bg, surface, surfaceRaised, border, text, textMuted
    }

    private static let provisional: [Token: (light: String, dark: String)] = [
        .accent: ("#3E62F0", "#8AA2FF"),
        .accentSoft: ("#E8EDFF", "#1D2240"),
        .online: ("#0F8A68", "#5EEAD4"),
        .warning: ("#B97400", "#F5B544"),
        .danger: ("#C43D3D", "#FF7B7B"),
        .bg: ("#FAFAFC", "#0E0F14"),
        .surface: ("#FFFFFF", "#161822"),
        .surfaceRaised: ("#F2F3F8", "#1E2130"),
        .border: ("#E3E5EE", "#2A2E40"),
        .text: ("#121420", "#F2F3F8"),
        .textMuted: ("#646980", "#9AA0B8"),
    ]

    /// `{"light": {token: hex}, "dark": {…}}` from the brand package, if bundled.
    private static let palette: [Token: (light: String, dark: String)] = {
        var out = provisional
        guard let url = Bundle.main.url(forResource: "colors", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let json = try? JSONDecoder().decode([String: [String: String]].self, from: data) else { return out }
        for token in Token.allCases {
            if let l = json["light"]?[token.rawValue], let d = json["dark"]?[token.rawValue] { out[token] = (l, d) }
        }
        return out
    }()

    static func color(_ token: Token) -> Color {
        let pair = palette[token] ?? provisional[token]!
        return Color(uiColor: UIColor { traits in
            UIColor(hex: traits.userInterfaceStyle == .dark ? pair.dark : pair.light)
        })
    }

    static var accent: Color { color(.accent) }
    static var accentSoft: Color { color(.accentSoft) }
    static var online: Color { color(.online) }
    static var warning: Color { color(.warning) }
    static var danger: Color { color(.danger) }
    static var bg: Color { color(.bg) }
    static var surface: Color { color(.surface) }
    static var surfaceRaised: Color { color(.surfaceRaised) }
    static var border: Color { color(.border) }
    static var text: Color { color(.text) }
    static var textMuted: Color { color(.textMuted) }

    static let cornerRadius: CGFloat = 14
    static let rowRadius: CGFloat = 10
}

extension UIColor {
    convenience init(hex: String) {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        var v: UInt64 = 0
        Scanner(string: s).scanHexInt64(&v)
        let r, g, b, a: CGFloat
        if s.count == 8 {
            r = CGFloat((v >> 24) & 0xFF) / 255; g = CGFloat((v >> 16) & 0xFF) / 255; b = CGFloat((v >> 8) & 0xFF) / 255; a = CGFloat(v & 0xFF) / 255
        } else {
            r = CGFloat((v >> 16) & 0xFF) / 255; g = CGFloat((v >> 8) & 0xFF) / 255; b = CGFloat(v & 0xFF) / 255; a = 1
        }
        self.init(red: r, green: g, blue: b, alpha: a)
    }
}

/// The mark: a ring of upper air with a glowing core, drawn with shapes so it works at any size.
struct AetherMark: View {
    var size: CGFloat = 64
    var tint: Color = Theme.accent

    var body: some View {
        ZStack {
            Circle()
                .fill(tint.opacity(0.12))
            Circle()
                .trim(from: 0.07, to: 0.93)
                .rotation(.degrees(90))
                .stroke(tint, style: StrokeStyle(lineWidth: size * 0.085, lineCap: .round))
                .padding(size * 0.2)
            Circle()
                .fill(tint)
                .frame(width: size * 0.22, height: size * 0.22)
            Circle()
                .fill(tint)
                .frame(width: size * 0.09, height: size * 0.09)
                .offset(y: size * 0.3)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// The wordmark: the system font at weight 650 with tight tracking.
struct AetherWordmark: View {
    var size: CGFloat = 28

    var body: some View {
        Text(verbatim: "Aether")
            .font(.system(size: size, weight: .semibold, design: .default))
            .fontWeight(.semibold)
            .tracking(-size * 0.03)
            .foregroundStyle(Theme.text)
    }
}

/// A status dot with a VoiceOver label.
struct StatusDot: View {
    var color: Color
    var label: String
    var pulsing = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 9, height: 9)
            .overlay {
                if pulsing {
                    Circle().stroke(color.opacity(0.4), lineWidth: 2).scaleEffect(1.8)
                }
            }
            .accessibilityLabel(label)
    }
}

/// A rounded chip with a small label (transport, counts).
struct Chip: View {
    var text: String
    var icon: String?
    var tint: Color = Theme.textMuted
    var filled = false

    var body: some View {
        HStack(spacing: 4) {
            if let icon { Image(systemName: icon).font(.caption2) }
            Text(text).font(.caption.weight(.medium))
        }
        .foregroundStyle(filled ? Color.white : tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(filled ? tint : tint.opacity(0.12), in: Capsule())
    }
}

/// Haptics on the moments that matter: send, approve, errors.
enum Haptics {
    @MainActor static func send() { UIImpactFeedbackGenerator(style: .medium).impactOccurred() }
    @MainActor static func success() { UINotificationFeedbackGenerator().notificationOccurred(.success) }
    @MainActor static func warning() { UINotificationFeedbackGenerator().notificationOccurred(.warning) }
    @MainActor static func light() { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
}
