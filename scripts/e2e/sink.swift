// sink: a focused text window that records whatever is typed or pasted into it.
//   sink <output-file> <seconds>   writes the window's text to <output-file>, then quits.
import Cocoa

let args = CommandLine.arguments
let output = args[1]
let seconds = Double(args[2]) ?? 10

/// Ignores function keys and Escape (the test posts those as hotkeys), so only real text lands.
final class SinkTextView: NSTextView {
    override func keyDown(with event: NSEvent) {
        let scalar = event.charactersIgnoringModifiers?.unicodeScalars.first?.value ?? 0
        if scalar == 0x1B || (0xF700...0xF8FF).contains(scalar) { return }
        super.keyDown(with: event)
    }
}

/// Real apps paste via the Edit menu's Cmd+V key equivalent, so the sink needs one too.
func installEditMenu() {
    let main = NSMenu()
    let editItem = NSMenuItem()
    main.addItem(editItem)
    let edit = NSMenu(title: "Edit")
    edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
    editItem.submenu = edit
    NSApp.mainMenu = main
}

final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var text: NSTextView!

    func applicationDidFinishLaunching(_ note: Notification) {
        window = NSWindow(contentRect: NSRect(x: 300, y: 300, width: 520, height: 180),
                          styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "Wispr E2E sink"
        installEditMenu()
        text = SinkTextView(frame: window.contentView!.bounds)
        text.isRichText = false
        window.contentView!.addSubview(text)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(text)
        NSApp.activate(ignoringOtherApps: true)
        Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { _ in
            try? self.text.string.write(toFile: output, atomically: true, encoding: .utf8)
            NSApp.terminate(nil)
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
