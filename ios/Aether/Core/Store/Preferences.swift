import Foundation
import SwiftUI

/// Preferences only (UserDefaults): appearance, the follow-up rule, developer switches, the welcome flag.
@MainActor @Observable
final class Preferences {
    enum Appearance: String, CaseIterable { case system, light, dark }
    enum Followup: String, CaseIterable { case steer, queue }

    private let defaults: UserDefaults

    var appearance: Appearance { didSet { defaults.set(appearance.rawValue, forKey: "appearance") } }
    var followup: Followup { didSet { defaults.set(followup.rawValue, forKey: "followup") } }
    var hubURL: String { didSet { defaults.set(hubURL, forKey: "hubURL") } }
    var allowPlainHTTP: Bool { didSet { defaults.set(allowPlainHTTP, forKey: "allowPlainHTTP") } }
    var hasSeenWelcome: Bool { didSet { defaults.set(hasSeenWelcome, forKey: "hasSeenWelcome") } }
    /// Last project per device (path; "" = no project).
    var lastProject: [String: String] { didSet { defaults.set(lastProject, forKey: "lastProject") } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        appearance = Appearance(rawValue: defaults.string(forKey: "appearance") ?? "") ?? .system
        followup = Followup(rawValue: defaults.string(forKey: "followup") ?? "") ?? .steer
        hubURL = defaults.string(forKey: "hubURL") ?? Preferences.defaultHubURL
        allowPlainHTTP = defaults.object(forKey: "allowPlainHTTP") as? Bool ?? Preferences.defaultAllowPlainHTTP
        hasSeenWelcome = defaults.bool(forKey: "hasSeenWelcome")
        lastProject = defaults.dictionary(forKey: "lastProject") as? [String: String] ?? [:]
    }

    var colorScheme: ColorScheme? {
        switch appearance {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }

    /// The simulator talks to a dev server over plain http; a real phone must opt in.
    static var defaultAllowPlainHTTP: Bool {
        #if targetEnvironment(simulator)
        return true
        #else
        return false
        #endif
    }

    static var defaultHubURL: String {
        #if targetEnvironment(simulator)
        return "http://127.0.0.1:8787"
        #else
        return ""
        #endif
    }
}
