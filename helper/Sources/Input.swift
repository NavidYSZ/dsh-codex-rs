import AppKit
import Carbon.HIToolbox

/// Marks every event this helper posts, so the Esc monitor can tell the
/// agent's own key presses from the user's.
let syntheticEventTag: Int64 = 0x44534843  // "DSHC"

enum InputError: LocalizedError {
    case unknownKey(String)
    case noAccessibility
    var errorDescription: String? {
        switch self {
        case .unknownKey(let k): return "unknown key name: \(k)"
        case .noAccessibility: return "Accessibility permission missing for DSH Computer Use"
        }
    }
}

final class Input {
    private let source = CGEventSource(stateID: .hidSystemState)

    private func post(_ event: CGEvent?) {
        guard let event else { return }
        event.setIntegerValueField(.eventSourceUserData, value: syntheticEventTag)
        event.post(tap: .cghidEventTap)
    }

    var cursorPosition: CGPoint { CGEvent(source: nil)?.location ?? .zero }

    func move(to p: CGPoint) {
        post(CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: p, mouseButton: .left))
    }

    func click(at p: CGPoint, button: String, count: Int, modifiers: CGEventFlags) {
        let (down, up, btn): (CGEventType, CGEventType, CGMouseButton) = {
            switch button {
            case "right": return (.rightMouseDown, .rightMouseUp, .right)
            case "middle": return (.otherMouseDown, .otherMouseUp, .center)
            default: return (.leftMouseDown, .leftMouseUp, .left)
            }
        }()
        move(to: p)
        for i in 1...max(1, count) {
            for type in [down, up] {
                let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: btn)
                e?.setIntegerValueField(.mouseEventClickState, value: Int64(i))
                e?.flags = modifiers
                post(e)
                usleep(12_000)
            }
            if i < count { usleep(40_000) }
        }
    }

    func drag(from a: CGPoint, to b: CGPoint, steps: Int = 24) {
        move(to: a)
        post(CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: a, mouseButton: .left))
        usleep(60_000)
        for s in 1...steps {
            let t = CGFloat(s) / CGFloat(steps)
            let p = CGPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t)
            post(CGEvent(mouseEventSource: source, mouseType: .leftMouseDragged, mouseCursorPosition: p, mouseButton: .left))
            usleep(10_000)
        }
        post(CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: b, mouseButton: .left))
    }

    func scroll(at p: CGPoint, dx: Int32, dy: Int32) {
        move(to: p)
        usleep(20_000)
        // Positive dy scrolls content down (towards the end), like a trackpad swipe up.
        post(CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                     wheel1: -dy, wheel2: -dx, wheel3: 0))
    }

    func type(_ text: String) {
        var pending = ""
        func flush() {
            guard !pending.isEmpty else { return }
            let units = Array(pending.utf16)
            var offset = 0
            while offset < units.count {
                let slice = Array(units[offset..<min(offset + 16, units.count)])
                for keyDown in [true, false] {
                    let e = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: keyDown)
                    slice.withUnsafeBufferPointer { e?.keyboardSetUnicodeString(stringLength: slice.count, unicodeString: $0.baseAddress) }
                    post(e)
                }
                usleep(14_000)
                offset += 16
            }
            pending = ""
        }
        for ch in text {
            if ch == "\n" || ch == "\r" { flush(); tap(CGKeyCode(kVK_Return), []) }
            else if ch == "\t" { flush(); tap(CGKeyCode(kVK_Tab), []) }
            else { pending.append(ch) }
        }
        flush()
    }

    /// "cmd+shift+t", "Return", "ctrl+a", "F5" …
    func key(_ combo: String) throws {
        let parts = combo.split(separator: "+").map { $0.trimmingCharacters(in: .whitespaces) }
        guard let keyName = parts.last, !keyName.isEmpty else { throw InputError.unknownKey(combo) }
        var flags: CGEventFlags = []
        for mod in parts.dropLast() {
            switch mod.lowercased() {
            case "cmd", "command", "meta", "super", "⌘": flags.insert(.maskCommand)
            case "shift", "⇧": flags.insert(.maskShift)
            case "alt", "option", "opt", "⌥": flags.insert(.maskAlternate)
            case "ctrl", "control", "⌃": flags.insert(.maskControl)
            case "fn": flags.insert(.maskSecondaryFn)
            default: throw InputError.unknownKey(String(mod))
            }
        }
        guard let (code, needsShift) = Keyboard.shared.keyCode(for: keyName) else { throw InputError.unknownKey(keyName) }
        if needsShift { flags.insert(.maskShift) }
        tap(code, flags)
    }

    private func tap(_ code: CGKeyCode, _ flags: CGEventFlags) {
        for keyDown in [true, false] {
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: keyDown)
            e?.flags = flags
            post(e)
            usleep(10_000)
        }
    }

    static func modifiers(_ names: [String]) -> CGEventFlags {
        var flags: CGEventFlags = []
        for n in names {
            switch n.lowercased() {
            case "cmd", "command", "meta", "super": flags.insert(.maskCommand)
            case "shift": flags.insert(.maskShift)
            case "alt", "option", "opt": flags.insert(.maskAlternate)
            case "ctrl", "control": flags.insert(.maskControl)
            default: break
            }
        }
        return flags
    }
}

/// Key names to virtual key codes. Character keys are resolved through the
/// active keyboard layout, so "cmd+z" means the key that types "z" on a
/// German QWERTZ layout as well.
final class Keyboard {
    static let shared = Keyboard()

    private let named: [String: Int] = [
        "return": kVK_Return, "enter": kVK_Return, "tab": kVK_Tab, "space": kVK_Space,
        "delete": kVK_Delete, "backspace": kVK_Delete, "forwarddelete": kVK_ForwardDelete,
        "escape": kVK_Escape, "esc": kVK_Escape,
        "left": kVK_LeftArrow, "right": kVK_RightArrow, "up": kVK_UpArrow, "down": kVK_DownArrow,
        "arrowleft": kVK_LeftArrow, "arrowright": kVK_RightArrow, "arrowup": kVK_UpArrow, "arrowdown": kVK_DownArrow,
        "home": kVK_Home, "end": kVK_End, "pageup": kVK_PageUp, "pagedown": kVK_PageDown,
        "f1": kVK_F1, "f2": kVK_F2, "f3": kVK_F3, "f4": kVK_F4, "f5": kVK_F5, "f6": kVK_F6,
        "f7": kVK_F7, "f8": kVK_F8, "f9": kVK_F9, "f10": kVK_F10, "f11": kVK_F11, "f12": kVK_F12,
        "keypadenter": kVK_ANSI_KeypadEnter, "help": kVK_Help,
    ]

    private var charMap: [String: (CGKeyCode, Bool)] = [:]
    private var layoutID: String = ""

    func keyCode(for name: String) -> (CGKeyCode, Bool)? {
        if let code = named[name.lowercased()] { return (CGKeyCode(code), false) }
        guard name.count == 1 else { return nil }
        refreshLayoutIfNeeded()
        if let hit = charMap[name] { return hit }
        if let hit = charMap[name.lowercased()] { return hit }
        return nil
    }

    private func refreshLayoutIfNeeded() {
        guard let src = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue() else { return }
        let id = (TISGetInputSourceProperty(src, kTISPropertyInputSourceID)).map {
            Unmanaged<CFString>.fromOpaque($0).takeUnretainedValue() as String
        } ?? ""
        if id == layoutID && !charMap.isEmpty { return }
        layoutID = id
        charMap = [:]
        guard let raw = TISGetInputSourceProperty(src, kTISPropertyUnicodeKeyLayoutData) else { return }
        let data = Unmanaged<CFData>.fromOpaque(raw).takeUnretainedValue() as Data
        data.withUnsafeBytes { buffer in
            guard let layout = buffer.bindMemory(to: UCKeyboardLayout.self).baseAddress else { return }
            for shifted in [false, true] {
                for code in 0..<128 {
                    var dead: UInt32 = 0
                    var length = 0
                    var chars = [UniChar](repeating: 0, count: 4)
                    let mods: UInt32 = shifted ? UInt32(shiftKey >> 8) & 0xFF : 0
                    let status = UCKeyTranslate(layout, UInt16(code), UInt16(kUCKeyActionDown), mods,
                                                UInt32(LMGetKbdType()), OptionBits(kUCKeyTranslateNoDeadKeysBit),
                                                &dead, chars.count, &length, &chars)
                    guard status == noErr, length > 0 else { continue }
                    let s = String(utf16CodeUnits: chars, count: length)
                    if charMap[s] == nil { charMap[s] = (CGKeyCode(code), shifted) }
                }
            }
        }
    }
}
