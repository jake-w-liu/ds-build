#!/usr/bin/env swift
// PSST_TRANSPORT_REV=3
// psst_chat_relay.swift — Chat-only Accessibility relay for ChatGPT macOS app.
// NEVER uses Work / Codex agent usage. DeepSeek has no vision; pure AX only.
//
// Usage:
//   swift psst_chat_relay.swift -- "your prompt"
//   swift psst_chat_relay.swift --doctor
//   echo prompt | swift psst_chat_relay.swift --stdin
//
// Exit 0 on complete; non-zero on abort (including Work mode).

import ApplicationServices
import AppKit
import Foundation
import CoreGraphics

/// Lossless best-effort snapshot of every eagerly readable pasteboard item/type.
/// Restoring only `.string` destroys images, files, RTF, and custom clipboard data.
struct PasteboardSnapshot {
  private let items: [[NSPasteboard.PasteboardType: Data]]

  init(_ pasteboard: NSPasteboard) {
    items = (pasteboard.pasteboardItems ?? []).map { item in
      var record: [NSPasteboard.PasteboardType: Data] = [:]
      for type in item.types {
        if let data = item.data(forType: type) { record[type] = data }
      }
      return record
    }
  }

  func restore(to pasteboard: NSPasteboard) {
    pasteboard.clearContents()
    let restored: [NSPasteboardItem] = items.map { record in
      let item = NSPasteboardItem()
      for (type, data) in record { item.setData(data, forType: type) }
      return item
    }
    if !restored.isEmpty { _ = pasteboard.writeObjects(restored) }
  }
}

func restorePasteboardIfOwned(
  _ snapshot: PasteboardSnapshot,
  pasteboard: NSPasteboard,
  expectedChangeCount: Int,
  context: String
) {
  guard pasteboard.changeCount == expectedChangeCount else {
    log("clipboard: skip stale restore after external change (\(context))")
    return
  }
  snapshot.restore(to: pasteboard)
}

// MARK: - macOS wake-hold (multi-layer caffeinate for the helper lifetime)

/// Multi-layer wake hold for long ChatGPT waits (host may use displaysleep≈2m).
///
/// Layers (see `man caffeinate`):
/// 1. **Primary** `caffeinate -dims -w <self>` — display/idle/disk/system; tied to helper.
/// 2. **User-active pulses** `caffeinate -u -t <pulse>` every ~45s (bare `-u` is only 5s).
/// 3. **`ensureAlive()`** — restart dead primary + fire due pulses; call from wait loops.
///
/// Does **not** unlock an already-locked screen. Stops cleanly on every exit.
final class WakeHold {
  static let shared = WakeHold()
  private var primary: Process?
  private var pulseProcesses: [Process] = []
  private(set) var caffeinatePid: Int32?
  private var stopped = false
  private var lastPulseAt = Date.distantPast
  private var restartCount = 0
  private var pulseCount = 0

  private static let pulseIntervalSec: TimeInterval = 45
  private static let pulseHoldSec = 120
  private static let maxRestarts = 200
  private static let caffeinatePath = "/usr/bin/caffeinate"

  @discardableResult
  func start() -> Int32? {
    #if os(macOS)
    stopped = false
    restartCount = 0
    pulseCount = 0
    pulseProcesses.removeAll()
    let pid = ensurePrimary(reason: "start")
    fireUserActivePulse(force: true)
    return pid
    #else
    return nil
    #endif
  }

  /// Call from long wait / poll loops. Restarts dead primary and refreshes user-active.
  @discardableResult
  func ensureAlive() -> Int32? {
    #if os(macOS)
    if stopped { return caffeinatePid }
    let pid = ensurePrimary(reason: "ensureAlive")
    fireUserActivePulse(force: false)
    return pid
    #else
    return nil
    #endif
  }

  private func ensurePrimary(reason: String) -> Int32? {
    if let p = primary, p.isRunning { return caffeinatePid }
    if stopped { return nil }
    if restartCount >= Self.maxRestarts {
      log("wake-hold: ERROR max restarts (\(Self.maxRestarts)) reached — screen may lock")
      return nil
    }
    guard FileManager.default.isExecutableFile(atPath: Self.caffeinatePath) else {
      log("wake-hold: caffeinate missing; continuing without hold")
      return nil
    }
    let selfPid = ProcessInfo.processInfo.processIdentifier
    let p = Process()
    p.executableURL = URL(fileURLWithPath: Self.caffeinatePath)
    p.arguments = ["-dims", "-w", "\(selfPid)"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
      try p.run()
      let isRestart = primary != nil || caffeinatePid != nil
      if isRestart { restartCount += 1 }
      primary = p
      caffeinatePid = p.processIdentifier
      let tag = isRestart ? "restarted#\(restartCount)" : "started"
      log("wake-hold: \(tag) primary pid=\(p.processIdentifier) self=\(selfPid) args=-dims -w \(selfPid) via=\(reason)")
      Thread.sleep(forTimeInterval: 0.12)
      if !p.isRunning {
        log("wake-hold: WARNING primary exited immediately — screen may lock")
        primary = nil
        caffeinatePid = nil
        return nil
      }
      return caffeinatePid
    } catch {
      log("wake-hold: failed to start primary: \(error)")
      return nil
    }
  }

  private func fireUserActivePulse(force: Bool) {
    if stopped { return }
    let now = Date()
    if !force, now.timeIntervalSince(lastPulseAt) < Self.pulseIntervalSec { return }
    guard FileManager.default.isExecutableFile(atPath: Self.caffeinatePath) else { return }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: Self.caffeinatePath)
    p.arguments = ["-u", "-t", "\(Self.pulseHoldSec)"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
      try p.run()
      pulseProcesses.removeAll { !$0.isRunning }
      pulseProcesses.append(p)
      lastPulseAt = now
      pulseCount += 1
      if pulseCount == 1 || pulseCount % 10 == 0 {
        log("wake-hold: user-active pulse#\(pulseCount) pid=\(p.processIdentifier) -u -t \(Self.pulseHoldSec)")
      }
    } catch {
      log("wake-hold: pulse failed: \(error)")
    }
  }

  func stop() {
    stopped = true
    for process in pulseProcesses { terminateTracked(process) }
    pulseProcesses.removeAll()
    terminateTracked(primary)
    log("wake-hold: stopped caffeinate pid=\(caffeinatePid.map(String.init) ?? "?") restarts=\(restartCount) pulses=\(pulseCount)")
    primary = nil
    caffeinatePid = nil
  }

  private func terminateTracked(_ p: Process?) {
    guard let p else { return }
    if p.isRunning {
      p.terminate()
      let deadline = Date().addingTimeInterval(1.0)
      while p.isRunning && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.05)
      }
      if p.isRunning { kill(p.processIdentifier, SIGKILL) }
    }
  }

  deinit {
    if let p = primary, p.isRunning { p.terminate() }
    for process in pulseProcesses where process.isRunning { process.terminate() }
  }
}

// MARK: - Session preflight (locked screen = no AX)

/// macOS Accessibility + synthetic key events require an unlocked console session.
func isScreenLocked() -> Bool {
  guard let cf = CGSessionCopyCurrentDictionary() else { return false }
  let d = cf as NSDictionary
  if let locked = d["CGSSessionScreenIsLocked"] as? Bool { return locked }
  if let n = d["CGSSessionScreenIsLocked"] as? NSNumber { return n.boolValue }
  return false
}

/// Same policy as zip helper: short caps auto-upgrade to unlimited unless strict.
func resolveHelperTimeoutSec(
  requested: Double,
  strict: Bool,
  minUnlimitedBelow: Double = 3600
) -> (timeoutSec: Double, upgraded: Bool, note: String) {
  if requested <= 0 { return (0, false, "unlimited") }
  if strict { return (requested, false, "strict-keep") }
  if requested < minUnlimitedBelow {
    return (
      0,
      true,
      "auto-upgraded-short-timeout-to-unlimited (requested=\(Int(requested))s < \(Int(minUnlimitedBelow))s; pass --timeout-strict to keep short caps)"
    )
  }
  return (requested, false, "keep-long-cap")
}

/// Park until unlock (keep caffeinate). Returns false if deadline expires still locked.
@discardableResult
func waitWhileScreenLocked(deadline: Date?, context: String) -> Bool {
  if !isScreenLocked() { return true }
  var announced = false
  while isScreenLocked() {
    if let deadline, Date() >= deadline {
      log("screen-locked: deadline expired while still locked (\(context))")
      return false
    }
    if !announced {
      log("screen-locked: parking (\(context)) — unlock Mac to resume; caffeinate held")
      announced = true
    }
    _ = WakeHold.shared.ensureAlive()
    Thread.sleep(forTimeInterval: 2.5)
  }
  if announced { log("screen-locked: unlocked — resuming (\(context))") }
  return true
}

func assertInteractiveSession(deadline: Date? = nil) {
  if !waitWhileScreenLocked(deadline: deadline, context: "preflight") {
    fail(
      "PSST_GPT_SCREEN_LOCKED",
      "macOS screen stayed locked until --timeout deadline. Unlock and re-run with --timeout 0 to park until unlock."
    )
  }
}

/// Stage result under CWD/.ds/psst-gpt/ so DS can continue after the slash turn.
@discardableResult
func stageResultForDs(_ obj: [String: Any], responseText: String?) -> [String: String] {
  let cwd = FileManager.default.currentDirectoryPath
  let dir = (cwd as NSString).appendingPathComponent(".ds/psst-gpt")
  let jsonPath = (dir as NSString).appendingPathComponent("last-result.json")
  let mdPath = (dir as NSString).appendingPathComponent("last-response.md")
  let stageId = UUID().uuidString
  var staged = obj
  var paths = ["resultPath": jsonPath, "stageId": stageId]
  staged["resultPath"] = jsonPath
  staged["stageId"] = stageId
  do {
    try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    // Invalidate the prior manifest before touching its response body. A failed
    // current transaction can then never leave old JSON paired with new/missing MD.
    if FileManager.default.fileExists(atPath: jsonPath) {
      try FileManager.default.removeItem(atPath: jsonPath)
    }
    if let responseText, !responseText.isEmpty {
      try responseText.write(toFile: mdPath, atomically: true, encoding: .utf8)
      staged["responsePath"] = mdPath
      paths["responsePath"] = mdPath
    } else if FileManager.default.fileExists(atPath: mdPath) {
      // The JSON now describes a response-less result. Do not leave an older body
      // at the canonical handoff path where DS could mistake it for this turn.
      try FileManager.default.removeItem(atPath: mdPath)
      staged.removeValue(forKey: "responsePath")
    }
    let data = try JSONSerialization.data(withJSONObject: staged, options: [.prettyPrinted, .sortedKeys])
    try data.write(to: URL(fileURLWithPath: jsonPath), options: .atomic)
  } catch {
    log("stage-result: \(error)")
    return [:]
  }
  return paths
}

// MARK: - AX helpers

func copyAttr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
  var v: CFTypeRef?
  return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}

func setAttr(_ el: AXUIElement, _ name: String, _ value: CFTypeRef) -> Bool {
  AXUIElementSetAttributeValue(el, name as CFString, value) == .success
}

func axEnabled(_ el: AXUIElement) -> Bool {
  guard let value = copyAttr(el, kAXEnabledAttribute as String) else { return false }
  if let enabled = value as? Bool { return enabled }
  if let number = value as? NSNumber { return number.boolValue }
  return false
}

func s(_ el: AXUIElement, _ n: String) -> String {
  guard let v = copyAttr(el, n) else { return "" }
  if let str = v as? String { return str }
  if CFGetTypeID(v) == CFStringGetTypeID() { return (v as! CFString) as String }
  return String(describing: v)
}

func kids(_ el: AXUIElement) -> [AXUIElement] {
  guard let v = copyAttr(el, kAXChildrenAttribute as String) else { return [] }
  if let arr = v as? [AXUIElement] { return arr }
  let cf = v as! CFArray
  var out: [AXUIElement] = []
  for i in 0..<CFArrayGetCount(cf) {
    out.append(unsafeBitCast(CFArrayGetValueAtIndex(cf, i), to: AXUIElement.self))
  }
  return out
}

func press(_ el: AXUIElement) -> Bool {
  AXUIElementPerformAction(el, kAXPressAction as CFString) == .success
}

/// A real pointer click focuses Electron's web contents before dispatching the
/// button action. ChatGPT's Copy handler rejects a successful AXPress when the
/// native app is frontmost but the embedded web view does not have DOM focus.
func clickCenter(_ el: AXUIElement) -> Bool {
  guard let positionValue = copyAttr(el, kAXPositionAttribute as String),
        let sizeValue = copyAttr(el, kAXSizeAttribute as String),
        CFGetTypeID(positionValue) == AXValueGetTypeID(),
        CFGetTypeID(sizeValue) == AXValueGetTypeID() else { return false }
  var position = CGPoint.zero
  var size = CGSize.zero
  guard AXValueGetValue(positionValue as! AXValue, .cgPoint, &position),
        AXValueGetValue(sizeValue as! AXValue, .cgSize, &size),
        size.width > 0,
        size.height > 0 else { return false }
  let center = CGPoint(x: position.x + size.width / 2, y: position.y + size.height / 2)
  let source = CGEventSource(stateID: .hidSystemState)
  guard let move = CGEvent(
    mouseEventSource: source,
    mouseType: .mouseMoved,
    mouseCursorPosition: center,
    mouseButton: .left
  ), let down = CGEvent(
    mouseEventSource: source,
    mouseType: .leftMouseDown,
    mouseCursorPosition: center,
    mouseButton: .left
  ), let up = CGEvent(
    mouseEventSource: source,
    mouseType: .leftMouseUp,
    mouseCursorPosition: center,
    mouseButton: .left
  ) else { return false }
  move.post(tap: .cghidEventTap)
  Thread.sleep(forTimeInterval: 0.08)
  down.post(tap: .cghidEventTap)
  up.post(tap: .cghidEventTap)
  Thread.sleep(forTimeInterval: 0.18)
  return true
}

func log(_ m: String) {
  FileHandle.standardError.write((m + "\n").data(using: .utf8)!)
}

func bfsAll(_ root: AXUIElement, max: Int = 20000, pred: (AXUIElement, String) -> Bool) -> [AXUIElement] {
  var out: [AXUIElement] = []
  var q: [AXUIElement] = [root]
  var i = 0, n = 0
  while i < q.count && n < max {
    let el = q[i]; i += 1; n += 1
    let r = s(el, kAXRoleAttribute as String)
    if pred(el, r) { out.append(el) }
    q.append(contentsOf: kids(el))
  }
  return out
}

func emitJSON(_ obj: [String: Any]) {
  if let data = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]),
     let str = String(data: data, encoding: .utf8) {
    FileHandle.standardOutput.write((str + "\n").data(using: .utf8)!)
  }
}

func fail(_ code: String, _ message: String, exitCode: Int32 = 1) -> Never {
  WakeHold.shared.stop()
  var payload: [String: Any] = [
    "ok": false,
    "status": "error",
    "code": code,
    "message": message,
    "surface": "psst-gpt-chat",
    "mode": "chat",
    "wakeHoldReleased": true,
  ]
  let staged = stageResultForDs(payload, responseText: nil)
  payload["handoffStaged"] = !staged.isEmpty
  if let stageId = staged["stageId"] { payload["handoffStageId"] = stageId }
  if let path = staged["resultPath"] { payload["resultPath"] = path }
  emitJSON(payload)
  exit(exitCode)
}

// MARK: - App resolve (ChatGPT desktop; bundle id is com.openai.codex on current app)

func findChatGPTApp() -> NSRunningApplication? {
  let running = NSWorkspace.shared.runningApplications
  if let a = running.first(where: { $0.bundleIdentifier == "com.openai.codex" }) { return a }
  if let a = running.first(where: { $0.bundleIdentifier == "com.openai.chat" }) { return a }
  if let a = running.first(where: { $0.localizedName == "ChatGPT" }) { return a }
  return nil
}

// MARK: - Chat / Work

func chatWork(_ root: AXUIElement) -> (chat: AXUIElement?, work: AXUIElement?, chatOn: Bool, workOn: Bool) {
  let chat = bfsAll(root, pred: { el, r in
    r.contains("Check") && s(el, kAXTitleAttribute as String) == "Chat"
  }).first
  let work = bfsAll(root, pred: { el, r in
    r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work"
  }).first
  let chatOn = (chat.map { s($0, kAXValueAttribute as String) } ?? "") == "1"
  let workOn = (work.map { s($0, kAXValueAttribute as String) } ?? "") == "1"
  return (chat, work, chatOn, workOn)
}

/// Ensure ChatGPT product mode (not Codex) and composer Chat (never Work).
func ensureChatOnly(_ root: AXUIElement) {
  // Product switcher: ChatGPT vs Codex
  if let sw = bfsAll(root, pred: { el, r in
    r.contains("PopUp") && s(el, kAXDescriptionAttribute as String).lowercased().contains("switch mode")
  }).first {
    let d = s(sw, kAXDescriptionAttribute as String)
    log("switchMode=\(d)")
    if d.localizedCaseInsensitiveContains("Codex") {
      _ = press(sw)
      Thread.sleep(forTimeInterval: 0.9)
      if let opt = bfsAll(root, pred: { el, r in
        r.contains("MenuItem") && s(el, kAXTitleAttribute as String).contains("Create, learn")
      }).first {
        log("select product ChatGPT")
        _ = press(opt)
        Thread.sleep(forTimeInterval: 1.2)
      }
    }
  }

  for attempt in 1...8 {
    let st = chatWork(root)
    log("mode attempt=\(attempt) chatOn=\(st.chatOn) workOn=\(st.workOn)")
    if st.workOn {
      // Prefer pressing Chat (never "use" Work)
      if let chat = st.chat {
        log("Work ON → press Chat only")
        _ = press(chat)
        Thread.sleep(forTimeInterval: 0.9)
        continue
      }
      fail("PSST_GPT_WORK_MODE", "Work mode is on and Chat control not found. Open ChatGPT Chat (not Work) manually — Work has no credits.")
    }
    if st.chatOn && !st.workOn { return }
    if let chat = st.chat, !st.chatOn {
      log("press Chat checkbox")
      _ = press(chat)
      Thread.sleep(forTimeInterval: 0.9)
      continue
    }
    // Checkbox absent is OK if composer is already Message ChatGPT
    if findChatComposer(root) != nil { return }
    Thread.sleep(forTimeInterval: 0.35)
  }

  if chatWork(root).workOn {
    fail("PSST_GPT_WORK_MODE", "Refusing Work mode (no Work credits). Switch the app to Chat manually.")
  }
}

func findChatComposer(_ root: AXUIElement) -> AXUIElement? {
  let tas = bfsAll(root, pred: { _, r in r == "AXTextArea" || r == "AXTextField" })
  if let t = tas.first(where: {
    s($0, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
  }) {
    return t
  }
  if tas.contains(where: {
    let b = (s($0, kAXDescriptionAttribute as String) + s($0, "AXPlaceholderValue") + s($0, kAXValueAttribute as String)).lowercased()
    return b.contains("work with")
  }) {
    return nil // Work composer only
  }
  return nil
}

func newChat(_ root: AXUIElement) {
  if let nc = bfsAll(root, pred: { el, r in
    r == "AXButton" && (s(el, kAXTitleAttribute as String) == "New chat" || s(el, kAXDescriptionAttribute as String) == "New chat")
  }).first {
    log("New chat press=\(press(nc))")
    Thread.sleep(forTimeInterval: 1.1)
  }
}

func trySelectFlash(_ root: AXUIElement) {
  guard let modelBtn = bfsAll(root, pred: { el, r in
    r == "AXButton" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Select ChatGPT model")
  }).first else {
    log("no model button")
    return
  }
  log("open model picker")
  _ = press(modelBtn)
  Thread.sleep(forTimeInterval: 1.0)
  let flash = bfsAll(root, pred: { el, r in
    let blob = (s(el, kAXTitleAttribute as String) + " " + s(el, kAXDescriptionAttribute as String)).lowercased()
    let ctrl = r == "AXButton" || r.contains("MenuItem") || r.contains("Radio") || r.contains("Cell")
    return ctrl && blob.contains("flash")
  })
  log("flashOptions=\(flash.count)")
  if let pick = flash.first {
    log("pickFlash=\(press(pick))")
    Thread.sleep(forTimeInterval: 0.5)
  } else {
    let src = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: src, virtualKey: 53, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: src, virtualKey: 53, keyDown: false)?.post(tap: .cghidEventTap)
    log("no flash control; keep current Chat model")
  }
}

func key(_ code: CGKeyCode, flags: CGEventFlags = []) {
  let src = CGEventSource(stateID: .hidSystemState)
  CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true).map {
    $0.flags = flags; $0.post(tap: .cghidEventTap)
  }
  CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false).map {
    $0.flags = flags; $0.post(tap: .cghidEventTap)
  }
}

func findSendButton(_ root: AXUIElement, requireEnabled: Bool = true) -> AXUIElement? {
  bfsAll(root, pred: { el, r in
    guard r == "AXButton", !requireEnabled || axEnabled(el) else { return false }
    let blob = (s(el, kAXDescriptionAttribute as String) + " " + s(el, kAXTitleAttribute as String))
    return blob.localizedCaseInsensitiveContains("Send message") ||
      blob.trimmingCharacters(in: .whitespacesAndNewlines).localizedCaseInsensitiveCompare("Send") == .orderedSame
  }).first
}

func findStopButton(_ root: AXUIElement) -> AXUIElement? {
  bfsAll(root, pred: { el, r in
    guard r == "AXButton" else { return false }
    let blob = s(el, kAXDescriptionAttribute as String) + " " + s(el, kAXTitleAttribute as String)
    return blob.localizedCaseInsensitiveContains("Stop")
  }).first
}

func isRelayChrome(_ text: String) -> Bool {
  let l = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
  if l.isEmpty { return true }
  let exact: Set<String> = [
    "new chat", "projects", "sites", "scheduled", "plugins", "recents",
    "message chatgpt", "work with chatgpt", "chatgpt", "search", "send",
    "add files and more", "select chatgpt model", "dictate", "share", "chat", "work",
  ]
  if exact.contains(l) { return true }
  if l.hasPrefix("pin chat") || l.hasPrefix("archive chat") || l.hasPrefix("jump to ") { return true }
  let loading = [
    "chatgpt is responding", "systems are thinking", "thinking a bit more",
    "for a quicker response", "before responding", "may be less capable",
  ]
  if loading.contains(where: { l.contains($0) }) { return true }
  return false
}

func relayCaptureTexts(_ root: AXUIElement) -> [String] {
  bfsAll(root, pred: { _, r in
    r == "AXStaticText" || r == "AXTextArea" || r == "AXTextField" || r == "AXGroup"
  }).compactMap { el in
    let role = s(el, kAXRoleAttribute as String)
    if (role == "AXTextArea" || role == "AXTextField") &&
       s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT") {
      return nil
    }
    let value = s(el, kAXValueAttribute as String).trimmingCharacters(in: .whitespacesAndNewlines)
    if value.isEmpty || isRelayChrome(value) { return nil }
    if role == "AXGroup" && value.count < 40 { return nil }
    return value
  }
}

func relayCopyButtons(_ root: AXUIElement) -> [AXUIElement] {
  bfsAll(root, pred: { el, r in
    guard r == "AXButton" else { return false }
    let blob = (
      s(el, kAXDescriptionAttribute as String) + " " +
      s(el, kAXTitleAttribute as String) + " " +
      s(el, kAXHelpAttribute as String)
    ).lowercased()
    return blob.contains("copy message") ||
      (blob.contains("copy") && blob.contains("message")) ||
      blob.trimmingCharacters(in: .whitespacesAndNewlines) == "copy"
  })
}

/// Activate both the native ChatGPT app and its AX window immediately before a
/// Copy click. Activation alone is insufficient in Electron: the app can be
/// frontmost while the embedded web view still reports `document.hasFocus()`
/// as false.
func focusedChatGPTRootForCopy() -> AXUIElement? {
  guard let app = findChatGPTApp() else { return nil }
  let activated = app.activate(options: [.activateAllWindows])
  let focusedRoot = AXUIElementCreateApplication(app.processIdentifier)
  _ = setAttr(focusedRoot, kAXFrontmostAttribute as String, kCFBooleanTrue)
  if let window = bfsAll(focusedRoot, pred: { _, role in role == "AXWindow" }).first {
    _ = setAttr(window, kAXMainAttribute as String, kCFBooleanTrue)
    _ = setAttr(window, kAXFocusedAttribute as String, kCFBooleanTrue)
  }
  Thread.sleep(forTimeInterval: 0.3)
  log("copy-focus activated=\(activated) pid=\(app.processIdentifier)")
  return focusedRoot
}

func requestedExactReply(_ prompt: String) -> String? {
  let patterns = [
    #"\b(?:reply|respond|return|output|print|say)\s+(?:with\s+)?exactly(?:\s+with)?(?:\s+(?:the|this))?(?:\s+(?:token|string))?\s*[:=]?\s*[`\"“]([^`\"”\n]{1,256})[`\"”]"#,
    #"\b(?:reply|respond|return|output|print|say)\s+(?:with\s+)?exactly(?:\s+with)?(?:\s+(?:the|this))?(?:\s+(?:token|string))?\s*[:=]?\s*([A-Za-z0-9][A-Za-z0-9_.:-]*(?:\s+[0-9]+)?)(?=\s*(?:and\s+nothing\s+else)?[.!?\n]|$)"#,
  ]
  let fullRange = NSRange(prompt.startIndex..<prompt.endIndex, in: prompt)
  for pattern in patterns {
    guard let regex = try? NSRegularExpression(pattern: pattern, options: [.caseInsensitive]),
          let match = regex.firstMatch(in: prompt, range: fullRange),
          match.numberOfRanges > 1,
          let range = Range(match.range(at: 1), in: prompt) else { continue }
    let value = prompt[range].trimmingCharacters(in: .whitespacesAndNewlines)
    if !value.isEmpty { return value }
  }
  return nil
}

func mergeRelayBody(_ current: String, _ candidate: String) -> String {
  let a = current.trimmingCharacters(in: .whitespacesAndNewlines)
  let b = candidate.trimmingCharacters(in: .whitespacesAndNewlines)
  if b.isEmpty { return a }
  if a.isEmpty || b == a { return b }
  if b.contains(a) && b.count > a.count { return b }
  if a.contains(b) { return a }
  return b.count > a.count ? b : a
}

func bestRelayAxBody(
  texts: [String],
  baseline: Set<String>,
  prompt: String,
  exactReply: String?
) -> String {
  if let exactReply,
     let hit = texts.first(where: {
       let t = $0.trimmingCharacters(in: .whitespacesAndNewlines)
       if baseline.contains($0) || t == prompt || prompt.contains(t) { return false }
       return t == exactReply ||
         (t.contains(exactReply) && t.count <= exactReply.count + 80 && !t.lowercased().contains("reply with"))
     }) {
    return hit.trimmingCharacters(in: .whitespacesAndNewlines)
  }
  let filtered = texts.filter { t in
    !baseline.contains(t) && t != prompt && !prompt.contains(t) && !isRelayChrome(t)
  }
  var unique: [String] = []
  for text in filtered {
    if unique.contains(where: { $0 == text || ($0.contains(text) && $0.count > text.count) }) { continue }
    unique.removeAll { text.contains($0) && text.count > $0.count }
    unique.append(text)
  }
  let longest = unique.max(by: { $0.count < $1.count }) ?? ""
  let joined = unique.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
  return joined.count > longest.count ? joined : longest
}

func harvestCurrentRelayCopy(
  baselineCopyCount: Int,
  prompt: String
) -> String {
  guard let focusedRoot = focusedChatGPTRootForCopy() else { return "" }
  let buttons = relayCopyButtons(focusedRoot)
  guard buttons.count > baselineCopyCount else { return "" }
  let pb = NSPasteboard.general
  var best = ""
  for button in buttons.dropFirst(baselineCopyCount).suffix(4).reversed() {
    let snapshot = PasteboardSnapshot(pb)
    let sentinel = "PSST_COPY_SENTINEL_\(UUID().uuidString)"
    pb.clearContents()
    _ = pb.setString(sentinel, forType: .string)
    let method: String
    if clickCenter(button) {
      method = "mouseClick"
    } else {
      method = "axPress-fallback"
      _ = press(button)
    }
    let copiedChangeCount = pb.changeCount
    let raw = pb.string(forType: .string) ?? ""
    let text = raw == sentinel ? "" : raw.trimmingCharacters(in: .whitespacesAndNewlines)
    log("copy-attempt method=\(method) changed=\(raw != sentinel) chars=\(text.count)")
    // The pressed button is itself bound to this turn by baselineCopyCount. Do
    // not require AX-text overlap: Copy is the recovery path when AX body text is absent.
    restorePasteboardIfOwned(
      snapshot,
      pasteboard: pb,
      expectedChangeCount: copiedChangeCount,
      context: "copy-message"
    )
    if text.isEmpty || text == prompt || isRelayChrome(text) { continue }
    best = mergeRelayBody(best, text)
  }
  return best
}

func selectEndedRelayBody(axBody: String, copiedBody: String) -> String {
  let copied = copiedBody.trimmingCharacters(in: .whitespacesAndNewlines)
  return copied.isEmpty ? axBody : copied
}

func relayResultContract(
  text: String,
  status: String,
  code: String?,
  wakePid: pid_t?
) -> [String: Any] {
  var payload: [String: Any] = [
    "ok": status == "complete",
    "status": status,
    "surface": "psst-gpt-chat",
    "mode": "chat",
    "workOn": false,
    "wakeHoldPid": wakePid as Any,
    "wakeHoldReleased": true,
    "responseChars": text.count,
  ]
  if let code { payload["code"] = code }
  if !text.isEmpty {
    if status == "complete" {
      payload["finalDeliveryText"] = text
      payload["mustReturnFinalDelivery"] = true
      payload["mustReturnVerbatim"] = true
    } else {
      payload["partial"] = text
      payload["partialIsDiagnosticOnly"] = true
      payload["mustNotReturnAsComplete"] = true
    }
  }
  return payload
}

struct RelayControls {
  let stop: Bool
  let send: Bool
  var active: Bool { stop && !send }
  var ended: Bool { send }
  var ambiguous: Bool { !stop && !send }
}

enum RelayPhase: String {
  case awaitingStart, active, settling, complete, captureFailed
}

func classifyRelayPhase(
  controls: RelayControls,
  body: String,
  sawThisTurn: Bool,
  stableSnapshots: Int,
  endedObservations: Int
) -> RelayPhase {
  if controls.active { return .active }
  if !sawThisTurn { return .awaitingStart }
  let acceptable = !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
  let strongEnd = controls.ended
  // Stop/Send both absent is AX ambiguity, not proof of completion. Require
  // Send-ready so unchanged partial text can never finalize an active reply.
  guard strongEnd else { return .active }
  if strongEnd && endedObservations < 2 { return .settling }
  if acceptable && stableSnapshots >= 3 { return .complete }
  if strongEnd && !acceptable && endedObservations >= 40 { return .captureFailed }
  return .settling
}

// MARK: - Commands

func doctor() {
  guard process.platformIsDarwin else {
    fail("PSST_GPT_UNSUPPORTED_PLATFORM", "macOS only")
  }
  let wakePid = WakeHold.shared.start()
  defer { WakeHold.shared.stop() }
  // Doctor must report lock status immediately — do not park forever.
  if isScreenLocked() {
    emitJSON([
      "ok": false,
      "status": "screen-locked",
      "code": "PSST_GPT_SCREEN_LOCKED",
      "screenLocked": true,
      "mode": "unknown",
      "workOn": false,
      "message": "macOS screen is locked. Unlock the console, leave ChatGPT on Chat, then re-run --doctor.",
      "wakeHoldPid": wakePid as Any,
      "wakeHoldReleased": true,
      "surface": "psst-gpt-chat",
    ])
    exit(30)
  }
  let appInstalled =
    FileManager.default.fileExists(atPath: "/Applications/ChatGPT.app") ||
    FileManager.default.fileExists(atPath: NSHomeDirectory() + "/Applications/ChatGPT.app")
  guard appInstalled else {
    fail("PSST_GPT_NOT_INSTALLED", "ChatGPT.app not found in /Applications or ~/Applications")
  }
  guard let app = findChatGPTApp() else {
    fail("PSST_GPT_NOT_RUNNING", "ChatGPT is not running. Open the app and open a Chat window (not Work).")
  }
  let root = AXUIElementCreateApplication(app.processIdentifier)
  ensureChatOnly(root)
  guard findChatComposer(root) != nil else {
    fail("PSST_GPT_NO_CHAT_COMPOSER", "No Message ChatGPT composer. Switch to Chat (not Work) and open a chat window.")
  }
  let st = chatWork(root)
  var payload: [String: Any] = [
    "ok": true,
    "status": "ready",
    "surface": "psst-gpt-chat",
    "mode": "chat",
    "bundleId": app.bundleIdentifier ?? "",
    "chatOn": st.chatOn,
    "workOn": st.workOn,
    "composer": "Message ChatGPT",
    "screenLocked": false,
    "wakeHoldPid": wakePid as Any,
    "message": "Chat-only relay ready (Work refused). Screen must stay unlocked for the run.",
  ]
  let staged = stageResultForDs(payload, responseText: nil)
  payload["handoffStaged"] = !staged.isEmpty
  if let stageId = staged["stageId"] { payload["handoffStageId"] = stageId }
  if let path = staged["resultPath"] { payload["resultPath"] = path }
  WakeHold.shared.stop()
  payload["wakeHoldReleased"] = true
  emitJSON(payload)
  exit(0)
}

func relay(prompt: String, timeoutSec: Double, newChatFlag: Bool, preferFlash: Bool) {
  let startedAt = Date()
  let operationDeadline: Date? = timeoutSec > 0 ? startedAt.addingTimeInterval(timeoutSec) : nil
  let wakePid = WakeHold.shared.start()
  assertInteractiveSession(deadline: operationDeadline)
  guard !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
    fail("PSST_GPT_EMPTY_PROMPT", "Prompt is empty")
  }
  log("relay wakeHoldPid=\(wakePid.map(String.init) ?? "none") timeoutSec=\(timeoutSec)")
  guard let app = findChatGPTApp() else {
    fail("PSST_GPT_NOT_RUNNING", "ChatGPT is not running. Open ChatGPT and use Chat (not Work).")
  }
  var root = AXUIElementCreateApplication(app.processIdentifier)
  ensureChatOnly(root)
  if newChatFlag {
    newChat(root)
    root = AXUIElementCreateApplication(app.processIdentifier)
    ensureChatOnly(root)
  }
  if preferFlash {
    trySelectFlash(root)
    ensureChatOnly(root)
  }
  guard let composer = findChatComposer(root) else {
    fail("PSST_GPT_NO_CHAT_COMPOSER", "Message ChatGPT composer not found. You are not on Chat — do not use Work.")
  }
  if chatWork(root).workOn {
    fail("PSST_GPT_WORK_MODE", "Work is ON at send time — abort (no Work credits).")
  }

  func composerContent(_ value: String) -> String {
    var content = value.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
    if content == "Message ChatGPT" || content == "\nMessage ChatGPT" { return "" }
    if content.hasSuffix("\nMessage ChatGPT") {
      content.removeLast("\nMessage ChatGPT".count)
    }
    return content
  }

  func promptLooksSet(_ value: String) -> Bool {
    let v = composerContent(value)
    let e = prompt.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
    return !v.isEmpty && v == e
  }

  var baseline = Set(relayCaptureTexts(root))
  _ = AXUIElementSetAttributeValue(composer, kAXFocusedAttribute as CFString, kCFBooleanTrue)
  var composerValue = ""
  for attempt in 1...3 {
    let ok = setAttr(composer, kAXValueAttribute as String, prompt as CFTypeRef)
    Thread.sleep(forTimeInterval: 0.4)
    composerValue = s(composer, kAXValueAttribute as String)
    log("composer set attempt=\(attempt) ok=\(ok) chars=\(composerValue.count)")
    if promptLooksSet(composerValue) { break }
    _ = AXUIElementSetAttributeValue(composer, kAXFocusedAttribute as CFString, kCFBooleanTrue)
  }
  if !promptLooksSet(composerValue) {
    let pasteboard = NSPasteboard.general
    let snapshot = PasteboardSnapshot(pasteboard)
    _ = AXUIElementSetAttributeValue(composer, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    key(0, flags: .maskCommand) // Cmd+A within the composer
    Thread.sleep(forTimeInterval: 0.1)
    pasteboard.clearContents()
    _ = pasteboard.setString(prompt, forType: .string)
    let promptClipboardChangeCount = pasteboard.changeCount
    key(9, flags: .maskCommand) // Cmd+V
    Thread.sleep(forTimeInterval: 0.5)
    composerValue = s(composer, kAXValueAttribute as String)
    restorePasteboardIfOwned(
      snapshot,
      pasteboard: pasteboard,
      expectedChangeCount: promptClipboardChangeCount,
      context: "prompt-paste"
    )
    log("composer clipboard fallback chars=\(composerValue.count)")
  }
  guard promptLooksSet(composerValue) else {
    fail("PSST_GPT_SET_FAILED", "Could not prove the requested prompt was set in the Chat composer.")
  }
  baseline.formUnion(relayCaptureTexts(root).filter { prompt.contains($0) || $0.contains(String(prompt.prefix(40))) })
  let baselineCopyCount = relayCopyButtons(root).count
  let preSendChars = composerValue.count

  func messageLooksSent() -> Bool {
    if findStopButton(root) != nil { return true }
    guard let current = findChatComposer(root) else { return false }
    let after = composerContent(s(current, kAXValueAttribute as String))
    if promptLooksSet(after) { return false }
    if after.isEmpty { return true }
    return preSendChars > 40 && after.count + 80 < preSendChars
  }

  var sendVerified = false
  var sendAttempt = 0
  var lastSendRefresh = Date.distantPast
  while !sendVerified {
    if let operationDeadline, Date() >= operationDeadline {
      fail("PSST_GPT_SEND_TIMEOUT", "The user-supplied --timeout expired before ChatGPT accepted the message.", exitCode: 2)
    }
    if !waitWhileScreenLocked(deadline: operationDeadline, context: "chat-send") {
      fail("PSST_GPT_SEND_TIMEOUT", "The screen stayed locked until the user-supplied --timeout expired.", exitCode: 30)
    }
    if Date().timeIntervalSince(lastSendRefresh) >= 20 {
      root = AXUIElementCreateApplication(app.processIdentifier)
      lastSendRefresh = Date()
      ensureChatOnly(root)
    }
    if sendAttempt > 0 && messageLooksSent() {
      Thread.sleep(forTimeInterval: 0.6)
      if messageLooksSent() {
        sendVerified = true
        log("send VERIFIED before retry attempt=\(sendAttempt + 1) (double-check ok)")
        break
      }
    }
    guard let send = findSendButton(root) else {
      _ = WakeHold.shared.ensureAlive()
      Thread.sleep(forTimeInterval: 0.75)
      continue
    }
    sendAttempt += 1
    if sendAttempt % 2 == 1 {
      log("send attempt=\(sendAttempt) method=axPress ok=\(press(send))")
    } else {
      key(36, flags: .maskCommand)
      log("send attempt=\(sendAttempt) method=cmd-return")
    }
    Thread.sleep(forTimeInterval: 1.0)
    if messageLooksSent() {
      Thread.sleep(forTimeInterval: 0.6)
      if messageLooksSent() {
        sendVerified = true
        log("send VERIFIED on attempt=\(sendAttempt) (double-check ok)")
        break
      }
    }
  }
  let exactReply = requestedExactReply(prompt)
  let deadline = operationDeadline
  var phase = RelayPhase.awaitingStart
  var phaseEnteredAt = Date()
  var best = ""
  var sawThisTurn = false
  var sawAuthoritativeCopy = false
  var fingerprint = ""
  var stableSnapshots = 0
  var endedObservations = 0
  var tick = 0

  func finish(_ text: String, status: String, code: String? = nil, exitCode: Int32) -> Never {
    WakeHold.shared.stop()
    var payload = relayResultContract(text: text, status: status, code: code, wakePid: wakePid)
    let staged = stageResultForDs(payload, responseText: text.isEmpty ? nil : text)
    payload["handoffStaged"] = !staged.isEmpty
    if let stageId = staged["stageId"] { payload["handoffStageId"] = stageId }
    if let path = staged["resultPath"] { payload["resultPath"] = path }
    if let path = staged["responsePath"] { payload["responsePath"] = path }
    emitJSON(payload)
    exit(exitCode)
  }

  while deadline == nil || Date() < deadline! {
    let inPhase = Date().timeIntervalSince(phaseEnteredAt)
    let interval: TimeInterval
    if phase == .active {
      interval = inPhase < 60 ? 2.5 : (inPhase < 300 ? 5 : (inPhase < 1800 ? 8 : 12))
    } else {
      interval = 2
    }
    Thread.sleep(forTimeInterval: interval)
    tick += 1
    _ = WakeHold.shared.ensureAlive()
    if !waitWhileScreenLocked(deadline: deadline, context: "relay-wait") {
      finish(best, status: "partial", code: "PSST_GPT_SCREEN_LOCKED", exitCode: 30)
    }
    if tick == 1 || tick % 12 == 0 {
      root = AXUIElementCreateApplication(app.processIdentifier)
      log("ax-refresh tick=\(tick)")
    }
    if chatWork(root).workOn {
      finish(best, status: "partial", code: "PSST_GPT_WORK_MODE", exitCode: 20)
    }

    var controls = RelayControls(
      stop: findStopButton(root) != nil,
      send: findSendButton(root, requireEnabled: false) != nil
    )
    if controls.active { sawThisTurn = true }
    let texts = relayCaptureTexts(root)
    let axBody = bestRelayAxBody(texts: texts, baseline: baseline, prompt: prompt, exactReply: exactReply)
    best = mergeRelayBody(best, axBody)
    if !axBody.isEmpty { sawThisTurn = true }

    if controls.ended || phase == .settling {
      let copied = harvestCurrentRelayCopy(
        baselineCopyCount: baselineCopyCount,
        prompt: prompt
      )
      if !copied.isEmpty {
        log("relay current-turn Copy-message authoritative chars=\(copied.count) axChars=\(best.count)")
        best = selectEndedRelayBody(axBody: best, copiedBody: copied)
        sawThisTurn = true
        sawAuthoritativeCopy = true
      }
    }

    controls = RelayControls(
      stop: findStopButton(root) != nil,
      send: findSendButton(root, requireEnabled: false) != nil
    )
    let newFingerprint = "\(best.count)|\(best.prefix(64))|\(best.suffix(64))"
    if !best.isEmpty && newFingerprint == fingerprint {
      stableSnapshots += 1
    } else {
      fingerprint = newFingerprint
      stableSnapshots = best.isEmpty ? 0 : 1
    }
    if controls.ended { endedObservations += 1 } else { endedObservations = 0 }

    let newPhase = classifyRelayPhase(
      controls: controls,
      body: best,
      sawThisTurn: sawThisTurn,
      stableSnapshots: stableSnapshots,
      endedObservations: endedObservations
    )
    if newPhase != phase {
      log("relay phase \(phase.rawValue) -> \(newPhase.rawValue) tick=\(tick) chars=\(best.count)")
      phase = newPhase
      phaseEnteredAt = Date()
    }
    log(
      "relay tick=\(tick) phase=\(phase.rawValue) chars=\(best.count) stable=\(stableSnapshots) " +
      "ended=\(endedObservations) stop=\(controls.stop) send=\(controls.send)"
    )

    if tick % 3 == 0 {
      _ = stageResultForDs([
        "ok": false,
        "status": "waiting",
        "phase": phase.rawValue,
        "partialChars": best.count,
        "wakeHoldPid": wakePid as Any,
      ], responseText: best.isEmpty ? nil : best)
    }

    switch phase {
    case .complete:
      if exactReply == nil && !sawAuthoritativeCopy {
        if endedObservations >= 40 {
          finish(best, status: "partial", code: "PSST_GPT_COPY_CAPTURE_UNAVAILABLE", exitCode: 3)
        }
        phase = .settling
        phaseEnteredAt = Date()
        stableSnapshots = 0
        continue
      }
      finish(best, status: "complete", exitCode: 0)
    case .captureFailed:
      finish(best, status: "partial", code: "PSST_GPT_CAPTURE_FAILED", exitCode: 3)
    case .awaitingStart, .active, .settling:
      continue
    }
  }

  // A positive --timeout is an explicit user cap. Never relabel a still-active
  // or unstably captured response as complete.
  finish(best, status: "partial", code: "PSST_GPT_TIMEOUT", exitCode: 2)
}

func runSelfcheckRelayPolicy() -> Never {
  struct PhaseCase {
    let name: String
    let controls: RelayControls
    let body: String
    let saw: Bool
    let stable: Int
    let ended: Int
    let expected: RelayPhase
  }
  let cases = [
    PhaseCase(
      name: "active-never-finishes-on-stability",
      controls: RelayControls(stop: true, send: false), body: "streaming body",
      saw: true, stable: 999, ended: 0, expected: .active
    ),
    PhaseCase(
      name: "prior-body-does-not-start-turn",
      controls: RelayControls(stop: false, send: true), body: "old response",
      saw: false, stable: 99, ended: 99, expected: .awaitingStart
    ),
    PhaseCase(
      name: "ended-stable-body-completes",
      controls: RelayControls(stop: false, send: true), body: "full response",
      saw: true, stable: 3, ended: 3, expected: .complete
    ),
    PhaseCase(
      name: "ambiguous-stable-body-never-completes",
      controls: RelayControls(stop: false, send: false), body: "partial response",
      saw: true, stable: 100, ended: 0, expected: .active
    ),
    PhaseCase(
      name: "ended-empty-eventually-capture-fails",
      controls: RelayControls(stop: false, send: true), body: "",
      saw: true, stable: 0, ended: 40, expected: .captureFailed
    ),
  ]
  var results: [[String: Any]] = []
  var failed = 0
  for c in cases {
    let got = classifyRelayPhase(
      controls: c.controls,
      body: c.body,
      sawThisTurn: c.saw,
      stableSnapshots: c.stable,
      endedObservations: c.ended
    )
    let pass = got == c.expected
    if !pass { failed += 1 }
    results.append([
      "name": c.name,
      "expected": c.expected.rawValue,
      "got": got.rawValue,
      "pass": pass,
    ])
  }
  let parserCases: [(String, String?)] = [
    ("Chat only. Reply with exactly the token V4FLASH_HANDOFF_OK and nothing else.", "V4FLASH_HANDOFF_OK"),
    ("Reply exactly: READY", "READY"),
    ("Respond exactly with `ACK 1`", "ACK 1"),
    ("Discuss CONSTANT_NAME in prose.", nil),
  ]
  for (input, expected) in parserCases {
    let got = requestedExactReply(input)
    let pass = got == expected
    if !pass { failed += 1 }
    results.append([
      "name": "exact-reply-parser",
      "input": input,
      "expected": expected as Any,
      "got": got as Any,
      "pass": pass,
    ])
  }
  let shortAx = String(repeating: "A", count: 120)
  let fullCopy = shortAx + String(repeating: "B", count: 10_000)
  let merged = mergeRelayBody(shortAx, fullCopy)
  let mergePass = merged == fullCopy
  if !mergePass { failed += 1 }
  results.append([
    "name": "full-copy-beats-truncated-ax",
    "expectedChars": fullCopy.count,
    "gotChars": merged.count,
    "pass": mergePass,
  ])
  let noisyAx = String(repeating: "Old sidebar and transcript line\n", count: 500)
  let cleanCopy = "Complete first paragraph.\n\nComplete final paragraph with PLAIN_COPY_END."
  let selected = selectEndedRelayBody(axBody: noisyAx, copiedBody: cleanCopy)
  let authoritativePass = selected == cleanCopy && selected.count < noisyAx.count
  if !authoritativePass { failed += 1 }
  results.append([
    "name": "shorter-current-turn-copy-is-authoritative",
    "axChars": noisyAx.count,
    "copyChars": cleanCopy.count,
    "pass": authoritativePass,
  ])
  let testPasteboard = NSPasteboard(name: NSPasteboard.Name("psst-gpt-selfcheck-\(UUID().uuidString)"))
  let customType = NSPasteboard.PasteboardType("dev.psst.selfcheck")
  let originalString = Data("clipboard text".utf8)
  let originalCustom = Data([0, 1, 2, 3, 255])
  let originalItem = NSPasteboardItem()
  originalItem.setData(originalString, forType: .string)
  originalItem.setData(originalCustom, forType: customType)
  testPasteboard.clearContents()
  _ = testPasteboard.writeObjects([originalItem])
  let snapshot = PasteboardSnapshot(testPasteboard)
  testPasteboard.clearContents()
  _ = testPasteboard.setString("temporary", forType: .string)
  snapshot.restore(to: testPasteboard)
  let clipboardPass = testPasteboard.data(forType: .string) == originalString &&
    testPasteboard.data(forType: customType) == originalCustom
  if !clipboardPass { failed += 1 }
  results.append([
    "name": "clipboard-restores-all-types",
    "pass": clipboardPass,
  ])
  let completeContract = relayResultContract(
    text: "complete response",
    status: "complete",
    code: nil,
    wakePid: nil
  )
  let completeContractPass = completeContract["ok"] as? Bool == true &&
    completeContract["finalDeliveryText"] as? String == "complete response" &&
    completeContract["mustReturnFinalDelivery"] as? Bool == true &&
    completeContract["mustReturnVerbatim"] as? Bool == true &&
    completeContract["partial"] == nil
  if !completeContractPass { failed += 1 }
  results.append([
    "name": "complete-contract-delivers-verbatim",
    "pass": completeContractPass,
  ])
  let partialContract = relayResultContract(
    text: "diagnostic partial",
    status: "partial",
    code: "COPY_CAPTURE_UNAVAILABLE",
    wakePid: nil
  )
  let partialContractPass = partialContract["ok"] as? Bool == false &&
    partialContract["partial"] as? String == "diagnostic partial" &&
    partialContract["partialIsDiagnosticOnly"] as? Bool == true &&
    partialContract["mustNotReturnAsComplete"] as? Bool == true &&
    partialContract["finalDeliveryText"] == nil &&
    partialContract["mustReturnFinalDelivery"] == nil &&
    partialContract["mustReturnVerbatim"] == nil
  if !partialContractPass { failed += 1 }
  results.append([
    "name": "partial-contract-cannot-masquerade-as-complete",
    "pass": partialContractPass,
  ])
  let output: [String: Any] = [
    "ok": failed == 0,
    "status": "selfcheck-relay-policy",
    "failed": failed,
    "cases": results,
  ]
  emitJSON(output)
  exit(failed == 0 ? 0 : 1)
}

// platform check without ProcessInfo dance
enum process {
  static var platformIsDarwin: Bool {
    #if os(macOS)
    return true
    #else
    return false
    #endif
  }
}

func pgrepRunning(_ pid: Int32) -> Bool {
  let p = Process()
  p.executableURL = URL(fileURLWithPath: "/bin/ps")
  p.arguments = ["-p", "\(pid)", "-o", "pid="]
  let pipe = Pipe()
  p.standardOutput = pipe
  p.standardError = FileHandle.nullDevice
  try? p.run()
  p.waitUntilExit()
  let data = pipe.fileHandleForReading.readDataToEndOfFile()
  let s = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
  return p.terminationStatus == 0 && !s.isEmpty
}

// MARK: - main

var args = Array(CommandLine.arguments.dropFirst())
// timeoutSec: 0 means wait indefinitely (heavy audits)
var timeoutSec: Double = 0
var timeoutStrict = false
var newChatFlag = true
var preferFlash = true
var useStdin = false
var doctorMode = false
var selfcheckWake = false
var selfcheckRelayPolicy = false
var promptParts: [String] = []

var i = 0
while i < args.count {
  let a = args[i]
  if a == "--doctor" { doctorMode = true; i += 1; continue }
  if a == "--selfcheck-wake" { selfcheckWake = true; i += 1; continue }
  if a == "--selfcheck-relay-policy" { selfcheckRelayPolicy = true; i += 1; continue }
  if a == "--stdin" { useStdin = true; i += 1; continue }
  if a == "--no-new-chat" { newChatFlag = false; i += 1; continue }
  if a == "--no-flash" { preferFlash = false; i += 1; continue }
  if a == "--timeout-strict" { timeoutStrict = true; i += 1; continue }
  if a == "--timeout", i + 1 < args.count {
    // 0 = no overall cap (poll until complete or process killed)
    timeoutSec = Double(args[i + 1]) ?? 0
    if timeoutSec < 0 { timeoutSec = 0 }
    i += 2
    continue
  }
  if a == "--" {
    promptParts.append(contentsOf: args[(i + 1)...])
    break
  }
  if a.hasPrefix("-") {
    fail("PSST_GPT_BAD_ARGS", "Unknown flag: \(a)")
  }
  promptParts.append(a)
  i += 1
}
// Auto-upgrade short wall-clock caps unless --timeout-strict.
do {
  let resolved = resolveHelperTimeoutSec(requested: timeoutSec, strict: timeoutStrict)
  if resolved.upgraded {
    FileHandle.standardError.write(("timeout-policy: \(resolved.note)\n").data(using: .utf8)!)
  }
  timeoutSec = resolved.timeoutSec
}

if selfcheckRelayPolicy {
  runSelfcheckRelayPolicy()
}

if selfcheckWake {
  // Deterministic lifecycle + restart test for multi-layer wake hold.
  #if os(macOS)
  let pid = WakeHold.shared.start()
  let during = pid.flatMap { pgrepRunning($0) } ?? false
  // Prove ensureAlive restarts a killed primary.
  var restartOk = false
  var pidAfterRestart: Int32?
  if let p = pid, during {
    kill(p, SIGTERM)
    Thread.sleep(forTimeInterval: 0.35)
    let dead = !pgrepRunning(p)
    pidAfterRestart = WakeHold.shared.ensureAlive()
    let aliveAgain = pidAfterRestart.flatMap { pgrepRunning($0) } ?? false
    restartOk = dead && aliveAgain && (pidAfterRestart != p)
  }
  // Pulse path: force another ensureAlive after interval would normally apply.
  let pulsePid = WakeHold.shared.ensureAlive()
  let pulseAlive = pulsePid.flatMap { pgrepRunning($0) } ?? false
  Thread.sleep(forTimeInterval: 0.35)
  WakeHold.shared.stop()
  Thread.sleep(forTimeInterval: 0.35)
  let afterPrimary = pid.flatMap { pgrepRunning($0) } ?? false
  let afterRestart = pidAfterRestart.flatMap { pgrepRunning($0) } ?? false
  let ok = during && !afterPrimary && !afterRestart && pid != nil && restartOk && pulseAlive
  emitJSON([
    "ok": ok,
    "status": "selfcheck-wake",
    "caffeinatePid": pid as Any,
    "caffeinatePidAfterRestart": pidAfterRestart as Any,
    "runningDuringHold": during,
    "restartOk": restartOk,
    "runningAfterRelease": afterPrimary || afterRestart,
    "platform": "darwin",
    "layers": "primary-dims-w + user-active-pulse + ensureAlive-restart",
  ])
  exit(ok ? 0 : 1)
  #else
  emitJSON(["ok": true, "status": "selfcheck-wake", "skipped": true, "platform": "non-darwin"])
  exit(0)
  #endif
}

if doctorMode {
  doctor()
}

var prompt = promptParts.joined(separator: " ")
if useStdin || prompt.isEmpty {
  if let data = try? FileHandle.standardInput.readToEnd(),
     let str = String(data: data, encoding: .utf8) {
    let t = str.trimmingCharacters(in: .whitespacesAndNewlines)
    if !t.isEmpty { prompt = t }
  }
}

if prompt.isEmpty {
  fail("PSST_GPT_EMPTY_PROMPT", "Usage: swift psst_chat_relay.swift -- \"prompt\" | --doctor")
}

relay(prompt: prompt, timeoutSec: timeoutSec, newChatFlag: newChatFlag, preferFlash: preferFlash)
