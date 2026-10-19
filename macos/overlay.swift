// wispr-overlay: the on-screen status pill.
//
// Driven by line commands on stdin from the wispr daemon:
//   listening | level <rms> | processing | notice <text> | alert <text> | hide
// Exits when stdin closes (i.e. when wispr exits).
//
// It must never take focus, or the paste would land in the wrong place: the app runs with the
// accessory activation policy, the window is a non-activating NSPanel that refuses key/main
// status, ignores the mouse, and is shown with orderFrontRegardless().
//
// Look: a small frosted capsule above the Dock. While listening it shows a recording dot, a
// waveform that follows the microphone level, and the elapsed time; while transcribing the
// bars ripple; notices size to their text and carry an icon (amber for problems).

import Cocoa

enum Style {
    static let height: CGFloat = 34
    static let padding: CGFloat = 14
    static let gap: CGFloat = 9
    static let bottomInset: CGFloat = 24
    static let maxWidth: CGFloat = 460

    static let barCount = 11
    static let barWidth: CGFloat = 3
    static let barGap: CGFloat = 2.5
    static let barMin: CGFloat = 3
    static let barMax: CGFloat = 18
    static var barsWidth: CGFloat { CGFloat(barCount) * barWidth + CGFloat(barCount - 1) * barGap }

    static let dot: CGFloat = 8
    static let icon: CGFloat = 15

    static let text = NSColor(calibratedWhite: 1, alpha: 0.94)
    static let secondary = NSColor(calibratedWhite: 1, alpha: 0.55)
    static let recording = NSColor(srgbRed: 1, green: 0.29, blue: 0.25, alpha: 1)
    static let warning = NSColor(srgbRed: 1, green: 0.74, blue: 0.27, alpha: 1)

    static let font = NSFont.systemFont(ofSize: 13, weight: .medium)
    static let timeFont = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .medium)
}

enum Mode: Equatable {
    case hidden
    case listening
    case processing
    case message(String, alert: Bool)
}

final class Panel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// A capsule image for NSVisualEffectView.maskImage, so the blur itself has round ends.
func capsuleMask(height: CGFloat) -> NSImage {
    let r = height / 2
    let image = NSImage(size: NSSize(width: height + 1, height: height), flipped: false) { rect in
        NSColor.black.setFill()
        NSBezierPath(roundedRect: rect, xRadius: r, yRadius: r).fill()
        return true
    }
    image.capInsets = NSEdgeInsets(top: r, left: r, bottom: r, right: r)
    image.resizingMode = .stretch
    return image
}

/// A single line of text that draws exactly what it measures, truncating with an ellipsis
/// only when given less room than `naturalWidth`.
final class Line: NSView {
    var string = "" { didSet { needsDisplay = true } }
    let font: NSFont
    let color: NSColor

    init(_ font: NSFont, _ color: NSColor) {
        self.font = font
        self.color = color
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { nil }

    var attributes: [NSAttributedString.Key: Any] {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return [.font: font, .foregroundColor: color, .paragraphStyle: style]
    }

    func naturalWidth(_ text: String) -> CGFloat {
        ceil((text as NSString).size(withAttributes: attributes).width)
    }

    /// Height of one line, so callers can centre it vertically.
    var lineHeight: CGFloat { ceil(font.ascender - font.descender) }

    override func draw(_ dirtyRect: NSRect) {
        (string as NSString).draw(
            with: bounds,
            options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine],
            attributes: attributes
        )
    }
}

final class Pill: NSView {
    let glass = NSVisualEffectView()
    let content = NSView()
    let dot = CALayer()
    let bars: [CALayer] = (0..<Style.barCount).map { _ in CALayer() }
    let time = Line(Style.timeFont, Style.secondary)
    let icon = NSImageView()
    let text = Line(Style.font, Style.text)

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = Style.height / 2
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true

        glass.material = .hudWindow
        glass.blendingMode = .behindWindow
        glass.state = .active
        glass.appearance = NSAppearance(named: .vibrantDark)
        glass.maskImage = capsuleMask(height: Style.height)
        glass.frame = bounds
        glass.autoresizingMask = [.width, .height]
        addSubview(glass)

        // A dark tint keeps the pill legible over white pages; the hairline gives it an edge
        // over dark ones.
        let tint = NSView(frame: bounds)
        tint.wantsLayer = true
        tint.layer?.backgroundColor = NSColor(calibratedWhite: 0, alpha: 0.32).cgColor
        tint.layer?.borderColor = NSColor(calibratedWhite: 1, alpha: 0.13).cgColor
        tint.layer?.borderWidth = 1
        tint.layer?.cornerRadius = Style.height / 2
        tint.layer?.cornerCurve = .continuous
        tint.autoresizingMask = [.width, .height]
        addSubview(tint)

        // Content keeps its own width and stays centred while the pill animates around it.
        content.wantsLayer = true
        content.autoresizingMask = [.minXMargin, .maxXMargin]
        addSubview(content)

        dot.backgroundColor = Style.recording.cgColor
        dot.cornerRadius = Style.dot / 2
        dot.shadowColor = Style.recording.cgColor
        dot.shadowOpacity = 0.9
        dot.shadowRadius = 4
        dot.shadowOffset = .zero
        content.layer?.addSublayer(dot)

        for bar in bars {
            bar.backgroundColor = Style.text.cgColor
            bar.cornerRadius = Style.barWidth / 2
            content.layer?.addSublayer(bar)
        }

        icon.imageScaling = .scaleProportionallyDown
        content.addSubview(time)
        content.addSubview(icon)
        content.addSubview(text)
    }

    required init?(coder: NSCoder) { nil }

    /// Lays the content out for `mode` and returns the pill width it needs.
    func configure(_ mode: Mode) -> CGFloat {
        let h = Style.height
        var x = Style.padding
        let showsBars = mode == .listening || mode == .processing
        dot.isHidden = mode != .listening
        time.isHidden = mode != .listening
        bars.forEach { $0.isHidden = !showsBars }
        icon.isHidden = true
        text.isHidden = true

        switch mode {
        case .listening:
            dot.frame = CGRect(x: x, y: (h - Style.dot) / 2, width: Style.dot, height: Style.dot)
            breathe(dot)
            x += Style.dot + Style.gap
            layoutBars(at: x)
            x += Style.barsWidth + Style.gap
            // Sized for "00:00" so the label never truncates; the pill only reserves "0:00".
            time.string = "0:00"
            let lh = time.lineHeight
            time.frame = NSRect(x: x, y: (h - lh) / 2, width: time.naturalWidth("00:00"), height: lh)
            x += time.naturalWidth("0:00")
        case .processing:
            layoutBars(at: x)
            x += Style.barsWidth
        case let .message(message, alert):
            let symbol = alert ? "exclamationmark.triangle.fill" : "info.circle.fill"
            let config = NSImage.SymbolConfiguration(pointSize: 13, weight: .semibold)
            icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
                .withSymbolConfiguration(config)
            icon.contentTintColor = alert ? Style.warning : Style.secondary
            icon.frame = NSRect(x: x, y: (h - Style.icon) / 2, width: Style.icon, height: Style.icon)
            icon.isHidden = false
            x += Style.icon + 8
            text.string = message
            let available = Style.maxWidth - x - Style.padding
            let w = min(text.naturalWidth(message), available)
            let lh = text.lineHeight
            text.frame = NSRect(x: x, y: (h - lh) / 2, width: w, height: lh)
            text.isHidden = false
            x += w
        case .hidden:
            break
        }
        let width = x + Style.padding
        content.frame = NSRect(x: (bounds.width - width) / 2, y: 0, width: width, height: h)
        return width
    }

    func layoutBars(at x: CGFloat) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (i, bar) in bars.enumerated() {
            let bx = x + CGFloat(i) * (Style.barWidth + Style.barGap)
            bar.frame = CGRect(x: bx, y: (Style.height - Style.barMin) / 2, width: Style.barWidth, height: Style.barMin)
        }
        CATransaction.commit()
    }

    /// Sets bar heights (0...1 each), keeping every bar vertically centred.
    func setBars(_ heights: [CGFloat], opacity: [Float]) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (i, bar) in bars.enumerated() {
            let barHeight = Style.barMin + (Style.barMax - Style.barMin) * max(0, min(1, heights[i]))
            var frame = bar.frame
            frame.size.height = barHeight
            frame.origin.y = (Style.height - barHeight) / 2
            bar.frame = frame
            bar.opacity = opacity[i]
        }
        CATransaction.commit()
    }

    func breathe(_ layer: CALayer) {
        layer.removeAllAnimations()
        let a = CABasicAnimation(keyPath: "opacity")
        a.fromValue = 1.0
        a.toValue = 0.4
        a.duration = 0.9
        a.autoreverses = true
        a.repeatCount = .infinity
        a.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
        layer.add(a, forKey: "breathe")
    }
}

final class Controller {
    let panel: Panel
    let pill: Pill
    var mode = Mode.hidden
    var hideTimer: Timer?
    var frameTimer: Timer?
    var generation = 0
    var screen = NSRect.zero

    var started = Date()
    var targetLevel: CGFloat = 0
    var level: CGFloat = 0
    let phases: [CGFloat] = (0..<Style.barCount).map { _ in CGFloat.random(in: 0...(2 * .pi)) }
    let speeds: [CGFloat] = (0..<Style.barCount).map { _ in CGFloat.random(in: 5...9) }

    var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

    init() {
        let rect = NSRect(x: 0, y: 0, width: 200, height: Style.height)
        panel = Panel(
            contentRect: rect,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .statusBar
        panel.ignoresMouseEvents = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        pill = Pill(frame: rect)
        panel.contentView = pill
    }

    func handle(_ line: String) {
        let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
        let arg = parts.count > 1 ? parts[1] : ""
        switch parts.first ?? "" {
        case "listening":
            started = Date()
            targetLevel = 0
            level = 0
            show(.listening, autoHide: nil)
        case "level":
            targetLevel = normalized(rms: CGFloat(Double(arg) ?? 0))
        case "processing":
            show(.processing, autoHide: nil)
        case "notice":
            show(.message(arg, alert: false), autoHide: 2.5)
        case "alert":
            show(.message(arg, alert: true), autoHide: 4)
        case "hide":
            hide()
        default:
            break
        }
    }

    /// Microphone RMS to 0...1 on a log scale: about -55 dBFS is silence, -15 dBFS is loud speech.
    func normalized(rms: CGFloat) -> CGFloat {
        guard rms > 0 else { return 0 }
        let db = 20 * log10(rms)
        return max(0, min(1, (db + 55) / 40))
    }

    func show(_ next: Mode, autoHide: TimeInterval?) {
        hideTimer?.invalidate()
        hideTimer = nil
        generation += 1
        let wasHidden = mode == .hidden
        mode = next

        if wasHidden {
            screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        }
        let width = pill.configure(next)
        let frame = NSRect(
            x: round(screen.midX - width / 2),
            y: screen.minY + Style.bottomInset,
            width: width,
            height: Style.height
        )

        if wasHidden {
            panel.setFrame(frame.offsetBy(dx: 0, dy: reduceMotion ? 0 : -8), display: false)
            pill.content.setFrameOrigin(.zero)
            panel.alphaValue = 0
            panel.orderFrontRegardless()
        }
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = wasHidden ? 0.22 : 0.18
            ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.2, 0.9, 0.3, 1)
            panel.animator().setFrame(frame, display: true)
            panel.animator().alphaValue = 1
        }, completionHandler: { [weak self] in
            self?.panel.invalidateShadow()
        })

        if next == .listening || next == .processing {
            startFrames()
        } else {
            stopFrames()
        }
        if let delay = autoHide {
            hideTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) { [weak self] _ in
                self?.hide()
            }
        }
    }

    func hide() {
        hideTimer?.invalidate()
        hideTimer = nil
        guard mode != .hidden else { return }
        mode = .hidden
        generation += 1
        let current = generation
        let frame = panel.frame.offsetBy(dx: 0, dy: reduceMotion ? 0 : -4)
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.16
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().setFrame(frame, display: true)
            panel.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            // A new state may have arrived while fading out; only then leave the pill up.
            guard let self, self.generation == current else { return }
            self.panel.orderOut(nil)
            self.stopFrames()
        })
    }

    func startFrames() {
        guard frameTimer == nil else { return }
        let timer = Timer(timeInterval: 1.0 / 60, repeats: true) { [weak self] _ in self?.frame() }
        RunLoop.main.add(timer, forMode: .common)
        frameTimer = timer
    }

    func stopFrames() {
        frameTimer?.invalidate()
        frameTimer = nil
    }

    func frame() {
        let t = CGFloat(Date().timeIntervalSince(started))
        let n = Style.barCount
        let centre = CGFloat(n - 1) / 2
        var heights = [CGFloat](repeating: 0, count: n)
        var opacity = [Float](repeating: 1, count: n)

        switch mode {
        case .listening:
            // Fast attack, slow release, and a slow decay so a stalled stream settles to rest.
            let k: CGFloat = targetLevel > level ? 0.5 : 0.14
            level += (targetLevel - level) * k
            targetLevel *= 0.985
            for i in 0..<n {
                let distance = abs(CGFloat(i) - centre) / centre
                let envelope = 0.35 + 0.65 * (1 - distance * distance)
                let wobble = reduceMotion ? 1 : 0.72 + 0.28 * sin(t * speeds[i] + phases[i])
                heights[i] = level * envelope * wobble
            }
            let seconds = Int(t)
            let clock = String(format: "%d:%02d", seconds / 60, seconds % 60)
            if pill.time.string != clock { pill.time.string = clock }
        case .processing:
            // A soft wave travelling left to right while the text is on its way.
            let speed: CGFloat = reduceMotion ? 2.5 : 6
            for i in 0..<n {
                let wave = 0.5 + 0.5 * sin(t * speed - CGFloat(i) * 0.6)
                heights[i] = 0.08 + 0.32 * wave
                opacity[i] = Float(0.35 + 0.6 * wave)
            }
        default:
            return
        }
        pill.setBars(heights, opacity: opacity)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let controller = Controller()

DispatchQueue.global(qos: .userInitiated).async {
    while let line = readLine() {
        DispatchQueue.main.async { controller.handle(line) }
    }
    DispatchQueue.main.async { NSApp.terminate(nil) }
}

app.run()
