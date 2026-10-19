// keypost: posts real key events through the HID event tap, as a keyboard would.
//   keypost fn down|up      Fn (🌐) pressed or released
//   keypost key <keycode>   press and release a key (53 = Escape, 51 = Delete)
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count == 3 else { fputs("usage: keypost fn down|up | keypost key <code>\n", stderr); exit(2) }

func post(_ code: CGKeyCode, down: Bool, type: CGEventType? = nil, flags: CGEventFlags = []) {
    guard let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else { exit(1) }
    if let type { e.type = type }
    e.flags = flags
    e.post(tap: .cghidEventTap)
}

if args[1] == "fn" {
    let down = args[2] == "down"
    post(63, down: down, type: .flagsChanged, flags: down ? .maskSecondaryFn : [])
} else if args[1] == "key", let code = CGKeyCode(args[2]) {
    post(code, down: true)
    usleep(30_000)
    post(code, down: false)
}
usleep(20_000)
