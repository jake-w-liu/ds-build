import ApplicationServices
import AppKit
import Foundation

struct Input: Decodable {
    let action: String?
    let prompt: String
    let filePaths: [String]
    let newChat: Bool?
    let timeoutMs: Double?
    let uploadTimeoutMs: Double?
    let responseStableMs: Double?
    let pollIntervalMs: Double?
    let responseStartTimeoutMs: Double?
    let returnAfterSend: Bool?
    let restoreFrontmostOnExit: Bool?
    let baselineCopyMessageCount: Int?
    let observedAssistantText: String?
}

struct NodeRecord {
    let element: AXUIElement
    let role: String
    let label: String
    let enabled: Bool?
    let position: CGPoint?
    let size: CGSize?
}

struct AssistantCaptureSnapshot {
    let promptVisible: Bool
    let assistantText: String
}

struct AssistantCaptureState {
    var assistantText = ""
    var promptVisibleEver = false
    var promptVisibleNow = false
    var incomplete = false
    var incompleteReason = ""
    var lastVisibleAssistantText = ""
}

enum AxUploadError: Error, CustomStringConvertible {
    case message(String)
    case coded(String, String)
    case codedDetails(String, String, [String: Any])

    var description: String {
        switch self {
        case .message(let value): return value
        case .coded(_, let message): return message
        case .codedDetails(_, let message, _): return message
        }
    }
}

let windowDescendantLimit = 4_000
let appDescendantLimit = 2_500

func emit(_ value: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: value, options: [.prettyPrinted, .sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

func fail(_ code: String, _ message: String, details: [String: Any]? = nil) -> Never {
    var payload: [String: Any] = [
        "ok": false,
        "code": code,
        "message": message,
    ]
    if let details {
        payload["details"] = details
    }
    if let data = try? JSONSerialization.data(withJSONObject: payload, options: [.prettyPrinted, .sortedKeys]) {
        FileHandle.standardError.write(data)
        FileHandle.standardError.write(Data("\n".utf8))
    } else {
        FileHandle.standardError.write(Data("\(code): \(message)\n".utf8))
    }
    exit(1)
}

func sleepMs(_ milliseconds: Double) {
    Thread.sleep(forTimeInterval: max(0, milliseconds) / 1000.0)
}

func normalizeTimeoutMs(_ timeoutMs: Double?, fallback: Double) -> Double {
    guard let timeoutMs else { return fallback }
    guard timeoutMs.isFinite else { return fallback }
    return timeoutMs <= 0 ? 0 : max(1, timeoutMs.rounded(.down))
}

func attr<T>(_ element: AXUIElement, _ name: String) -> T? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success, let value else {
        return nil
    }
    return value as? T
}

func stringAttr(_ element: AXUIElement, _ name: String) -> String {
    if let value: String = attr(element, name) { return value }
    if let value: NSAttributedString = attr(element, name) { return value.string }
    return ""
}

func boolAttr(_ element: AXUIElement, _ name: String) -> Bool? {
    attr(element, name)
}

func pointAttr(_ element: AXUIElement, _ name: String) -> CGPoint? {
    guard let value: AXValue = attr(element, name) else { return nil }
    var point = CGPoint.zero
    return AXValueGetValue(value, .cgPoint, &point) ? point : nil
}

func sizeAttr(_ element: AXUIElement, _ name: String) -> CGSize? {
    guard let value: AXValue = attr(element, name) else { return nil }
    var size = CGSize.zero
    return AXValueGetValue(value, .cgSize, &size) ? size : nil
}

func children(_ element: AXUIElement) -> [AXUIElement] {
    attr(element, kAXChildrenAttribute) ?? []
}

func descendants(_ root: AXUIElement, limit: Int = 12_000) -> [AXUIElement] {
    var output: [AXUIElement] = []
    var stack = [root]
    var seen: Set<UInt> = [UInt(CFHash(root))]
    while let current = stack.popLast(), output.count < limit {
        let items = children(current)
        for item in items {
            let key = UInt(CFHash(item))
            if seen.insert(key).inserted {
                output.append(item)
                stack.append(item)
            }
        }
    }
    return output
}

func normalize(_ value: String) -> String {
    value
        .replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

func label(_ element: AXUIElement) -> String {
    normalize([
        stringAttr(element, kAXDescriptionAttribute),
        stringAttr(element, kAXTitleAttribute),
        stringAttr(element, kAXValueAttribute),
    ].filter { !$0.isEmpty }.joined(separator: " "))
}

func transcriptText(_ element: AXUIElement) -> String {
    let values = [
        stringAttr(element, kAXDescriptionAttribute),
        stringAttr(element, kAXValueAttribute),
        stringAttr(element, kAXTitleAttribute),
    ].filter { !$0.isEmpty }
    guard let first = values.first else { return "" }
    let normalized = normalizeAssistantText(first)
    return normalized.lowercased() == "text" ? "" : normalized
}

func record(_ element: AXUIElement) -> NodeRecord {
    NodeRecord(
        element: element,
        role: stringAttr(element, kAXRoleAttribute),
        label: label(element),
        enabled: boolAttr(element, kAXEnabledAttribute),
        position: pointAttr(element, kAXPositionAttribute),
        size: sizeAttr(element, kAXSizeAttribute)
    )
}

func press(_ record: NodeRecord, _ name: String) throws {
    let error = AXUIElementPerformAction(record.element, kAXPressAction as CFString)
    guard error == .success else {
        throw AxUploadError.message("Could not press \(name): AX error \(error.rawValue), label=\(record.label)")
    }
}

func click(_ record: NodeRecord, _ name: String) throws {
    guard let position = record.position, let size = record.size else {
        throw AxUploadError.message("Could not click \(name): geometry unavailable, label=\(record.label)")
    }
    let center = CGPoint(x: position.x + (size.width / 2), y: position.y + (size.height / 2))
    let source = CGEventSource(stateID: .hidSystemState)
    let move = CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: center, mouseButton: .left)
    let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: center, mouseButton: .left)
    let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: center, mouseButton: .left)
    move?.post(tap: .cghidEventTap)
    down?.post(tap: .cghidEventTap)
    up?.post(tap: .cghidEventTap)
    sleepMs(120)
}

func setText(_ record: NodeRecord, _ text: String) throws {
    let error = AXUIElementSetAttributeValue(record.element, kAXValueAttribute as CFString, text as CFTypeRef)
    guard error == .success else {
        throw AxUploadError.message("Could not set composer text: AX error \(error.rawValue)")
    }
}

func key(_ code: CGKeyCode, flags: CGEventFlags = []) {
    let source = CGEventSource(stateID: .hidSystemState)
    let down = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true)!
    down.flags = flags
    down.post(tap: .cghidEventTap)
    let up = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false)!
    up.flags = flags
    up.post(tap: .cghidEventTap)
    sleepMs(80)
}

func restoreFrontmostApplication(_ app: NSRunningApplication?) {
    guard let app else { return }
    guard app.bundleIdentifier != "com.openai.chat" && app.bundleIdentifier != "com.openai.codex" else { return }
    _ = app.activate()
    sleepMs(200)
}

struct PasteboardSnapshot {
    let items: [[NSPasteboard.PasteboardType: Data]]

    init(_ pasteboard: NSPasteboard) {
        items = (pasteboard.pasteboardItems ?? []).compactMap { item in
            var values: [NSPasteboard.PasteboardType: Data] = [:]
            for type in item.types {
                if let data = item.data(forType: type) {
                    values[type] = data
                }
            }
            return values.isEmpty ? nil : values
        }
    }

    func restore(_ pasteboard: NSPasteboard) {
        pasteboard.clearContents()
        let restoredItems = items.map { values in
            let item = NSPasteboardItem()
            for (type, data) in values {
                item.setData(data, forType: type)
            }
            return item
        }
        if !restoredItems.isEmpty {
            pasteboard.writeObjects(restoredItems)
        }
    }
}

func restorePasteboardIfOwned(
    _ snapshot: PasteboardSnapshot,
    pasteboard: NSPasteboard,
    expectedChangeCount: Int
) {
    if pasteboard.changeCount == expectedChangeCount {
        snapshot.restore(pasteboard)
    }
}

func waitFor<T>(_ timeoutMs: Double, intervalMs: Double = 250, _ block: () throws -> T?) throws -> T {
    let normalizedTimeoutMs = timeoutMs.isFinite ? timeoutMs : 0
    let deadline = normalizedTimeoutMs > 0
        ? Date().addingTimeInterval(normalizedTimeoutMs / 1000.0)
        : nil
    var lastError: Error?
    while deadline == nil || Date() < deadline! {
        do {
            if let result = try block() { return result }
        } catch {
            lastError = error
        }
        sleepMs(intervalMs)
    }
    if let lastError { throw lastError }
    throw AxUploadError.message("Timed out after \(Int(normalizedTimeoutMs)) ms")
}

func chatGPTRunningApp() -> NSRunningApplication? {
    if let app = NSRunningApplication.runningApplications(withBundleIdentifier: "com.openai.codex").first {
        return app
    }
    if let app = NSRunningApplication.runningApplications(withBundleIdentifier: "com.openai.chat").first {
        return app
    }
    return NSWorkspace.shared.runningApplications.first(where: { $0.localizedName == "ChatGPT" })
}

func composerValue(_ composerRecord: NodeRecord) -> String {
    var content = stringAttr(composerRecord.element, kAXValueAttribute)
        .replacingOccurrences(of: "\r\n", with: "\n")
        .replacingOccurrences(of: "\r", with: "\n")
    if content == "Message ChatGPT" || content == "\nMessage ChatGPT" { return "" }
    if content.hasSuffix("\nMessage ChatGPT") {
        content.removeLast("\nMessage ChatGPT".count)
    }
    return content
}

func canonicalPrompt(_ value: String) -> String {
    value
        .replacingOccurrences(of: "\r\n", with: "\n")
        .replacingOccurrences(of: "\r", with: "\n")
}

func chatGPTAppElement() throws -> AXUIElement {
    guard let app = chatGPTRunningApp() else {
        throw AxUploadError.message("ChatGPT is not running")
    }
    _ = app.activate(options: [.activateAllWindows])
    sleepMs(700)
    return AXUIElementCreateApplication(app.processIdentifier)
}

func chatWindow(_ appElement: AXUIElement) throws -> AXUIElement {
    let windows: [AXUIElement] = attr(appElement, kAXWindowsAttribute) ?? []
    let realWindows = windows.filter {
        let role = stringAttr($0, kAXRoleAttribute)
        return role == kAXWindowRole || role == "AXWindow"
    }
    if let first = realWindows.first { return first }
    // Codex ChatGPT sometimes returns empty/odd AXWindows; use any descendant
    // window-like node that contains a text area composer.
    for element in descendants(appElement, limit: 400) {
        let role = stringAttr(element, kAXRoleAttribute)
        if role == kAXWindowRole || role == "AXWindow" {
            if composer(element) != nil {
                return element
            }
        }
    }
    // Last resort: app element itself if it has a composer descendant
    if composer(appElement) != nil {
        return appElement
    }
    if !windows.isEmpty {
        throw AxUploadError.message("ChatGPT exposed only the app shell and no usable chat window through macOS Accessibility")
    }
    throw AxUploadError.message("No ChatGPT window is available")
}

func firstRecord(_ root: AXUIElement, role: String? = nil, matching pattern: String? = nil) -> NodeRecord? {
    let regex = pattern.flatMap { try? NSRegularExpression(pattern: $0, options: [.caseInsensitive]) }
    for element in descendants(root, limit: windowDescendantLimit) {
        let item = record(element)
        if let role, item.role != role { continue }
        if let regex {
            let range = NSRange(item.label.startIndex..<item.label.endIndex, in: item.label)
            if regex.firstMatch(in: item.label, range: range) == nil { continue }
        }
        return item
    }
    return nil
}

func composer(_ window: AXUIElement) -> NodeRecord? {
    // Prefer Chat composer ("Message ChatGPT"); never pick Work ("Work with ChatGPT") first.
    let nodes = descendants(window, limit: windowDescendantLimit).map(record)
    if let chat = nodes.first(where: {
        $0.role == kAXTextAreaRole &&
            ($0.label.range(of: "Message ChatGPT", options: .caseInsensitive) != nil ||
             $0.label.range(of: "text entry area", options: .caseInsensitive) != nil)
    }) {
        return chat
    }
    // Any text area that is not Work composer
    if let nonWork = nodes.first(where: {
        $0.role == kAXTextAreaRole &&
            $0.label.range(of: "Work with", options: .caseInsensitive) == nil
    }) {
        return nonWork
    }
    return firstRecord(window, role: kAXTextAreaRole)
}

/// Paste a file into the Chat composer via clipboard (works on Codex ChatGPT app
/// where the old "Upload file" menu item is not exposed to Accessibility).
func pasteFileViaClipboard(_ filePath: String, appElement: AXUIElement, uploadTimeoutMs: Double) throws {
    let fileURL = URL(fileURLWithPath: filePath)
    let fileName = fileURL.lastPathComponent
    let (window, composerRecord) = try waitForComposer(appElement, timeoutMs: uploadTimeoutMs)
    // Refuse Work composer
    if composerRecord.label.range(of: "Work with", options: .caseInsensitive) != nil {
        throw AxUploadError.coded(
            "PSST_GPT_WORK_MODE",
            "Composer is Work mode — refuse upload (use Chat / Message ChatGPT only)."
        )
    }

    let pasteboard = NSPasteboard.general
    let previousItems = PasteboardSnapshot(pasteboard)

    let lowerName = fileName.lowercased()
    let baselineMatches = descendants(window, limit: windowDescendantLimit)
        .map { record($0).label }
        .filter { labelMatchesUploadedFile($0, fileName: lowerName) }
        .count

    pasteboard.clearContents()
    guard pasteboard.writeObjects([fileURL as NSURL]) else {
        throw AxUploadError.message("Could not place \(fileName) on the pasteboard for Chat upload.")
    }
    let attachmentClipboardChangeCount = pasteboard.changeCount

    // Focus composer and Cmd+V
    _ = AXUIElementSetAttributeValue(composerRecord.element, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    sleepMs(200)
    key(9, flags: [.maskCommand]) // V
    sleepMs(350)
    restorePasteboardIfOwned(
        previousItems,
        pasteboard: pasteboard,
        expectedChangeCount: attachmentClipboardChangeCount
    )

    // Confirm a new exact current-file attachment. Zero timeout is unbounded.
    let deadline = uploadTimeoutMs > 0
        ? Date().addingTimeInterval(uploadTimeoutMs / 1000.0)
        : nil
    while deadline == nil || Date() < deadline! {
        let matches = descendants(window, limit: windowDescendantLimit)
            .map { record($0).label }
            .filter { labelMatchesUploadedFile($0, fileName: lowerName) }
            .count
        if matches > baselineMatches {
            return
        }
        // Re-resolve window (tree can refresh)
        if let w = try? chatWindow(appElement) {
            let matches2 = descendants(w, limit: windowDescendantLimit)
                .map { record($0).label }
                .filter { labelMatchesUploadedFile($0, fileName: lowerName) }
                .count
            if matches2 > baselineMatches {
                return
            }
        }
        sleepMs(300)
    }
    throw AxUploadError.coded(
        "PSST_GPT_ATTACHMENT_MISSING",
        "Clipboard paste did not attach \(fileName) in ChatGPT Chat composer."
    )
}

func snapshot(_ window: AXUIElement) -> [String: Any] {
    let nodes = descendants(window, limit: windowDescendantLimit)
    let records = nodes.map(record)
    let composerRecord = records.first { $0.role == kAXTextAreaRole }
    let composerTop = composerRecord?.position?.y ?? CGFloat.greatestFiniteMagnitude
    let staticTexts = zip(records, nodes)
        .filter { record, node in
            record.role == kAXStaticTextRole &&
                !transcriptText(node).isEmpty &&
                ((record.position?.y ?? 0) < composerTop - 8)
        }
        .sorted {
            let ly = $0.0.position?.y ?? 0
            let ry = $1.0.position?.y ?? 0
            if ly != ry { return ly < ry }
            return ($0.0.position?.x ?? 0) < ($1.0.position?.x ?? 0)
        }
        .map { _, node in transcriptText(node) }
    let buttonLabels = records
        .filter { $0.role == kAXButtonRole && !$0.label.isEmpty }
        .map(\.label)
    let isAnswering = buttonLabels.contains {
        $0.range(of: "\\b(stop|cancel)\\b", options: [.regularExpression, .caseInsensitive]) != nil
    }
    let sendReady = !isAnswering && records.contains { item in
        item.role == kAXButtonRole &&
            item.label.range(of: "\\b(send|submit)\\b", options: [.regularExpression, .caseInsensitive]) != nil
    }
    return [
        "title": stringAttr(window, kAXTitleAttribute).isEmpty ? "ChatGPT" : stringAttr(window, kAXTitleAttribute),
        "bundleId": (chatGPTRunningApp()?.bundleIdentifier ?? "com.openai.codex"),
        "processName": "ChatGPT",
        "frontmostProcessName": "ChatGPT",
        "background": false,
        "hasComposer": composerRecord != nil,
        "composerValue": composerRecord.map(composerValue) ?? "",
        "visibleModelLabel": buttonLabels.first { $0.range(of: "5\\.|4\\.|o3|Instant|Thinking|Pro", options: [.regularExpression, .caseInsensitive]) != nil } ?? "",
        "transcriptTexts": staticTexts,
        "visibleText": staticTexts.joined(separator: "\n"),
        "buttonLabels": buttonLabels,
        "copyMessageCount": copyMessageRecords(window).count,
        "isAnswering": isAnswering,
        "stopPresent": isAnswering,
        "sendReady": sendReady,
        "directAx": true,
    ]
}

func assistantCaptureSnapshot(from state: [String: Any], prompt: String) -> AssistantCaptureSnapshot {
    let transcript = state["transcriptTexts"] as? [String] ?? []
    let promptNeedle = normalize(prompt).lowercased()
    var promptIndex = -1
    for index in stride(from: transcript.count - 1, through: 0, by: -1) {
        let text = normalize(transcript[index]).lowercased()
        if text == promptNeedle ||
            (promptNeedle.count >= 80 && text.contains(String(promptNeedle.prefix(80)))) ||
            (text.count >= 80 && promptNeedle.contains(String(text.prefix(80)))) {
            promptIndex = index
            break
        }
    }
    let slice = promptIndex >= 0 ? transcript.dropFirst(promptIndex + 1) : transcript[...]
    let ignored = Set(["Ask anything", "Thinking", "Pro thinking", "Searching", "Searching the web"])
    let assistantText = slice
        .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
        .filter { !$0.isEmpty && !ignored.contains($0) && normalize($0).lowercased() != promptNeedle }
        .joined(separator: "\n")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    return AssistantCaptureSnapshot(
        promptVisible: promptIndex >= 0,
        assistantText: assistantText
    )
}

func assistantText(from state: [String: Any], prompt: String) -> String {
    assistantCaptureSnapshot(from: state, prompt: prompt).assistantText
}

func minimumAssistantTailOverlap(_ previous: String, _ current: String) -> Int {
    let shortest = min(previous.count, current.count)
    if shortest <= 24 { return shortest }
    if shortest <= 80 { return max(12, shortest / 2) }
    return max(24, Int(Double(shortest) * 0.2))
}

func mergeAssistantTails(_ previousText: String, _ currentVisibleText: String) -> String? {
    let previous = normalizeAssistantText(previousText)
    let current = normalizeAssistantText(currentVisibleText)
    if current.isEmpty { return previous }
    if previous.isEmpty { return nil }
    if current == previous || previous.hasSuffix(current) || previous.contains(current) {
        return previous
    }
    if current.contains(previous) {
        return current
    }
    let minOverlap = minimumAssistantTailOverlap(previous, current)
    let maxOverlap = min(previous.count, current.count)
    for overlapLength in stride(from: maxOverlap, through: minOverlap, by: -1) {
        let previousSuffix = String(previous.suffix(overlapLength))
        let currentPrefix = String(current.prefix(overlapLength))
        if previousSuffix == currentPrefix {
            return normalizeAssistantText(previous + current.dropFirst(overlapLength))
        }
    }
    return nil
}

func advanceAssistantCapture(_ state: AssistantCaptureState, snapshot: AssistantCaptureSnapshot) -> AssistantCaptureState {
    var next = state
    let visibleAssistantText = normalizeAssistantText(snapshot.assistantText)
    next.promptVisibleNow = snapshot.promptVisible
    if !visibleAssistantText.isEmpty {
        next.lastVisibleAssistantText = visibleAssistantText
    }
    if snapshot.promptVisible {
        next.promptVisibleEver = true
    }
    if visibleAssistantText.isEmpty {
        return next
    }
    if snapshot.promptVisible {
        if next.assistantText.isEmpty ||
            visibleAssistantText.count >= next.assistantText.count ||
            visibleAssistantText.contains(next.assistantText) ||
            !next.assistantText.contains(visibleAssistantText) {
            next.assistantText = visibleAssistantText
        }
        next.incomplete = false
        next.incompleteReason = ""
        return next
    }
    if next.assistantText.isEmpty {
        next.incomplete = true
        next.incompleteReason = "The active prompt scrolled out of the visible ChatGPT transcript before any assistant text was captured."
        return next
    }
    if let merged = mergeAssistantTails(next.assistantText, visibleAssistantText) {
        next.assistantText = merged
        next.incomplete = false
        next.incompleteReason = ""
        return next
    }
    next.incomplete = true
    next.incompleteReason = "The assistant response grew beyond the visible ChatGPT transcript and the newly visible tail could not be aligned with the previously captured text."
    return next
}

func normalizeAssistantText(_ text: String) -> String {
    text
        .replacingOccurrences(of: "\r", with: "")
        .split(separator: "\n", omittingEmptySubsequences: false)
        .map { line in
            String(line).replacingOccurrences(of: #"[ \t]+$"#, with: "", options: .regularExpression)
        }
        .joined(separator: "\n")
        .replacingOccurrences(of: #"\n{3,}"#, with: "\n\n", options: .regularExpression)
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

func responseAccepted(_ state: [String: Any], prompt: String, captureState: AssistantCaptureState) -> Bool {
    if captureState.promptVisibleEver { return true }
    let composerValue = normalize(String(describing: state["composerValue"] ?? ""))
    let normalizedPrompt = normalize(prompt)
    if composerValue.isEmpty { return true }
    return composerValue != normalizedPrompt
}

func chooseNewChat(_ appElement: AXUIElement) throws {
    var didStartNewChat = false
    if let window = try? chatWindow(appElement),
       let button = firstRecord(window, role: kAXButtonRole, matching: "^New chat$") {
        try press(button, "New chat")
        didStartNewChat = true
    }
    if !didStartNewChat {
        key(45, flags: [.maskCommand])
    }
    sleepMs(1_000)
}

func recoverFromTransientComposerState(_ appElement: AXUIElement, window: AXUIElement? = nil) {
    if let app = chatGPTRunningApp() {
        _ = app.activate(options: [.activateAllWindows])
        sleepMs(150)
    }
    if let window {
        if let button = firstRecord(window, role: kAXButtonRole, matching: "^New chat$") {
            try? press(button, "New chat recovery")
        }
    }
    key(45, flags: [.maskCommand])
    sleepMs(750)
}

func waitForComposer(_ appElement: AXUIElement, timeoutMs: Double) throws -> (AXUIElement, NodeRecord) {
    let normalizedTimeoutMs = timeoutMs.isFinite ? max(0, timeoutMs) : 0
    let deadline = normalizedTimeoutMs > 0
        ? Date().addingTimeInterval(normalizedTimeoutMs / 1000.0)
        : nil
    var lastError: Error?
    var sawWindowWithoutComposer = false
    var lastRecoveryAttemptAt = Date.distantPast
    func recoverIfDue(_ window: AXUIElement? = nil) {
        let elapsedMs = Date().timeIntervalSince(lastRecoveryAttemptAt) * 1000
        guard elapsedMs >= 1_500 else {
            return
        }
        lastRecoveryAttemptAt = Date()
        recoverFromTransientComposerState(appElement, window: window)
    }
    while deadline == nil || Date() < deadline! {
        do {
            let window = try chatWindow(appElement)
            guard let item = composer(window) else {
                sawWindowWithoutComposer = true
                lastError = AxUploadError.message("No ChatGPT composer is available in the visible ChatGPT window")
                recoverIfDue(window)
                sleepMs(250)
                continue
            }
            return (window, item)
        } catch let error as AxUploadError {
            lastError = error
            let message = error.description.lowercased()
            let canRecoverFromShellOnly = message.contains("only the app shell") ||
                message.contains("no chatgpt window is available")
            if canRecoverFromShellOnly {
                recoverIfDue()
                sleepMs(250)
                continue
            }
            if sawWindowWithoutComposer {
                throw AxUploadError.message("No ChatGPT composer is available in the visible ChatGPT window")
            }
            throw error
        }
    }
    if sawWindowWithoutComposer {
        throw AxUploadError.message("No ChatGPT composer is available in the visible ChatGPT window")
    }
    if let lastError {
        throw lastError
    }
    throw AxUploadError.message("No ChatGPT window is available")
}

func didSendStart(_ appElement: AXUIElement, prompt: String) throws -> Bool {
    let window = try chatWindow(appElement)
    let state = snapshot(window)
    if (state["isAnswering"] as? Bool) == true {
        return true
    }
    guard let currentComposer = composer(window) else {
        // Missing composer is AX ambiguity, never proof that the prompt landed.
        return false
    }
    let currentValue = composerValue(currentComposer)
    return currentValue.isEmpty || currentValue != canonicalPrompt(prompt)
}

func pressSendAndVerify(_ record: NodeRecord, appElement: AXUIElement, prompt: String, timeoutMs: Double) throws -> Bool {
    try press(record, "Send")
    sleepMs(250)
    do {
        _ = try waitFor(timeoutMs, intervalMs: 100) {
            try didSendStart(appElement, prompt: prompt) ? true : nil
        } as Bool
        return true
    } catch {
        return false
    }
}

func labelMatchesUploadedFile(_ label: String, fileName: String) -> Bool {
    let normalizedLabel = normalize(label).lowercased()
    let normalizedFileName = normalize(fileName).lowercased()
    let maximumAttachmentLabelLength = max(96, normalizedFileName.count + 48)
    guard normalizedLabel.count <= maximumAttachmentLabelLength else {
        return false
    }
    if normalizedLabel == normalizedFileName ||
        normalizedLabel.hasPrefix("\(normalizedFileName) ") ||
        normalizedLabel.hasPrefix("\(normalizedFileName),") {
        return true
    }

    let fileURL = URL(fileURLWithPath: normalizedFileName)
    let stem = fileURL.deletingPathExtension().lastPathComponent
    let ext = fileURL.pathExtension
    guard !stem.isEmpty else {
        return false
    }

    let escapedStem = NSRegularExpression.escapedPattern(for: stem)
    let pattern: String
    if ext.isEmpty {
        pattern = #"\b\#(escapedStem)(?:\s*\(\d+\))?\b"#
    } else {
        let escapedExt = NSRegularExpression.escapedPattern(for: ext)
        pattern = #"\b\#(escapedStem)(?:\s*\(\d+\))?\.\#(escapedExt)\b"#
    }
    return normalizedLabel.range(of: pattern, options: .regularExpression) != nil
}

func labelsContainUploadNeedle(_ labels: [String], fileName: String) -> Bool {
    labels.contains { labelMatchesUploadedFile($0, fileName: fileName) }
}

func extractCopiedAssistantText(_ copiedText: String, prompt: String) -> String {
    let normalizedPrompt = normalize(prompt).lowercased()
    let lines = copiedText
        .components(separatedBy: .newlines)
        .map { normalize($0) }
        .filter { !$0.isEmpty };
    if lines.isEmpty {
        return "";
    }
    var promptIndex = -1
    if !normalizedPrompt.isEmpty {
        for index in stride(from: lines.count - 1, through: 0, by: -1) {
            let line = lines[index].lowercased()
            if line == normalizedPrompt ||
                (normalizedPrompt.count >= 80 && line.contains(String(normalizedPrompt.prefix(80)))) ||
                (normalizedPrompt.count >= 80 && normalizedPrompt.contains(String(line.prefix(80)))) {
                promptIndex = index
                break
            }
        }
    }
    let assistantLines = promptIndex >= 0
        ? Array(lines.dropFirst(promptIndex + 1))
        : lines
    return normalizeAssistantText(assistantLines.joined(separator: "\n")).trimmingCharacters(in: .whitespacesAndNewlines)
}

func extractVisibleConversationTextByClipboard(_ appElement: AXUIElement) -> String {
    let pasteboard = NSPasteboard.general
    let snapshot = PasteboardSnapshot(pasteboard)
    let sentinel = "PSST_COPY_SENTINEL_\(UUID().uuidString)"
    pasteboard.clearContents()
    pasteboard.setString(sentinel, forType: .string)
    let sentinelChangeCount = pasteboard.changeCount
    key(0, flags: [.maskCommand])
    sleepMs(120)
    key(8, flags: [.maskCommand])
    let copiedChangeCount = pasteboard.changeCount
    let copiedText = pasteboard.string(forType: .string) ?? ""
    restorePasteboardIfOwned(snapshot, pasteboard: pasteboard, expectedChangeCount: copiedChangeCount)
    return copiedChangeCount == sentinelChangeCount ? "" : copiedText
}

func copyMessageRecords(_ window: AXUIElement) -> [NodeRecord] {
    descendants(window, limit: windowDescendantLimit).map(record).filter { item in
        guard item.role == kAXButtonRole else { return false }
        let lower = normalize(item.label).lowercased()
        return lower == "copy" || lower.contains("copy message")
    }
}

func copyIdentityWords(_ value: String) -> [String] {
    value.lowercased().split { character in
        character.unicodeScalars.allSatisfy { !CharacterSet.alphanumerics.contains($0) }
    }.map(String.init)
}

/// Prove that a clipboard body belongs to the assistant body already observed
/// for this turn. ChatGPT virtualizes message controls, so Copy-button counts
/// and AX element identities are not stable enough to establish turn identity.
func copyMatchesObservedAssistant(_ copied: String, observedAssistantText: String) -> Bool {
    let candidate = normalize(copied).lowercased()
    let observed = normalize(observedAssistantText).lowercased()
    guard !candidate.isEmpty, !observed.isEmpty else { return false }
    if candidate == observed {
        return true
    }

    let observedWords = copyIdentityWords(observed)
    let candidateWords = copyIdentityWords(candidate)
    // Short bodies such as "OK" are unsafe substring identities because an
    // older response can contain them. Equality above is sufficient for them.
    guard observedWords.count >= 8, candidateWords.count >= 8 else { return false }
    if candidate.contains(observed) || observed.contains(candidate) {
        return true
    }
    let candidateWordText = candidateWords.joined(separator: " ")
    let width = min(10, observedWords.count)
    let maxStart = observedWords.count - width
    let starts = Set([0, maxStart / 4, maxStart / 2, (maxStart * 3) / 4, maxStart])
    let matches = starts.reduce(into: 0) { count, start in
        let anchor = observedWords[start..<(start + width)].joined(separator: " ")
        if candidateWordText.contains(anchor) { count += 1 }
    }
    return matches >= min(2, starts.count)
}

func assistantCopyCandidate(
    _ copied: String,
    prompt: String,
    observedAssistantText: String,
    hasPostBaselineButtonEvidence: Bool,
    baselineWasEmpty: Bool
) -> String? {
    let candidate = copied.trimmingCharacters(in: .whitespacesAndNewlines)
    let canonicalCopied = canonicalPrompt(candidate)
        .trimmingCharacters(in: .whitespacesAndNewlines)
    let canonicalUserPrompt = canonicalPrompt(prompt)
        .trimmingCharacters(in: .whitespacesAndNewlines)
    if canonicalCopied.isEmpty || canonicalCopied == canonicalUserPrompt { return nil }
    if !observedAssistantText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        guard copyMatchesObservedAssistant(candidate, observedAssistantText: observedAssistantText) else {
            return nil
        }
    } else if !hasPostBaselineButtonEvidence && !baselineWasEmpty {
        // With no AX body, accepting an arbitrary old visible Copy control would
        // be indistinguishable from returning a prior conversation response.
        return nil
    }
    return candidate
}

func firstAssistantCopyCandidate(
    _ candidates: [String],
    prompt: String,
    observedAssistantText: String,
    hasPostBaselineButtonEvidence: Bool,
    baselineWasEmpty: Bool
) -> String {
    candidates
        .compactMap {
            assistantCopyCandidate(
                $0,
                prompt: prompt,
                observedAssistantText: observedAssistantText,
                hasPostBaselineButtonEvidence: hasPostBaselineButtonEvidence,
                baselineWasEmpty: baselineWasEmpty
            )
        }
        .first ?? ""
}

func waitForCopiedClipboardString(
    _ pasteboard: NSPasteboard,
    sentinel: String,
    sentinelChangeCount: Int,
    timeoutMs: Double
) -> (text: String, changeCount: Int) {
    let deadline = Date().addingTimeInterval(max(1, timeoutMs) / 1000.0)
    repeat {
        let changeCount = pasteboard.changeCount
        let raw = pasteboard.string(forType: .string) ?? ""
        if changeCount != sentinelChangeCount && raw != sentinel {
            return (raw, changeCount)
        }
        sleepMs(40)
    } while Date() < deadline
    return ("", pasteboard.changeCount)
}

func copyCurrentAssistantMessage(
    _ appElement: AXUIElement,
    afterBaselineCount: Int,
    prompt: String,
    observedAssistantText: String = ""
) -> String {
    _ = AXUIElementSetAttributeValue(appElement, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
    guard let initialWindow = try? chatWindow(appElement) else { return "" }
    _ = AXUIElementSetAttributeValue(initialWindow, kAXMainAttribute as CFString, kCFBooleanTrue)
    _ = AXUIElementSetAttributeValue(initialWindow, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    sleepMs(300)
    let initialButtons = copyMessageRecords(initialWindow)
    guard !initialButtons.isEmpty else { return "" }
    let maxButtonsToTry = min(8, initialButtons.count)
    let pasteboard = NSPasteboard.general
    // Re-resolve the AX window and controls for every attempt. A successful or
    // partially successful Electron Copy click changes the toolbar to "Copied",
    // invalidating the remaining AX element handles from the prior traversal.
    let methods: [(physical: Bool, waitMs: Double)] = [(true, 1_200), (false, 2_500)]
    for method in methods {
        for offsetFromEnd in 0..<maxButtonsToTry {
            guard let window = try? chatWindow(appElement) else { continue }
            _ = AXUIElementSetAttributeValue(window, kAXMainAttribute as CFString, kCFBooleanTrue)
            _ = AXUIElementSetAttributeValue(window, kAXFocusedAttribute as CFString, kCFBooleanTrue)
            let buttons = copyMessageRecords(window)
            guard offsetFromEnd < buttons.count else {
                sleepMs(250)
                continue
            }
            let index = buttons.count - 1 - offsetFromEnd
            let button = buttons[index]
            let hasPostBaselineEvidence = buttons.count > afterBaselineCount && index >= afterBaselineCount
            let pasteboardSnapshot = PasteboardSnapshot(pasteboard)
            let sentinel = "PSST_COPY_SENTINEL_\(UUID().uuidString)"
            pasteboard.clearContents()
            pasteboard.setString(sentinel, forType: .string)
            let sentinelChangeCount = pasteboard.changeCount
            let acted: Bool
            if method.physical && button.position != nil && button.size != nil {
                acted = (try? click(button, "Copy message")) != nil
            } else {
                acted = (try? press(button, "Copy message")) != nil
            }
            let copiedResult = acted
                ? waitForCopiedClipboardString(
                    pasteboard,
                    sentinel: sentinel,
                    sentinelChangeCount: sentinelChangeCount,
                    timeoutMs: method.waitMs
                )
                : (text: "", changeCount: pasteboard.changeCount)
            let copied = copiedResult.text.trimmingCharacters(in: .whitespacesAndNewlines)
            restorePasteboardIfOwned(
                pasteboardSnapshot,
                pasteboard: pasteboard,
                expectedChangeCount: copiedResult.changeCount
            )
            guard let candidate = assistantCopyCandidate(
                copied,
                prompt: prompt,
                observedAssistantText: observedAssistantText,
                hasPostBaselineButtonEvidence: hasPostBaselineEvidence,
                baselineWasEmpty: afterBaselineCount == 0
            ) else {
                sleepMs(250)
                continue
            }
            // Buttons are traversed newest-first. The first proven candidate is
            // the current reply; scanning older buttons can select a longer old
            // response that happens to contain the current rendered body.
            return candidate
        }
    }
    return ""
}

func uploadDialogAcceptButton(_ appElement: AXUIElement) -> NodeRecord? {
    descendants(appElement, limit: appDescendantLimit).map(record).first { item in
        guard item.role == kAXButtonRole, item.enabled ?? true else {
            return false
        }
        return normalize(item.label).range(of: "^(Open|Choose|Select)$",
                                           options: [.regularExpression, .caseInsensitive]) != nil
    }
}

func uploadDialogCancelButton(_ appElement: AXUIElement) -> NodeRecord? {
    descendants(appElement, limit: appDescendantLimit).map(record).first { item in
        guard item.role == kAXButtonRole, item.enabled ?? true else {
            return false
        }
        return normalize(item.label).range(of: "^(Cancel|Close)$",
                                           options: [.regularExpression, .caseInsensitive]) != nil
    }
}

func uploadDialogIsVisible(_ appElement: AXUIElement) -> Bool {
    uploadDialogAcceptButton(appElement) != nil || uploadDialogCancelButton(appElement) != nil
}

func uploadDialogFileRecord(_ appElement: AXUIElement, fileName: String, prefix: String) -> NodeRecord? {
    let records = descendants(appElement, limit: appDescendantLimit).map(record)
    let exact = records.first { item in
        guard item.position != nil, item.size != nil else {
            return false
        }
        guard item.role == "AXRow" || item.role == "AXCell" || item.role == "AXOutlineRow" || item.role == kAXStaticTextRole else {
            return false
        }
        return normalize(item.label).lowercased() == fileName
    }
    if let exact { return exact }
    return records.first { item in
        guard item.position != nil, item.size != nil else {
            return false
        }
        guard item.role == "AXRow" || item.role == "AXCell" || item.role == "AXOutlineRow" || item.role == kAXStaticTextRole else {
            return false
        }
        let lower = normalize(item.label).lowercased()
        return lower.contains(fileName) || lower == prefix
    }
}

func selectUploadDialogFile(_ record: NodeRecord) {
    if AXUIElementPerformAction(record.element, kAXPressAction as CFString) == .success {
        sleepMs(150)
        return
    }
    try? click(record, "Upload file row")
}

func driveUploadSearchFallback(_ fileName: String) {
    let pasteboard = NSPasteboard.general
    let pasteboardSnapshot = PasteboardSnapshot(pasteboard)
    pasteboard.clearContents()
    pasteboard.setString(fileName, forType: .string)
    let ownedChangeCount = pasteboard.changeCount
    key(9, flags: [.maskCommand])
    sleepMs(250)
    key(125)
    sleepMs(150)
    key(36)
    sleepMs(250)
    restorePasteboardIfOwned(
        pasteboardSnapshot,
        pasteboard: pasteboard,
        expectedChangeCount: ownedChangeCount
    )
}

func windowHasSheet(_ window: AXUIElement) -> Bool {
    children(window).contains { stringAttr($0, kAXRoleAttribute) == "AXSheet" }
}

func appHasUploadSheet(_ appElement: AXUIElement) -> Bool {
    guard let window = try? chatWindow(appElement) else {
        return false
    }
    return windowHasSheet(window)
}

func dismissUploadSheetIfPresent(_ appElement: AXUIElement) {
    guard appHasUploadSheet(appElement) else {
        return
    }
    key(53)
    sleepMs(400)
}

@discardableResult
func pressUploadDialogAccept(_ appElement: AXUIElement) -> Bool {
    guard let accept = uploadDialogAcceptButton(appElement) else {
        return false
    }
    try? press(accept, "Open")
    sleepMs(350)
    return true
}

func chooseUploadDialogPath(_ filePath: String) {
    let pasteboard = NSPasteboard.general
    let pasteboardSnapshot = PasteboardSnapshot(pasteboard)
    pasteboard.clearContents()
    pasteboard.setString(filePath, forType: .string)
    let ownedChangeCount = pasteboard.changeCount
    defer {
        restorePasteboardIfOwned(
            pasteboardSnapshot,
            pasteboard: pasteboard,
            expectedChangeCount: ownedChangeCount
        )
    }

    key(5, flags: [.maskCommand, .maskShift])
    sleepMs(350)
    key(9, flags: [.maskCommand])
    sleepMs(200)
    key(36)
    sleepMs(800)
    key(31, flags: [.maskCommand])
    sleepMs(450)
    key(36)
    sleepMs(450)
}

func composerAttachmentLabels(_ window: AXUIElement, composerRecord: NodeRecord) -> [String] {
    guard let composerPosition = composerRecord.position, let composerSize = composerRecord.size else {
        return []
    }
    let composerElementId = CFHash(composerRecord.element)
    let composerBottom = composerPosition.y + min(composerSize.height, 360)
    return descendants(window, limit: windowDescendantLimit).map(record).compactMap { item in
        if CFHash(item.element) == composerElementId ||
            item.role == kAXTextAreaRole ||
            item.role == kAXTextFieldRole {
            return nil
        }
        guard let position = item.position, let size = item.size else {
            return nil
        }
        let lower = normalize(item.label).lowercased()
        guard !lower.isEmpty else { return nil }
        let bottom = position.y + size.height
        let right = position.x + size.width
        let withinX = right >= composerPosition.x - 40 &&
            position.x <= composerPosition.x + composerSize.width + 80
        let withinY = bottom >= composerPosition.y - 180 &&
            position.y <= composerBottom + 90
        guard withinX && withinY else { return nil }
        if lower.range(of: "^(send|attach|search|chatgpt|new chat|share|move|sidebar)$",
                       options: [.regularExpression, .caseInsensitive]) != nil {
            return nil
        }
        return lower
    }
}

func uploadConfirmationLabels(_ window: AXUIElement, composerRecord: NodeRecord) -> [String] {
    let composerElementId = CFHash(composerRecord.element)
    var seen = Set<String>()
    var labels: [String] = []

    for value in composerAttachmentLabels(window, composerRecord: composerRecord) {
        if seen.insert(value).inserted {
            labels.append(value)
        }
    }

    for item in descendants(window, limit: windowDescendantLimit).map(record) {
        if CFHash(item.element) == composerElementId ||
            item.role == kAXTextAreaRole ||
            item.role == kAXTextFieldRole {
            continue
        }
        let lower = normalize(item.label).lowercased()
        guard !lower.isEmpty, lower.count <= 160 else {
            continue
        }
        if lower.range(of: "^(send|attach|search|chatgpt|new chat|share|move|sidebar|stop|cancel|close)$",
                       options: [.regularExpression, .caseInsensitive]) != nil {
            continue
        }
        if seen.insert(lower).inserted {
            labels.append(lower)
        }
    }

    return labels
}

struct UploadDiagnostics {
    var iterations = 0
    var dialogVisible = false
    var composerVisible = false
    var acceptButtonVisible = false
    var fileRecordVisible = false
    var triedSearchFallback = false
    var lastWindowTitle = ""
    var lastError = ""
    var attachmentLabels: [String] = []

    func details(fileName: String) -> [String: Any] {
        [
            "fileName": fileName,
            "iterations": iterations,
            "dialogVisible": dialogVisible,
            "composerVisible": composerVisible,
            "acceptButtonVisible": acceptButtonVisible,
            "fileRecordVisible": fileRecordVisible,
            "triedSearchFallback": triedSearchFallback,
            "lastWindowTitle": lastWindowTitle,
            "lastError": lastError,
            "attachmentLabels": Array(attachmentLabels.prefix(30)),
        ]
    }
}

func uploadFile(_ filePath: String, appElement: AXUIElement, uploadTimeoutMs: Double) throws {
    let fileURL = URL(fileURLWithPath: filePath)
    let fileName = fileURL.lastPathComponent.lowercased()
    let filePrefix = fileURL.deletingPathExtension().lastPathComponent.lowercased()
    var diagnostics = UploadDiagnostics()
    let uploadDeadline = uploadTimeoutMs > 0
        ? Date().addingTimeInterval(uploadTimeoutMs / 1000.0)
        : nil

    func uploadTimeoutDetails(_ phase: String) -> [String: Any] {
        var details = diagnostics.details(fileName: fileName)
        details["phase"] = phase
        details["uploadTimeoutMs"] = uploadTimeoutMs
        return details
    }

    func remainingUploadMs(_ phase: String) throws -> Double {
        guard let uploadDeadline else {
            return 0
        }
        let remainingMs = uploadDeadline.timeIntervalSinceNow * 1000
        guard remainingMs > 0 else {
            throw AxUploadError.codedDetails(
                "PSST_GPT_UPLOAD_TIMEOUT",
                "Timed out while \(phase) for \(fileName).",
                uploadTimeoutDetails(phase)
            )
        }
        return max(1, remainingMs)
    }

    func phaseFailure(_ phase: String, _ error: Error) -> AxUploadError {
        if let axError = error as? AxUploadError {
            switch axError {
            case .coded(_, _), .codedDetails(_, _, _):
                return axError
            case .message(let message):
                if message.localizedCaseInsensitiveContains("Timed out after") {
                    return AxUploadError.codedDetails(
                        "PSST_GPT_UPLOAD_TIMEOUT",
                        "Timed out while \(phase) for \(fileName).",
                        uploadTimeoutDetails(phase)
                    )
                }
                return AxUploadError.codedDetails(
                    "PSST_GPT_UPLOAD_FAILED",
                    "Upload failed while \(phase) for \(fileName): \(message)",
                    uploadTimeoutDetails(phase)
                )
            }
        }
        return AxUploadError.codedDetails(
            "PSST_GPT_UPLOAD_FAILED",
            "Upload failed while \(phase) for \(fileName): \(String(describing: error))",
            uploadTimeoutDetails(phase)
        )
    }

    func withUploadPhase<T>(_ phase: String, _ block: () throws -> T) throws -> T {
        do {
            return try block()
        } catch {
            throw phaseFailure(phase, error)
        }
    }

    func waitForUpload<T>(_ phase: String, intervalMs: Double = 250, _ block: () throws -> T?) throws -> T {
        try withUploadPhase(phase) {
            try waitFor(try remainingUploadMs(phase), intervalMs: intervalMs, block)
        }
    }

    let (window, composerRecord) = try withUploadPhase("waiting for composer before upload") {
        try waitForComposer(appElement, timeoutMs: try remainingUploadMs("waiting for composer before upload"))
    }
    if composerRecord.label.range(of: "Work with", options: .caseInsensitive) != nil {
        throw AxUploadError.coded(
            "PSST_GPT_WORK_MODE",
            "Composer is Work mode — refuse upload (use Chat only)."
        )
    }

    // Prefer menu attach path when available; fall back to clipboard paste for
    // the Codex ChatGPT app (no AX-exposed "Upload file" menu item).
    var usedClipboardFallback = false
    do {
        guard let composerPosition = composerRecord.position, let composerSize = composerRecord.size else {
            throw AxUploadError.codedDetails(
                "PSST_GPT_COMPOSER_GEOMETRY_MISSING",
                "Composer geometry is unavailable while uploading \(fileName).",
                uploadTimeoutDetails("finding attach button")
            )
        }
        let composerBottom = composerPosition.y + min(composerSize.height, 360)
        let buttons = descendants(window, limit: windowDescendantLimit).map(record)
            .filter { $0.role == kAXButtonRole && ($0.enabled ?? true) && $0.position != nil && $0.size != nil }
        let candidates = buttons.filter { item in
            if item.label.range(of: "Attach", options: [.caseInsensitive]) != nil { return true }
            if item.label.range(of: "Add files", options: [.caseInsensitive]) != nil { return true }
            let position = item.position!
            let size = item.size!
            let centerY = position.y + size.height / 2
            return position.x >= composerPosition.x - 12 &&
                position.x <= composerPosition.x + 90 &&
                centerY >= composerPosition.y - 20 &&
                centerY <= composerBottom + 80 &&
                size.width >= 10 &&
                size.width <= 55 &&
                size.height >= 10 &&
                size.height <= 55
        }.sorted { ($0.position!.x, $0.size!.width) < ($1.position!.x, $1.size!.width) }
        // Prefer explicit Add files / Attach label over geometry guess
        let attach = buttons.first(where: {
            $0.label.range(of: "Add files", options: .caseInsensitive) != nil ||
                $0.label.range(of: "Attach", options: .caseInsensitive) != nil
        }) ?? candidates.first
        guard let attach else {
            throw AxUploadError.codedDetails(
                "PSST_GPT_ATTACH_BUTTON_MISSING",
                "Could not find the ChatGPT Attach button while uploading \(fileName).",
                uploadTimeoutDetails("finding attach button")
            )
        }
        try withUploadPhase("pressing attach button") {
            try press(attach, "Attach")
        }
        sleepMs(400)
        // Short probe only — new ChatGPT often never exposes "Upload file" via AX.
        let uploadItem: NodeRecord = try waitFor(2_500, intervalMs: 100) {
            descendants(appElement, limit: appDescendantLimit).map(record).first {
                $0.role == kAXMenuItemRole && (
                    $0.label.range(of: "Upload file", options: [.caseInsensitive]) != nil ||
                    $0.label.range(of: "Upload from", options: [.caseInsensitive]) != nil ||
                    $0.label.range(of: #"\b(Files|Computer|Device)\b"#, options: [.regularExpression, .caseInsensitive]) != nil
                )
            }
        }
        try withUploadPhase("pressing Upload file menu item") {
            try press(uploadItem, "Upload file")
        }
        _ = try waitForUpload("waiting for upload dialog", intervalMs: 150) {
            uploadDialogIsVisible(appElement) ? true : nil
        } as Bool

        try withUploadPhase("choosing upload file path") {
            chooseUploadDialogPath(filePath)
        }
        sleepMs(900)
    } catch {
        // New ChatGPT app: attach menu is not AX-exposed → clipboard file paste.
        diagnostics.lastError = String(describing: error)
        usedClipboardFallback = true
        try withUploadPhase("clipboard file paste fallback") {
            try pasteFileViaClipboard(filePath, appElement: appElement, uploadTimeoutMs: uploadTimeoutMs)
        }
        sleepMs(500)
    }
    if usedClipboardFallback {
        // Attachment already confirmed by pasteFileViaClipboard.
        return
    }

    var didRetryPathPaste = false
    let composerRecoveryTimeoutMs = uploadTimeoutMs
    var composerMissingAfterDialogAt: Date?
    do {
        _ = try waitForUpload("waiting for attachment confirmation", intervalMs: 500) {
            diagnostics.iterations += 1
            let window = try chatWindow(appElement)
            if windowHasSheet(window) {
                diagnostics.dialogVisible = true
                diagnostics.composerVisible = false
                diagnostics.acceptButtonVisible = uploadDialogAcceptButton(appElement) != nil
                if let fileRecord = uploadDialogFileRecord(appElement, fileName: fileName, prefix: filePrefix) {
                    diagnostics.fileRecordVisible = true
                    selectUploadDialogFile(fileRecord)
                    _ = pressUploadDialogAccept(appElement)
                } else {
                    diagnostics.fileRecordVisible = false
                }
                composerMissingAfterDialogAt = nil
                if !didRetryPathPaste && diagnostics.iterations >= 4 {
                    chooseUploadDialogPath(filePath)
                    didRetryPathPaste = true
                    diagnostics.triedSearchFallback = true
                } else {
                    key(31, flags: [.maskCommand])
                    sleepMs(250)
                    key(36)
                    sleepMs(250)
                }
                return nil
            }

            diagnostics.dialogVisible = false
            diagnostics.lastWindowTitle = stringAttr(window, kAXTitleAttribute)
            diagnostics.lastError = ""
            if let currentComposer = composer(window) {
                diagnostics.composerVisible = true
                composerMissingAfterDialogAt = nil
                let attachmentLabels = uploadConfirmationLabels(window, composerRecord: currentComposer)
                diagnostics.attachmentLabels = attachmentLabels
                return labelsContainUploadNeedle(attachmentLabels, fileName: fileName) ? true : nil
            }
            diagnostics.composerVisible = false
            if composerMissingAfterDialogAt == nil {
                composerMissingAfterDialogAt = Date()
                return nil
            }
            let missingForMs = Date().timeIntervalSince(composerMissingAfterDialogAt!) * 1000
            if composerRecoveryTimeoutMs > 0 && missingForMs >= composerRecoveryTimeoutMs {
                throw AxUploadError.message("No ChatGPT composer is available in the visible ChatGPT window")
            }
            return nil
        } as Bool
    } catch let error as AxUploadError {
        dismissUploadSheetIfPresent(appElement)
        switch error {
        case .coded(_, _), .codedDetails(_, _, _):
            throw error
        case .message(let message) where message.localizedCaseInsensitiveContains("No ChatGPT composer is available"):
            throw error
        default:
            throw AxUploadError.codedDetails(
                "PSST_GPT_UPLOAD_NOT_CONFIRMED",
                "The ChatGPT app did not show \(fileName) as an attached file before the upload timeout.",
                diagnostics.details(fileName: fileName)
            )
        }
    } catch {
        dismissUploadSheetIfPresent(appElement)
        throw AxUploadError.codedDetails(
            "PSST_GPT_UPLOAD_NOT_CONFIRMED",
            "The ChatGPT app did not show \(fileName) as an attached file before the upload timeout.",
            diagnostics.details(fileName: fileName)
        )
    }
}

func sendIfNeeded(_ appElement: AXUIElement, prompt: String, timeoutMs: Double) throws {
    let deadline = timeoutMs > 0 ? Date().addingTimeInterval(timeoutMs / 1000.0) : nil
    var attempted = false
    while deadline == nil || Date() < deadline! {
        if attempted {
            if try didSendStart(appElement, prompt: prompt) {
                sleepMs(500)
                if try didSendStart(appElement, prompt: prompt) { return }
            }
        }
        let window = try chatWindow(appElement)
        let state = snapshot(window)
        if (state["isAnswering"] as? Bool) == true { return }
        guard let currentComposer = composer(window) else {
            sleepMs(250)
            continue
        }
        let currentValue = composerValue(currentComposer)
        if currentValue != canonicalPrompt(prompt) {
            if attempted && currentValue.isEmpty { return }
            throw AxUploadError.coded(
                "PSST_GPT_PROMPT_CHANGED_BEFORE_SEND",
                "Composer no longer exactly matches the requested prompt; refusing a potentially altered or duplicate send."
            )
        }

        let send = firstRecord(window, role: kAXButtonRole, matching: "^Send(?: message)?$")
        guard let send, send.enabled == true else {
            // Attachment may still be uploading. Unlimited timeout waits for the
            // enabled Send signal rather than failing after a fixed retry count.
            sleepMs(300)
            continue
        }
        try press(send, "Send")
        attempted = true
        sleepMs(600)
    }
    throw AxUploadError.coded(
        "PSST_GPT_SEND_TIMEOUT",
        "The user-supplied upload timeout expired before ChatGPT accepted the message."
    )
}

func runProbe(_ input: Input) throws -> [String: Any] {
    let frontmostBefore = input.restoreFrontmostOnExit == true
        ? NSWorkspace.shared.frontmostApplication
        : nil
    defer {
        if input.restoreFrontmostOnExit == true {
            restoreFrontmostApplication(frontmostBefore)
        }
    }

    let appElement = try chatGPTAppElement()
    let probeTimeoutMs = normalizeTimeoutMs(input.uploadTimeoutMs, fallback: 0)
    let (window, _) = try waitForComposer(appElement, timeoutMs: probeTimeoutMs)
    return [
        "ok": true,
        "status": "ready",
        "state": snapshot(window),
    ]
}

func runSnapshot(_ input: Input) throws -> [String: Any] {
    let frontmostBefore = input.restoreFrontmostOnExit == true
        ? NSWorkspace.shared.frontmostApplication
        : nil
    defer {
        if input.restoreFrontmostOnExit == true {
            restoreFrontmostApplication(frontmostBefore)
        }
    }

    let appElement = try chatGPTAppElement()
    let window = try chatWindow(appElement)
    return [
        "ok": true,
        "status": "ready",
        "state": snapshot(window),
    ]
}

func run(_ input: Input) throws -> [String: Any] {
    let trustPromptKey = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
    let trustOptions = [trustPromptKey: true] as CFDictionary
    guard AXIsProcessTrustedWithOptions(trustOptions) else {
        throw AxUploadError.message("macOS Accessibility automation is not enabled for /usr/bin/swift")
    }

    if input.action == "selfcheckCopyPolicy" {
        let longPrompt = "Reply exactly with OK and nothing else. " + String(repeating: "prompt ", count: 80)
        let observed = "The complete assistant body begins here and contains several distinct words for identity matching."
        let fullObserved = observed + " " + String(repeating: "verified response segment ", count: 20)
        let unrelated = "An older assistant response discusses a completely different topic and must never be selected."
        let cases: [[String: Any]] = [
            [
                "name": "reject-exact-user-prompt",
                "pass": assistantCopyCandidate(
                    longPrompt,
                    prompt: longPrompt,
                    observedAssistantText: "",
                    hasPostBaselineButtonEvidence: true,
                    baselineWasEmpty: false
                ) == nil,
            ],
            [
                "name": "reject-line-ending-normalized-user-prompt",
                "pass": assistantCopyCandidate(
                    "line one\r\nline two",
                    prompt: "line one\nline two",
                    observedAssistantText: "",
                    hasPostBaselineButtonEvidence: true,
                    baselineWasEmpty: false
                ) == nil,
            ],
            [
                "name": "accept-short-exact-reply-contained-in-prompt",
                "pass": assistantCopyCandidate(
                    "OK",
                    prompt: longPrompt,
                    observedAssistantText: "OK",
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == "OK",
            ],
            [
                "name": "long-prompt-cannot-beat-short-assistant-reply",
                "pass": firstAssistantCopyCandidate(
                    [longPrompt, "OK"],
                    prompt: longPrompt,
                    observedAssistantText: "OK",
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == "OK",
            ],
            [
                "name": "newest-proven-copy-wins-over-older-longer-superset",
                "pass": firstAssistantCopyCandidate(
                    [observed, "Older prefix \(fullObserved) older suffix"],
                    prompt: longPrompt,
                    observedAssistantText: observed,
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == observed,
            ],
            [
                "name": "virtualized-equal-count-accepts-current-body-match",
                "pass": assistantCopyCandidate(
                    fullObserved,
                    prompt: longPrompt,
                    observedAssistantText: observed,
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == fullObserved.trimmingCharacters(in: .whitespacesAndNewlines),
            ],
            [
                "name": "virtualized-equal-count-rejects-old-response",
                "pass": assistantCopyCandidate(
                    unrelated,
                    prompt: longPrompt,
                    observedAssistantText: observed,
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == nil,
            ],
            [
                "name": "no-body-equal-count-remains-unproven",
                "pass": assistantCopyCandidate(
                    unrelated,
                    prompt: longPrompt,
                    observedAssistantText: "",
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == nil,
            ],
            [
                "name": "short-body-substring-does-not-select-old-response",
                "pass": assistantCopyCandidate(
                    "An older response says OK but is not the current answer.",
                    prompt: longPrompt,
                    observedAssistantText: "OK",
                    hasPostBaselineButtonEvidence: false,
                    baselineWasEmpty: false
                ) == nil,
            ],
        ]
        return [
            "ok": cases.allSatisfy { $0["pass"] as? Bool == true },
            "status": "selfcheck-copy-policy",
            "cases": cases,
        ]
    }

    if input.action == "probe" {
        return try runProbe(input)
    }
    if input.action == "snapshot" {
        return try runSnapshot(input)
    }
    if input.action == "copyLatestAssistant" {
        let frontmostBefore = input.restoreFrontmostOnExit == true
            ? NSWorkspace.shared.frontmostApplication
            : nil
        defer {
            if input.restoreFrontmostOnExit == true {
                restoreFrontmostApplication(frontmostBefore)
            }
        }
        let appElement = try chatGPTAppElement()
        let text = copyCurrentAssistantMessage(
            appElement,
            afterBaselineCount: max(0, input.baselineCopyMessageCount ?? 0),
            prompt: input.prompt,
            observedAssistantText: input.observedAssistantText ?? ""
        )
        return [
            "ok": !text.isEmpty,
            "status": text.isEmpty ? "unavailable" : "complete",
            "assistantText": text,
            "responseCapture": text.isEmpty ? "none" : "current-turn-copy-message",
        ]
    }

    guard !normalize(input.prompt).isEmpty else {
        throw AxUploadError.message("Prompt is empty")
    }
    for filePath in input.filePaths {
        guard FileManager.default.fileExists(atPath: filePath) else {
            throw AxUploadError.message("Upload file does not exist: \(filePath)")
        }
    }

    let frontmostBefore = input.restoreFrontmostOnExit == true
        ? NSWorkspace.shared.frontmostApplication
        : nil
    defer {
        if input.restoreFrontmostOnExit == true {
            restoreFrontmostApplication(frontmostBefore)
        }
    }

    let uploadTimeoutMs = normalizeTimeoutMs(input.uploadTimeoutMs, fallback: 0)
    let appElement = try chatGPTAppElement()
    if input.newChat != false {
        try chooseNewChat(appElement)
    }
    var (_, composerRecord) = try waitForComposer(appElement, timeoutMs: uploadTimeoutMs)
    try setText(composerRecord, input.prompt)
    sleepMs(300)
    (_, composerRecord) = try waitForComposer(appElement, timeoutMs: uploadTimeoutMs)
    guard composerValue(composerRecord) == canonicalPrompt(input.prompt) else {
        throw AxUploadError.message("Composer text verification failed")
    }
    let baselineCopyButtonCount = copyMessageRecords(try chatWindow(appElement)).count

    for filePath in input.filePaths {
        try uploadFile(filePath, appElement: appElement, uploadTimeoutMs: uploadTimeoutMs)
    }
    try sendIfNeeded(appElement, prompt: input.prompt, timeoutMs: uploadTimeoutMs)
    if input.returnAfterSend == true {
        let window = try chatWindow(appElement)
        var pendingState = snapshot(window)
        // Preserve the pre-send baseline even if a very fast reply adds its Copy
        // button before this pending snapshot is returned to the Node waiter.
        pendingState["copyMessageCount"] = baselineCopyButtonCount
        return [
            "ok": true,
            "status": "pending",
            "assistantText": "",
            "state": pendingState,
        ]
    }

    let timeoutMs = normalizeTimeoutMs(input.timeoutMs, fallback: 0)
    let stableMs = input.responseStableMs ?? 8_000
    let pollMs = input.pollIntervalMs ?? 2_000
    let responseStartTimeoutMs = normalizeTimeoutMs(
        input.responseStartTimeoutMs,
        fallback: 0
    )
    let deadline = timeoutMs > 0
        ? Date().addingTimeInterval(timeoutMs / 1000.0)
        : nil
    var captureState = AssistantCaptureState()
    var lastChangedAt = Date()
    var responseStartedEver = false
    var attemptedClipboardRecovery = false
    var consecutiveEndedObservations = 0

    while deadline == nil || Date() < deadline! {
        sleepMs(pollMs)
        let window = try chatWindow(appElement)
        let state = snapshot(window)
        var nextCaptureState = advanceAssistantCapture(
            captureState,
            snapshot: assistantCaptureSnapshot(from: state, prompt: input.prompt)
        )
        if nextCaptureState.incomplete &&
            !attemptedClipboardRecovery {
            attemptedClipboardRecovery = true
            let copiedText = extractVisibleConversationTextByClipboard(appElement)
            let copiedAssistantText = extractCopiedAssistantText(copiedText, prompt: input.prompt)
            if !copiedAssistantText.isEmpty {
                nextCaptureState = AssistantCaptureState(
                    assistantText: copiedAssistantText,
                    promptVisibleEver: true,
                    promptVisibleNow: nextCaptureState.promptVisibleNow,
                    incomplete: false,
                    incompleteReason: "",
                    lastVisibleAssistantText: copiedAssistantText
                )
                responseStartedEver = true
            }
        }
        if nextCaptureState.assistantText != captureState.assistantText ||
            nextCaptureState.promptVisibleEver != captureState.promptVisibleEver ||
            nextCaptureState.promptVisibleNow != captureState.promptVisibleNow ||
            nextCaptureState.incomplete != captureState.incomplete ||
            nextCaptureState.incompleteReason != captureState.incompleteReason {
            lastChangedAt = Date()
        }
        captureState = nextCaptureState
        let answering = state["isAnswering"] as? Bool ?? false
        let sendReady = state["sendReady"] as? Bool ?? false
        if sendReady && !answering {
            consecutiveEndedObservations += 1
        } else {
            consecutiveEndedObservations = 0
        }
        let stableForMs = Date().timeIntervalSince(lastChangedAt) * 1000
        if answering || !captureState.assistantText.isEmpty {
            responseStartedEver = true
        }
        if responseStartTimeoutMs > 0 &&
            !responseStartedEver &&
            responseAccepted(state, prompt: input.prompt, captureState: captureState) &&
            !captureState.incomplete &&
            stableForMs >= responseStartTimeoutMs {
            throw AxUploadError.coded(
                "PSST_GPT_RESPONSE_NOT_STARTED",
                "ChatGPT accepted the upload prompt but never started answering."
            )
        }
        if captureState.incomplete && consecutiveEndedObservations >= 2 && stableForMs >= stableMs {
            throw AxUploadError.message(captureState.incompleteReason)
        }
        if consecutiveEndedObservations >= 2 &&
            !captureState.assistantText.isEmpty &&
            captureState.promptVisibleEver &&
            !captureState.incomplete &&
            stableForMs >= stableMs {
            let copied = copyCurrentAssistantMessage(
                appElement,
                afterBaselineCount: baselineCopyButtonCount,
                prompt: input.prompt,
                observedAssistantText: captureState.assistantText
            )
            guard !copied.isEmpty else {
                throw AxUploadError.coded(
                    "PSST_GPT_RESPONSE_CAPTURE_INCOMPLETE",
                    "Generation ended, but no current-turn Copy-message body could be captured; refusing to label visible AX text as complete."
                )
            }
            return [
                "ok": true,
                "status": "complete",
                "assistantText": copied,
                "responseCapture": "current-turn-copy-message",
                "state": state,
            ]
        }
    }

    throw AxUploadError.message("ChatGPT did not finish answering before the timeout")
}

do {
    guard CommandLine.arguments.count >= 2 else {
        throw AxUploadError.message("Missing JSON input argument")
    }
    let data = Data(CommandLine.arguments[1].utf8)
    let input = try JSONDecoder().decode(Input.self, from: data)
    try emit(try run(input))
} catch let AxUploadError.codedDetails(code, message, details) {
    fail(code, message, details: details)
} catch let AxUploadError.coded(code, message) {
    fail(code, message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("Accessibility automation is not enabled") ||
        message.localizedCaseInsensitiveContains("Accessibility is not enabled") ||
        message.localizedCaseInsensitiveContains("not trusted") {
    fail("MACOS_ACCESSIBILITY_DISABLED", message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("only the app shell") {
    fail("PSST_GPT_WINDOW_SHELL_ONLY", message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("No ChatGPT window is available") {
    fail("PSST_GPT_WINDOW_MISSING", message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("No ChatGPT composer is available") {
    fail("PSST_GPT_COMPOSER_MISSING", message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("Timed out after") {
    fail("PSST_GPT_DIRECT_AX_TIMEOUT", message)
} catch let AxUploadError.message(message)
    where message.localizedCaseInsensitiveContains("visible ChatGPT transcript") ||
        message.localizedCaseInsensitiveContains("active prompt scrolled out") {
    fail("PSST_GPT_RESPONSE_CAPTURE_INCOMPLETE", message)
} catch {
    fail("PSST_GPT_DIRECT_AX_FAILED", String(describing: error))
}
