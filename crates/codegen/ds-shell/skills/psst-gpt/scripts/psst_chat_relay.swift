#!/usr/bin/env swift
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
  private var pulse: Process?
  private var pulsePids: [Int32] = []
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
    pulsePids.removeAll()
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
      pulse = p
      pulsePids.append(p.processIdentifier)
      if pulsePids.count > 16 { pulsePids.removeFirst(pulsePids.count - 16) }
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
    for pid in pulsePids {
      if kill(pid, 0) == 0 {
        kill(pid, SIGTERM)
        Thread.sleep(forTimeInterval: 0.05)
        if kill(pid, 0) == 0 { kill(pid, SIGKILL) }
      }
    }
    pulsePids.removeAll()
    terminateTracked(pulse)
    pulse = nil
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
    if let p = pulse, p.isRunning { p.terminate() }
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

func assertInteractiveSession() {
  if isScreenLocked() {
    fail(
      "PSST_GPT_SCREEN_LOCKED",
      "macOS screen is locked. PsstGPT needs an unlocked console session (Accessibility + key events do not work while locked). Unlock the Mac, leave ChatGPT open on Chat, then retry."
    )
  }
}

/// Stage result under CWD/.ds/psst-gpt/ so DS can continue after the slash turn.
@discardableResult
func stageResultForDs(_ obj: [String: Any], responseText: String?) -> [String: String] {
  let cwd = FileManager.default.currentDirectoryPath
  let dir = (cwd as NSString).appendingPathComponent(".ds/psst-gpt")
  try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
  let jsonPath = (dir as NSString).appendingPathComponent("last-result.json")
  let mdPath = (dir as NSString).appendingPathComponent("last-response.md")
  if let data = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]) {
    try? data.write(to: URL(fileURLWithPath: jsonPath))
  }
  if let responseText, !responseText.isEmpty {
    try? responseText.write(toFile: mdPath, atomically: true, encoding: .utf8)
  }
  return ["resultPath": jsonPath, "responsePath": mdPath]
}

// MARK: - AX helpers

func copyAttr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
  var v: CFTypeRef?
  return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}

func setAttr(_ el: AXUIElement, _ name: String, _ value: CFTypeRef) -> Bool {
  AXUIElementSetAttributeValue(el, name as CFString, value) == .success
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
  emitJSON([
    "ok": false,
    "status": "error",
    "code": code,
    "message": message,
    "surface": "psst-gpt-chat",
    "mode": "chat",
    "wakeHoldReleased": true,
  ])
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

func waitAssistant(root: AXUIElement, marker: String, timeoutSec: Double) -> String? {
  var stable = 0
  var last = ""
  let deadline = Date().addingTimeInterval(timeoutSec)
  while Date() < deadline {
    Thread.sleep(forTimeInterval: 2)
    _ = WakeHold.shared.ensureAlive()
    if chatWork(root).workOn {
      fail("PSST_GPT_WORK_MODE", "Work flipped ON during wait — aborting (no Work credits).")
    }
    let texts = bfsAll(root, pred: { _, r in r == "AXStaticText" })
      .map { s($0, kAXValueAttribute as String) }.filter { !$0.isEmpty }
    let hits = texts.filter { $0.contains(marker) }
    let assistant = hits.filter {
      let t = $0.trimmingCharacters(in: .whitespacesAndNewlines)
      return t == marker || (t.contains(marker) && !t.lowercased().contains("reply with") && t.count <= marker.count + 80)
    }
    // Prefer full assistant-like hit; also accept any short marker-only line
    let sig = "\(hits.count)|\(assistant.count)"
    log("tick hits=\(hits.count) assistant=\(assistant.count) workOn=\(chatWork(root).workOn)")
    if sig == last { stable += 1 } else { stable = 0; last = sig }
    if let a = assistant.first, stable >= 2 {
      return a.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    // Broader capture: after stability, take longest hit that is not the user prompt
    if hits.count >= 2 && stable >= 3 {
      let nonUser = hits.filter { !$0.lowercased().contains("reply with") }
      return (nonUser.first ?? hits.last)?.trimmingCharacters(in: .whitespacesAndNewlines)
    }
  }
  return nil
}

// MARK: - Commands

func doctor() {
  guard process.platformIsDarwin else {
    fail("PSST_GPT_UNSUPPORTED_PLATFORM", "macOS only")
  }
  let wakePid = WakeHold.shared.start()
  defer { WakeHold.shared.stop() }
  assertInteractiveSession()
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
  payload["resultPath"] = staged["resultPath"] as Any
  WakeHold.shared.stop()
  payload["wakeHoldReleased"] = true
  emitJSON(payload)
  exit(0)
}

func relay(prompt: String, timeoutSec: Double, newChatFlag: Bool, preferFlash: Bool) {
  let wakePid = WakeHold.shared.start()
  assertInteractiveSession()
  guard !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
    fail("PSST_GPT_EMPTY_PROMPT", "Prompt is empty")
  }
  log("relay wakeHoldPid=\(wakePid.map(String.init) ?? "none") timeoutSec=\(timeoutSec)")
  guard let app = findChatGPTApp() else {
    fail("PSST_GPT_NOT_RUNNING", "ChatGPT is not running. Open ChatGPT and use Chat (not Work).")
  }
  let root = AXUIElementCreateApplication(app.processIdentifier)
  ensureChatOnly(root)
  if newChatFlag {
    newChat(root)
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

  // Unique marker for capture (appended instruction for exact-echo tests is optional)
  let marker = "PSST_CHAT_\(Int(Date().timeIntervalSince1970))"
  // Send user prompt as-is; for verification callers can include a token.
  _ = setAttr(composer, kAXValueAttribute as String, prompt as CFTypeRef)
  _ = AXUIElementSetAttributeValue(composer, kAXFocusedAttribute as CFString, kCFBooleanTrue)
  Thread.sleep(forTimeInterval: 0.3)
  let cv = s(composer, kAXValueAttribute as String)
  if cv.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
    fail("PSST_GPT_SET_FAILED", "Could not set Chat composer text")
  }
  log("composer set (\(cv.count) chars)")

  if let send = bfsAll(root, pred: { el, r in
    r == "AXButton" && (s(el, kAXDescriptionAttribute as String) == "Send" || s(el, kAXTitleAttribute as String) == "Send")
  }).first {
    log("send=\(press(send))")
  } else {
    log("Send missing; Return")
    let src = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: src, virtualKey: 36, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: src, virtualKey: 36, keyDown: false)?.post(tap: .cghidEventTap)
  }

  // Wait for response via static texts. Prefer exact short replies; filter chrome.
  let chrome: Set<String> = [
    "new chat", "projects", "sites", "scheduled", "plugins", "recents",
    "message chatgpt", "work with chatgpt", "chatgpt", "search", "send",
    "add files and more", "select chatgpt model", "dictate", "share",
  ]
  func isChrome(_ t: String) -> Bool {
    let lower = t.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    if lower.isEmpty { return true }
    if chrome.contains(lower) { return true }
    if lower.hasPrefix("pin chat") || lower.hasPrefix("archive chat") { return true }
    if lower.hasPrefix("jump to ") { return true }
    if lower == "chat" || lower == "work" { return true }
    return false
  }

  // Optional: if prompt asks for an exact token, prefer that for verification.
  let exactToken: String? = {
    // e.g. "... only this token and nothing else: FOO" or "only: FOO"
    if let r = prompt.range(of: #"\b(OK_[A-Z0-9_]+|PSST_[A-Z0-9_]+)\b"#, options: .regularExpression) {
      return String(prompt[r])
    }
    return nil
  }()

  func completeWith(_ text: String, partial: Bool) -> Never {
    var payload: [String: Any] = [
      "ok": true,
      "status": "complete",
      "surface": "psst-gpt-chat",
      "mode": "chat",
      "workOn": false,
      "mustReturnFinalDelivery": true,
      "mustReturnVerbatim": true,
      "finalDeliveryText": text,
      "message": partial
        ? "Chat-only relay complete (timeout with partial stability)"
        : "Chat-only relay complete",
    ]
    let staged = stageResultForDs(payload, responseText: text)
    payload["resultPath"] = staged["resultPath"] as Any
    payload["responsePath"] = staged["responsePath"] as Any
    WakeHold.shared.stop()
    payload["wakeHoldReleased"] = true
    emitJSON(payload)
    exit(0)
  }

  var stable = 0
  var lastSig = ""
  var lastAssistant = ""
  // timeoutSec <= 0 → wait indefinitely (heavy audit)
  let deadline: Date? = timeoutSec > 0 ? Date().addingTimeInterval(timeoutSec) : nil
  while deadline == nil || Date() < deadline! {
    Thread.sleep(forTimeInterval: 2)
    if isScreenLocked() {
      fail("PSST_GPT_SCREEN_LOCKED", "Screen locked during wait — unlock and re-run /psst-gpt (or poll the same ChatGPT chat).")
    }
    if chatWork(root).workOn {
      fail("PSST_GPT_WORK_MODE", "Work flipped ON during wait — abort.")
    }
    let texts = bfsAll(root, pred: { _, r in r == "AXStaticText" })
      .map { s($0, kAXValueAttribute as String) }
      .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
      .filter { !$0.isEmpty && !isChrome($0) }

    // Exact-token success (smoke / instructed replies)
    if let token = exactToken {
      let hits = texts.filter { $0 == token || ($0.contains(token) && $0.count <= token.count + 40 && !$0.lowercased().contains("reply with")) }
      log("tick tokenHits=\(hits.count) workOn=false")
      if let a = hits.first(where: { $0 == token }) ?? hits.first {
        if a == lastAssistant { stable += 1 } else { stable = 0; lastAssistant = a }
        if stable >= 2 {
          completeWith(lastAssistant, partial: false)
        }
        continue
      }
    }

    // General: texts that appear after the user prompt blob
    let fingerprint = String(prompt.prefix(min(64, prompt.count)))
    var after = false
    var collected: [String] = []
    for t in texts {
      if t.contains(fingerprint) || t == prompt {
        after = true
        collected = []
        continue
      }
      if after && t != prompt && !t.contains(fingerprint) {
        collected.append(t)
      }
    }
    let assistantText = collected.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    let sig = "\(assistantText.count)|\(assistantText.prefix(80))"
    log("tick assistantChars=\(assistantText.count) workOn=false")
    if sig == lastSig && !assistantText.isEmpty {
      stable += 1
    } else {
      stable = 0
      lastSig = sig
      if !assistantText.isEmpty { lastAssistant = assistantText }
    }
    if stable >= 3 && !lastAssistant.isEmpty && lastAssistant.lowercased() != "message chatgpt" {
      completeWith(lastAssistant, partial: false)
    }
  }

  if !lastAssistant.isEmpty && lastAssistant.lowercased() != "message chatgpt" {
    completeWith(lastAssistant, partial: true)
  }
  fail("PSST_GPT_TIMEOUT", "Timed out waiting for ChatGPT Chat response", exitCode: 2)
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
var newChatFlag = true
var preferFlash = true
var useStdin = false
var doctorMode = false
var selfcheckWake = false
var promptParts: [String] = []

var i = 0
while i < args.count {
  let a = args[i]
  if a == "--doctor" { doctorMode = true; i += 1; continue }
  if a == "--selfcheck-wake" { selfcheckWake = true; i += 1; continue }
  if a == "--stdin" { useStdin = true; i += 1; continue }
  if a == "--no-new-chat" { newChatFlag = false; i += 1; continue }
  if a == "--no-flash" { preferFlash = false; i += 1; continue }
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
    let dead = !(pgrepRunning(p) ?? false)
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
