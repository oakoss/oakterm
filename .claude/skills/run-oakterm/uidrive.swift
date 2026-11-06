// Drive a GUI process without bringing it to the front, so keys never land in
// whatever window the user has focused.
//   uidrive winid <pid>            print the process's largest window id (for screencapture -l)
//   uidrive key <pid> <spec>...    send keys: plain text, or a chord in braces such as
//                                  {cmd+f} {shift+enter} {ctrl+r} {esc} {enter} {bs} {tab}
import CoreGraphics
import Foundation

func windowID(pid: pid_t) -> CGWindowID? {
    guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] else { return nil }
    var best: (CGWindowID, Double)?
    for info in list where (info[kCGWindowOwnerPID as String] as? pid_t) == pid {
        guard (info[kCGWindowLayer as String] as? Int) == 0,
              let id = info[kCGWindowNumber as String] as? CGWindowID,
              let bounds = info[kCGWindowBounds as String] as? [String: Double] else { continue }
        let area = (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
        if best == nil || area > best!.1 { best = (id, area) }
    }
    return best?.0
}

// US ANSI virtual key codes: a chord matches on the physical key, not its text.
let named: [String: CGKeyCode] = [
    "enter": 36, "esc": 53, "bs": 51, "tab": 48, "space": 49,
    "up": 126, "down": 125, "left": 123, "right": 124,
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9,
    "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "o": 31, "u": 32,
    "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46,
    "1": 18, "2": 19, "3": 20, "4": 21, "5": 23, "6": 22, "7": 26, "8": 28, "9": 25, "0": 29,
    "[": 33, "]": 30, "/": 44, "-": 27, "\\": 42, "=": 24, ";": 41, "'": 39, ",": 43,
    ".": 47, "`": 50, " ": 49,
]

// US ANSI shifted characters, as (unshifted key, Shift).
let shifted: [Character: String] = [
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
    "(": "9", ")": "0", "{": "[", "}": "]", "?": "/", "_": "-", "|": "\\", "+": "=",
    ":": ";", "\"": "'", "<": ",", ">": ".", "~": "`",
]

func keyFor(_ ch: Character) -> (CGKeyCode, CGEventFlags)? {
    if let code = named[String(ch)] { return (code, []) }
    if ch.isUppercase, let code = named[ch.lowercased()] { return (code, .maskShift) }
    if let base = shifted[ch], let code = named[base] { return (code, .maskShift) }
    return nil
}

func post(pid: pid_t, code: CGKeyCode, flags: CGEventFlags = [], text: String? = nil) {
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else {
            fputs("could not create key event for code \(code)\n", stderr); exit(1)
        }
        event.flags = flags
        if let text {
            let units = Array(text.utf16)
            event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        }
        event.postToPid(pid)
        usleep(30_000)
    }
}

struct Keystroke {
    let code: CGKeyCode
    let flags: CGEventFlags
    let text: String?
}

func parse(spec: String) -> [Keystroke] {
    if spec.hasPrefix("{"), spec.hasSuffix("}") {
        var flags: CGEventFlags = []
        var key = String(spec.dropFirst().dropLast())
        let modifiers: [(String, CGEventFlags)] = [("cmd+", .maskCommand), ("shift+", .maskShift),
                                                   ("ctrl+", .maskControl), ("alt+", .maskAlternate)]
        while let (prefix, flag) = modifiers.first(where: { key.hasPrefix($0.0) }) {
            flags.insert(flag)
            key.removeFirst(prefix.count)
        }
        guard let code = named[key] else { fputs("unknown key \(key)\n", stderr); exit(2) }
        return [Keystroke(code: code, flags: flags, text: key.count == 1 ? key : nil)]
    }
    return spec.map { ch in
        guard let (code, flags) = keyFor(ch) else { fputs("no key for \(ch)\n", stderr); exit(2) }
        return Keystroke(code: code, flags: flags, text: String(ch))
    }
}

let args = CommandLine.arguments
guard args.count >= 3, let pid = pid_t(args[2]) else {
    fputs("usage: uidrive winid <pid> | uidrive key <pid> <spec>...\n", stderr); exit(2)
}
switch args[1] {
case "winid":
    guard let id = windowID(pid: pid) else { fputs("no window for pid \(pid)\n", stderr); exit(1) }
    print(id)
case "key":
    guard args.count >= 4 else { fputs("usage: uidrive key <pid> <spec>...\n", stderr); exit(2) }
    // Parse every spec before posting any, so a bad one sends nothing.
    let specs = args.dropFirst(3).map(parse)
    for strokes in specs {
        for s in strokes { post(pid: pid, code: s.code, flags: s.flags, text: s.text) }
        usleep(150_000)
    }
default:
    fputs("unknown command \(args[1])\n", stderr); exit(2)
}
