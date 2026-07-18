#!/usr/bin/env swift
// Chat-only zip upload + response capture for ChatGPT macOS (com.openai.codex).
// Uses clipboard file paste (Add files menu is not AX-exposed on current app).
// NEVER uses Work mode.
//
// Usage:
//   swift psst_zip_upload.swift --zip /path/to/file.zip -- "prompt..."
//   swift psst_zip_upload.swift --root /path/to/project -- "prompt..."
//   swift psst_zip_upload.swift --zip file.zip --timeout 120 -- "Read the zip and reply..."

import ApplicationServices
import AppKit
import Foundation
import CoreGraphics

/// Multi-layer macOS wake hold for long zip audits (host may use displaysleep≈2m).
///
/// Layers (see `man caffeinate`):
/// 1. **Primary** `caffeinate -dims -w <self>` — display/idle/disk/system; tied to helper.
///    No `-u`/`-t` on primary so it cannot expire early while the helper is still up.
/// 2. **User-active pulses** `caffeinate -u -t <pulse>` every ~45s — resets idle/lock
///    timers (bare `-u` is only 5s; single long `-t` dies if the process is killed).
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

  /// How often to re-assert user-active (must be < host displaysleep, often 2 min).
  private static let pulseIntervalSec: TimeInterval = 45
  /// Duration of each user-active assertion (man: bare `-u` defaults to 5s).
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
    // Immediate user-active so idle clock does not start before first wait tick.
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
    // Primary: prevent sleep only; lifetime bound to this helper via -w.
    // User-active is handled by pulses (avoids -t expiry killing the whole hold).
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
    // -u turns display on if off and declares user active for -t seconds.
    p.arguments = ["-u", "-t", "\(Self.pulseHoldSec)"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
      try p.run()
      pulse = p
      pulsePids.append(p.processIdentifier)
      // Cap bookkeeping list (oldest already expire via -t).
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
      while p.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
      if p.isRunning { kill(p.processIdentifier, SIGKILL) }
    }
  }

  deinit {
    if let p = primary, p.isRunning { p.terminate() }
    if let p = pulse, p.isRunning { p.terminate() }
  }
}

func isScreenLocked() -> Bool {
  guard let cf = CGSessionCopyCurrentDictionary() else { return false }
  let d = cf as NSDictionary
  if let locked = d["CGSSessionScreenIsLocked"] as? Bool { return locked }
  if let n = d["CGSSessionScreenIsLocked"] as? NSNumber { return n.boolValue }
  return false
}

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
func press(_ el: AXUIElement) -> Bool { AXUIElementPerformAction(el, kAXPressAction as CFString) == .success }
func log(_ m: String) { FileHandle.standardError.write((m + "\n").data(using: .utf8)!) }
func key(_ code: CGKeyCode, flags: CGEventFlags = []) {
  let src = CGEventSource(stateID: .hidSystemState)
  let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)!
  down.flags = flags; down.post(tap: .cghidEventTap)
  let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)!
  up.flags = flags; up.post(tap: .cghidEventTap)
  Thread.sleep(forTimeInterval: 0.08)
}
func bfsAll(_ root: AXUIElement, max: Int = 25000, pred: (AXUIElement, String) -> Bool) -> [AXUIElement] {
  var out: [AXUIElement] = []; var q = [root]; var i = 0, n = 0
  while i < q.count && n < max {
    let el = q[i]; i += 1; n += 1
    let r = s(el, kAXRoleAttribute as String)
    if pred(el, r) { out.append(el) }
    q.append(contentsOf: kids(el))
  }
  return out
}
func bfsFirst(_ root: AXUIElement, pred: (AXUIElement, String) -> Bool) -> AXUIElement? {
  bfsAll(root, pred: pred).first
}

/// True when AX text is still a loading/ingest shell, not a real ChatGPT answer.
/// Pure function — also exercised by `--selfcheck-finish-rules`.
func isIncompleteZipReply(_ t: String) -> Bool {
  let trimmed = t.trimmingCharacters(in: .whitespacesAndNewlines)
  let l = trimmed.lowercased()
  if trimmed.isEmpty { return true }
  if l.contains("no sources yet") { return true }
  if l.contains("audit request for codebase") && trimmed.count < 400 { return true }
  // Mid-stream / loading chrome (must never finalize as complete)
  let loadingChrome = [
    "chatgpt is responding", "systems are thinking", "thinking a bit more",
    "untitled conversation", "for a quicker response", "learn more",
    "before responding", "may be less capable",
  ]
  if loadingChrome.contains(where: { l.contains($0) }) { return true }
  // Chat title / one-line chips are not an audit body
  if l == "audit rust monorepo" || (l.contains("audit rust monorepo") && trimmed.count < 400) {
    return true
  }
  if l.hasPrefix("untitled") && trimmed.count < 400 { return true }
  // Fragment salad: many short lines (AX word chips) without a real paragraph
  let lines = trimmed.split(whereSeparator: \.isNewline)
    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
    .filter { !$0.isEmpty }
  if lines.count >= 3 {
    let avg = lines.map(\.count).reduce(0, +) / max(lines.count, 1)
    if avg < 48 && trimmed.count < 900 { return true }
  }
  // Zip audits need a substantive body; short stubs always incomplete
  if trimmed.count < 300 { return true }
  // Mostly our own prompt headings echoed back while zip is still opening
  let promptish = ["architecture", "dependencies", "configuration", "do not suggest any code edits"]
  let hits = promptish.filter { l.contains($0) }.count
  if hits >= 2 && trimmed.count < 1500 {
    let hasFinding = l.range(of: #"\b(severity|finding|risks?|recommend)\b"#, options: .regularExpression) != nil
    if !hasFinding { return true }
  }
  // Require at least one sentence-like chunk or structured section
  let hasSentence = trimmed.contains(". ") || trimmed.contains(".\n") || trimmed.contains("##") ||
    trimmed.contains("1.") || trimmed.contains("- ")
  if !hasSentence && trimmed.count < 1200 { return true }
  return false
}

/// Drive real `isIncompleteZipReply` with known fixtures (no ChatGPT needed).
func runSelfcheckFinishRules() -> Never {
  struct Case { let name: String; let text: String; let expectIncomplete: Bool }
  let longAudit = """
  ## Executive assessment
  The monorepo shows modular crates and solid tests. Top risk: sandbox fail-open
  on unsupported platforms returns a permissive path without hard failure.
  1. Severity high: AlwaysApprove defaults expand privilege.
  2. Recommendation: fail closed when sandbox cannot apply.
  Architecture notes: permission resolver, network hooks, plugin install.
  """
  let cases: [Case] = [
    Case(name: "empty", text: "", expectIncomplete: true),
    Case(name: "no_sources", text: "No sources yet", expectIncomplete: true),
    Case(name: "title_only", text: "Audit Rust Monorepo", expectIncomplete: true),
    Case(name: "short_stub", text: "Looks fine overall with some risks mentioned briefly.", expectIncomplete: true),
    Case(name: "fragment_salad", text:
      "Audit Rust Monorepo\narchitectural\nsubstantial:\nconcentrated\nauthentication,",
      expectIncomplete: true),
    Case(name: "loading_chrome", text:
      "Untitled conversation\nChatGPT is responding\nOur systems are thinking a bit more about this request before responding.",
      expectIncomplete: true),
    Case(name: "real_audit", text: longAudit, expectIncomplete: false),
  ]
  var results: [[String: Any]] = []
  var failed = 0
  for c in cases {
    let got = isIncompleteZipReply(c.text)
    let pass = got == c.expectIncomplete
    if !pass { failed += 1 }
    results.append([
      "name": c.name,
      "expectIncomplete": c.expectIncomplete,
      "gotIncomplete": got,
      "pass": pass,
      "chars": c.text.count,
    ])
  }
  let ok = failed == 0
  let out: [String: Any] = [
    "ok": ok,
    "status": "selfcheck-finish-rules",
    "failed": failed,
    "cases": results,
  ]
  if let d = try? JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys]),
     let str = String(data: d, encoding: .utf8) {
    print(str)
  }
  exit(ok ? 0 : 1)
}

func emit(_ obj: [String: Any], exitCode: Int32 = 0, stageResponse: String? = nil) -> Never {
  var out = obj
  if exitCode == 0 || stageResponse != nil {
    let text = stageResponse ?? (obj["finalDeliveryText"] as? String)
    let staged = stageResultForDs(out, responseText: text)
    out["resultPath"] = staged["resultPath"] as Any
    if out["responsePath"] == nil {
      out["responsePath"] = staged["responsePath"] as Any
    }
  }
  WakeHold.shared.stop()
  out["wakeHoldReleased"] = true
  if let d = try? JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys]),
     let str = String(data: d, encoding: .utf8) {
    print(str)
  }
  exit(exitCode)
}

func zipRoot(_ root: String) throws -> String {
  let fm = FileManager.default
  let outDir = fm.temporaryDirectory.appendingPathComponent("psst-gpt-zip-\(UUID().uuidString)", isDirectory: true)
  try fm.createDirectory(at: outDir, withIntermediateDirectories: true)
  let zipPath = outDir.appendingPathComponent("source-archive.zip").path
  // Exclude build/cache/VCS noise so "full codebase" audits stay attachable.
  let excludes = [
    "target/*", "*/target/*",
    ".git/*",
    "node_modules/*", "*/node_modules/*",
    ".ds/sessions/*", ".ds/cache/*",
    "*.o", "*.a", "*.rlib", "*.dylib", "*.so",
    ".lyceum-trash/*",
    "third_party/*", "*/third_party/*",
    "*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp", "*.pdf",
    "*.mp4", "*.mov", "*.zip", "*.tar", "*.gz",
    "*.wasm", "*.bin",
  ]
  var args = ["-qr", zipPath, ".", "-x"]
  args.append(contentsOf: excludes)
  let proc = Process()
  proc.executableURL = URL(fileURLWithPath: "/usr/bin/zip")
  proc.arguments = args
  proc.currentDirectoryURL = URL(fileURLWithPath: root)
  try proc.run()
  proc.waitUntilExit()
  guard proc.terminationStatus == 0, fm.fileExists(atPath: zipPath) else {
    throw NSError(domain: "psst", code: 1, userInfo: [NSLocalizedDescriptionKey: "zip failed"])
  }
  if let attrs = try? fm.attributesOfItem(atPath: zipPath),
     let size = attrs[.size] as? NSNumber {
    log("zip size bytes=\(size.intValue) path=\(zipPath)")
  }
  return zipPath
}

// --- args ---
var zipPath: String?
var rootPath: String?
// 0 = wait indefinitely (heavy audits). Default 0 for long zip audits.
var timeoutSec: Double = 0
var newChat = true
var packOnly = false
var promptParts: [String] = []
var args = Array(CommandLine.arguments.dropFirst())
var i = 0
while i < args.count {
  let a = args[i]
  if a == "--zip", i + 1 < args.count { zipPath = args[i + 1]; i += 2; continue }
  if a == "--root", i + 1 < args.count { rootPath = args[i + 1]; i += 2; continue }
  if a == "--timeout", i + 1 < args.count {
    timeoutSec = Double(args[i + 1]) ?? 0
    if timeoutSec < 0 { timeoutSec = 0 }
    i += 2
    continue
  }
  if a == "--no-new-chat" { newChat = false; i += 1; continue }
  if a == "--pack-only" { packOnly = true; i += 1; continue }
  if a == "--selfcheck-finish-rules" { runSelfcheckFinishRules() }
  if a == "--selfcheck-generation-policy" { runSelfcheckGenerationPolicy() }
  if a == "--" { promptParts.append(contentsOf: args[(i + 1)...]); break }
  if a.hasPrefix("-") { emit(["ok": false, "code": "BAD_ARGS", "message": "Unknown \(a)"], exitCode: 2) }
  promptParts.append(a); i += 1
}
let prompt = promptParts.joined(separator: " ").trimmingCharacters(in: .whitespacesAndNewlines)
// --pack-only only needs a root (no ChatGPT / no prompt)
if packOnly {
  let wakePid = WakeHold.shared.start()
  defer { WakeHold.shared.stop() }
  guard let rootPath else {
    emit(["ok": false, "code": "BAD_ARGS", "message": "--pack-only requires --root"], exitCode: 2)
  }
  do {
    let path = try zipRoot(rootPath)
    let size = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? NSNumber)?.intValue ?? -1
    // Ensure excludes worked: refuse absurd sizes (> 200MB) as packaging failure for audits
    let okSize = size > 0 && size < 200 * 1024 * 1024
    emit([
      "ok": okSize,
      "status": "pack-only",
      "zipPath": path,
      "bytes": size,
      "wakeHoldPid": wakePid as Any,
      "excludesTargetGit": true,
    ], exitCode: okSize ? 0 : 1)
  } catch {
    emit(["ok": false, "code": "ZIP_FAILED", "message": "\(error)"], exitCode: 3)
  }
}
guard !prompt.isEmpty else {
  emit(["ok": false, "code": "EMPTY_PROMPT", "message": "Usage: --zip file.zip -- \"prompt\" | --root dir -- \"prompt\""], exitCode: 2)
}
// Start wake-hold before long zip+AX work (released in emit on every exit).
let wakePid = WakeHold.shared.start()
if isScreenLocked() {
  emit([
    "ok": false,
    "code": "PSST_GPT_SCREEN_LOCKED",
    "message": "macOS screen is locked. Unlock the console and leave ChatGPT Chat open — AX automation cannot run while locked.",
    "wakeHoldPid": wakePid as Any,
  ], exitCode: 30)
}

do {
  if zipPath == nil, let rootPath {
    zipPath = try zipRoot(rootPath)
    log("zipped \(rootPath) -> \(zipPath!)")
  }
} catch {
  emit(["ok": false, "code": "ZIP_FAILED", "message": "\(error)"], exitCode: 3)
}
guard let zipPath, FileManager.default.fileExists(atPath: zipPath) else {
  emit(["ok": false, "code": "ZIP_MISSING", "message": "Provide --zip or --root"], exitCode: 2)
}
let zipURL = URL(fileURLWithPath: zipPath)

guard let app = NSWorkspace.shared.runningApplications.first(where: {
  $0.bundleIdentifier == "com.openai.codex" || $0.bundleIdentifier == "com.openai.chat" || $0.localizedName == "ChatGPT"
}) else {
  emit(["ok": false, "code": "NO_APP", "message": "ChatGPT not running"], exitCode: 4)
}
_ = app.activate(options: [.activateAllWindows])
Thread.sleep(forTimeInterval: 0.5)
let root = AXUIElementCreateApplication(app.processIdentifier)

// Chat only — never leave Work on
for attempt in 1...6 {
  let chat = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Chat" })
  let work = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work" })
  let chatOn = (chat.map { s($0, kAXValueAttribute as String) } ?? "") == "1"
  let workOn = (work.map { s($0, kAXValueAttribute as String) } ?? "") == "1"
  log("mode attempt=\(attempt) chatOn=\(chatOn) workOn=\(workOn)")
  if chatOn && !workOn { break }
  if let chat {
    log("press Chat (never Work)")
    _ = press(chat)
    Thread.sleep(forTimeInterval: 0.9)
    continue
  }
  if workOn {
    emit(["ok": false, "code": "WORK_MODE", "message": "Work is on and Chat control missing — switch to Chat manually"], exitCode: 20)
  }
  Thread.sleep(forTimeInterval: 0.4)
}
if let work = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work" }),
   s(work, kAXValueAttribute as String) == "1" {
  emit(["ok": false, "code": "WORK_MODE", "message": "Work is still on — refuse (no Work credits)"], exitCode: 20)
}

if newChat, let nc = bfsFirst(root, pred: { el, r in
  r == "AXButton" && (s(el, kAXTitleAttribute as String) == "New chat" || s(el, kAXDescriptionAttribute as String) == "New chat")
}) {
  _ = press(nc); Thread.sleep(forTimeInterval: 1.1)
}

guard let composer = bfsFirst(root, pred: { el, r in
  r == "AXTextArea" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
}) else {
  emit(["ok": false, "code": "NO_CHAT_COMPOSER", "message": "Need Message ChatGPT composer (Chat mode)"], exitCode: 5)
}

// Clipboard file paste
let pb = NSPasteboard.general
let oldString = pb.string(forType: .string)
pb.clearContents()
guard pb.writeObjects([zipURL as NSURL]) else {
  emit(["ok": false, "code": "CLIPBOARD_FAILED"], exitCode: 6)
}
_ = AXUIElementSetAttributeValue(composer, kAXFocusedAttribute as CFString, kCFBooleanTrue)
Thread.sleep(forTimeInterval: 0.25)
key(9, flags: .maskCommand)
Thread.sleep(forTimeInterval: 2.0)

let zipName = zipURL.lastPathComponent.lowercased()
var attached = false
var labels: [String] = []
for el in bfsAll(root, pred: { _, _ in true }) {
  let blob = [s(el, kAXTitleAttribute as String), s(el, kAXDescriptionAttribute as String), s(el, kAXValueAttribute as String)].joined(separator: " ")
  let lower = blob.lowercased()
  if lower.contains(zipName) || (lower.contains(".zip") && lower.count < 120) {
    attached = true
    labels.append(String(blob.prefix(100)))
  }
}
log("attached=\(attached)")
if !attached {
  pb.clearContents()
  if let oldString { pb.setString(oldString, forType: .string) }
  emit(["ok": false, "code": "ATTACHMENT_MISSING", "message": "Zip did not attach in Chat composer"], exitCode: 7)
}

// Baseline static texts before send (for diff-based capture of long audits).
func isChromeText(_ t: String) -> Bool {
  let l = t.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
  if l.isEmpty { return true }
  let chrome = [
    "new chat", "projects", "plugins", "sites", "scheduled", "message chatgpt",
    "recents", "search", "send", "add files and more", "select chatgpt model",
    "chatgpt", "work", "chat",
    // Zip-ingest / loading chrome — not a finished audit body
    "no sources yet", "sources", "thinking", "searching", "analyzing",
    "audit request for codebase", "rust codebase audit",
    // Auto chat titles / chips (not body)
    "audit rust monorepo", "untitled conversation",
    "chatgpt is responding", "learn more",
  ]
  if chrome.contains(l) { return true }
  if l.hasPrefix("pin chat") || l.hasPrefix("archive chat") || l.hasPrefix("remove ") { return true }
  if l == zipName || (l.hasSuffix(".zip") && l.count < 80) { return true }
  if l.contains("source-archive") && l.count < 80 { return true }
  if l.contains("no sources yet") { return true }
  if l.contains("chatgpt is responding") || l.contains("systems are thinking") { return true }
  if l.contains("thinking a bit more") || l.contains("for a quicker response") { return true }
  // Sidebar recents / icon noise
  if l.count < 3 { return true }
  return false
}

func isComposerTextArea(_ el: AXUIElement) -> Bool {
  let r = s(el, kAXRoleAttribute as String)
  guard r == "AXTextArea" else { return false }
  return s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
}

/// Harvest reply-visible AX text (static + non-composer text areas/fields + long group values).
func allCaptureTexts() -> [String] {
  var out: [String] = []
  for el in bfsAll(root, pred: { _, r in
    r == "AXStaticText" || r == "AXTextArea" || r == "AXTextField" || r == "AXGroup"
  }) {
    if isComposerTextArea(el) { continue }
    let role = s(el, kAXRoleAttribute as String)
    let v = s(el, kAXValueAttribute as String).trimmingCharacters(in: .whitespacesAndNewlines)
    if v.isEmpty { continue }
    if role == "AXGroup" && v.count < 48 { continue }
    if isChromeText(v) { continue }
    if v.count >= 2 { out.append(v) }
  }
  return out
}
func allStaticTexts() -> [String] { allCaptureTexts() }

/// Scroll transcript so long replies stay reachable in the AX tree.
func scrollTranscript() {
  // Page Down + End (macOS key codes: 121=PageDown, 119=End)
  key(121)
  Thread.sleep(forTimeInterval: 0.12)
  key(119)
  Thread.sleep(forTimeInterval: 0.15)
}

/// Prefer ChatGPT "Copy message" buttons — clipboard often holds the full assistant body.
func harvestViaCopyMessageButtons() -> String {
  let buttons = bfsAll(root, pred: { el, r in
    guard r == "AXButton" else { return false }
    let blob = (s(el, kAXDescriptionAttribute as String) + " " + s(el, kAXTitleAttribute as String)).lowercased()
    return blob.contains("copy message") || blob == "copy message"
  })
  if buttons.isEmpty {
    log("copy-harvest: no Copy message buttons")
    return ""
  }
  var bestClip = ""
  // Last assistant messages are usually the last copy buttons in the tree order.
  for btn in buttons.suffix(6).reversed() {
    pb.clearContents()
    _ = press(btn)
    Thread.sleep(forTimeInterval: 0.45)
    let clip = (pb.string(forType: .string) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    if clip.count < 40 { continue }
    if isChromeText(clip) { continue }
    // Reject pure prompt echo
    if promptLooksSet(clip, prompt) && clip.count < prompt.count + 80 { continue }
    if clip.count > bestClip.count {
      bestClip = clip
      log("copy-harvest: clipboard chars=\(clip.count)")
    }
  }
  return bestClip
}

/// Deep harvest: scroll + AX tree + optional clipboard copy (when generation finished).
func deepHarvestReply(includeCopy: Bool) -> String {
  scrollTranscript()
  var parts: [String] = []
  var seen = Set<String>()
  for t in allCaptureTexts() {
    if seen.contains(t) { continue }
    if prompt.contains(t) && t.count < 80 { continue }
    seen.insert(t)
    parts.append(t)
  }
  var body = parts.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
  if includeCopy {
    let clip = harvestViaCopyMessageButtons()
    if clip.count > body.count {
      log("deepHarvest: preferring clipboard over AX tree (\(clip.count) > \(body.count))")
      body = clip
    }
  }
  return body
}

let baseline = Set(allCaptureTexts())
log("baseline capture texts=\(baseline.count)")

// --- Put prompt into composer and PROVE it stuck (Electron often ignores silent AX set) ---
func composerText(_ el: AXUIElement) -> String {
  s(el, kAXValueAttribute as String)
}
func focusComposer(_ el: AXUIElement) {
  _ = AXUIElementSetAttributeValue(el, kAXFocusedAttribute as CFString, kCFBooleanTrue)
  Thread.sleep(forTimeInterval: 0.15)
}
func promptLooksSet(_ value: String, _ expected: String) -> Bool {
  let v = value.trimmingCharacters(in: .whitespacesAndNewlines)
  if v.isEmpty { return false }
  let needle = String(expected.prefix(48)).trimmingCharacters(in: .whitespacesAndNewlines)
  if !needle.isEmpty && v.contains(needle) { return true }
  // Long prompts: accept substantial length match even if AX truncates slightly
  return v.count >= min(80, max(20, expected.count / 3))
}
func setComposerPrompt(_ el: AXUIElement, _ text: String) -> String {
  focusComposer(el)
  // 1) AX set
  let axOk = setAttr(el, kAXValueAttribute as String, text as CFTypeRef)
  Thread.sleep(forTimeInterval: 0.35)
  var cv = composerText(el)
  log("composer AX set ok=\(axOk) chars=\(cv.count) head=\(String(cv.prefix(80)).replacingOccurrences(of: "\n", with: " "))")
  if promptLooksSet(cv, text) { return cv }

  // 2) Clipboard string paste (updates React state more reliably than pure AX set)
  focusComposer(el)
  // Do NOT Cmd+A (would risk deselecting/clearing attachment chip). Paste at caret.
  pb.clearContents()
  pb.setString(text, forType: .string)
  key(9, flags: .maskCommand) // Cmd+V
  Thread.sleep(forTimeInterval: 0.5)
  cv = composerText(el)
  log("composer paste chars=\(cv.count) head=\(String(cv.prefix(80)).replacingOccurrences(of: "\n", with: " "))")
  if promptLooksSet(cv, text) { return cv }

  // 3) Last resort: AX set again after re-focus
  focusComposer(el)
  _ = setAttr(el, kAXValueAttribute as String, text as CFTypeRef)
  Thread.sleep(forTimeInterval: 0.35)
  cv = composerText(el)
  log("composer retry AX chars=\(cv.count)")
  return cv
}

var liveComposer = composer
var setValue = setComposerPrompt(liveComposer, prompt)
if !promptLooksSet(setValue, prompt) {
  // Re-resolve composer (window may have re-rendered after attach)
  if let c2 = bfsFirst(root, pred: { el, r in
    r == "AXTextArea" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
  }) {
    liveComposer = c2
    setValue = setComposerPrompt(liveComposer, prompt)
  }
}
if !promptLooksSet(setValue, prompt) {
  emit([
    "ok": false,
    "code": "PROMPT_SET_FAILED",
    "message": "Could not put audit prompt into Chat composer (AX/paste both failed). Message was NOT sent.",
    "composerChars": setValue.count,
    "attached": attached,
    "wakeHoldPid": wakePid as Any,
  ], exitCode: 8)
}
log("composer prompt ready chars=\(setValue.count)")

// Include prompt fragments in baseline after set (user bubble / draft)
var baseline2 = baseline.union(Set(allStaticTexts()))

func findSendButton() -> AXUIElement? {
  bfsFirst(root, pred: { el, r in
    r == "AXButton" && (
      s(el, kAXDescriptionAttribute as String) == "Send" ||
      s(el, kAXTitleAttribute as String) == "Send" ||
      s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Send message")
    )
  })
}
func findStopButton() -> AXUIElement? {
  bfsFirst(root, pred: { el, r in
    r == "AXButton" && (
      s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Stop") ||
      s(el, kAXTitleAttribute as String).localizedCaseInsensitiveContains("Stop")
    )
  })
}
func messageLooksSent(preChars: Int, preValue: String) -> Bool {
  // Stop generating = model is answering → send landed.
  if findStopButton() != nil {
    log("send-check: Stop button present")
    return true
  }
  // Composer MUST be found. Missing composer used to return true (false positive)
  // and left drafts unsent while the wait-loop hung forever.
  guard let c = bfsFirst(root, pred: { el, r in
    r == "AXTextArea" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
  }) else {
    log("send-check: composer missing — not treating as sent")
    return false
  }
  let after = composerText(c).trimmingCharacters(in: .whitespacesAndNewlines)
  log("send-check: composer chars=\(after.count) pre=\(preChars)")
  // Still holds the audit draft → definitely not sent
  if promptLooksSet(after, prompt) { return false }
  if after.count >= max(40, preChars - 20) && preChars > 40 { return false }
  // Empty (or nearly empty) composer = submitted
  if after.isEmpty { return true }
  if after.count + 80 < preChars { return true }
  // User bubble appeared AND composer no longer has the draft
  let needle = String(prompt.prefix(60))
  let statics = allStaticTexts()
  if !needle.isEmpty && statics.contains(where: { $0.contains(String(needle.prefix(40))) }) {
    if after.count < max(20, preChars / 3) { return true }
  }
  _ = preValue
  return false
}

// --- Send with verification (AXPress success alone is NOT enough) ---
var sendVerified = false
let preSend = setValue
let preChars = preSend.count
for attempt in 1...5 {
  focusComposer(liveComposer)
  // Multi-line composers: bare Return inserts a newline — it does NOT send.
  // Prefer AX Send / mouse click / Cmd+Return only.
  if attempt == 1, let sendBtn = findSendButton() {
    let ok = press(sendBtn)
    log("send attempt=\(attempt) method=axPress ok=\(ok)")
  } else if attempt == 2 {
    key(36, flags: .maskCommand) // Cmd+Return
    log("send attempt=\(attempt) method=cmd-return")
  } else if attempt == 3, let sendBtn = findSendButton() {
    var pos: CFTypeRef?
    var size: CFTypeRef?
    if AXUIElementCopyAttributeValue(sendBtn, kAXPositionAttribute as CFString, &pos) == .success,
       AXUIElementCopyAttributeValue(sendBtn, kAXSizeAttribute as CFString, &size) == .success {
      var p = CGPoint.zero
      var sz = CGSize.zero
      if AXValueGetValue(pos as! AXValue, .cgPoint, &p),
         AXValueGetValue(size as! AXValue, .cgSize, &sz) {
        let c = CGPoint(x: p.x + sz.width / 2, y: p.y + sz.height / 2)
        let src = CGEventSource(stateID: .hidSystemState)
        CGEvent(mouseEventSource: src, mouseType: .mouseMoved, mouseCursorPosition: c, mouseButton: .left)?.post(tap: .cghidEventTap)
        CGEvent(mouseEventSource: src, mouseType: .leftMouseDown, mouseCursorPosition: c, mouseButton: .left)?.post(tap: .cghidEventTap)
        CGEvent(mouseEventSource: src, mouseType: .leftMouseUp, mouseCursorPosition: c, mouseButton: .left)?.post(tap: .cghidEventTap)
        log("send attempt=\(attempt) method=mouseClick at=\(Int(c.x)),\(Int(c.y))")
      } else {
        _ = press(sendBtn)
        log("send attempt=\(attempt) method=axPress-fallback")
      }
    } else {
      _ = press(sendBtn)
      log("send attempt=\(attempt) method=axPress-fallback")
    }
  } else if attempt == 4, let sendBtn = findSendButton() {
    _ = press(sendBtn)
    Thread.sleep(forTimeInterval: 0.2)
    key(36, flags: .maskCommand)
    log("send attempt=\(attempt) method=axPress+cmd-return")
  } else {
    key(36, flags: .maskCommand)
    log("send attempt=\(attempt) method=cmd-return-last")
  }
  Thread.sleep(forTimeInterval: 1.2)
  if messageLooksSent(preChars: preChars, preValue: preSend) {
    // Settle: Electron can briefly clear then restore draft; re-check once.
    Thread.sleep(forTimeInterval: 0.6)
    if messageLooksSent(preChars: preChars, preValue: preSend) {
      sendVerified = true
      log("send VERIFIED on attempt=\(attempt) (double-check ok)")
      break
    }
    log("send attempt=\(attempt) flapped — draft returned after brief clear")
  }
  // Draft may have been wiped by a failed partial send — re-set prompt
  if let c = bfsFirst(root, pred: { el, r in
    r == "AXTextArea" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
  }) {
    liveComposer = c
    let cur = composerText(c)
    if !promptLooksSet(cur, prompt) {
      log("send retry: re-setting prompt (composer chars=\(cur.count))")
      // Re-check attachment still present
      var stillAttached = false
      for el in bfsAll(root, pred: { _, _ in true }) {
        let blob = [s(el, kAXTitleAttribute as String), s(el, kAXDescriptionAttribute as String), s(el, kAXValueAttribute as String)].joined(separator: " ").lowercased()
        if blob.contains(zipName) || (blob.contains(".zip") && blob.count < 120) { stillAttached = true; break }
      }
      if !stillAttached {
        log("send retry: re-pasting zip attachment")
        pb.clearContents()
        _ = pb.writeObjects([zipURL as NSURL])
        focusComposer(liveComposer)
        key(9, flags: .maskCommand)
        Thread.sleep(forTimeInterval: 1.5)
      }
      _ = setComposerPrompt(liveComposer, prompt)
    }
  }
  Thread.sleep(forTimeInterval: 0.4)
}
if !sendVerified {
  let stuck = bfsFirst(root, pred: { el, r in
    r == "AXTextArea" && s(el, kAXDescriptionAttribute as String).localizedCaseInsensitiveContains("Message ChatGPT")
  }).map { composerText($0) } ?? ""
  emit([
    "ok": false,
    "code": "SEND_FAILED",
    "message": "Composer still holds the draft after send attempts — message was NOT submitted to ChatGPT.",
    "composerChars": stuck.count,
    "composerHead": String(stuck.prefix(120)),
    "attached": attached,
    "wakeHoldPid": wakePid as Any,
  ], exitCode: 9)
}
log("send confirmed; entering wait-loop")
Thread.sleep(forTimeInterval: 0.8)
baseline2 = baseline2.union(Set(allStaticTexts().filter { prompt.contains($0) || $0.count < 40 && prompt.contains($0.prefix(20)) }))

// Optional exact token from prompt (PSST_… / OK_…)
let exactToken: String? = {
  if let r = prompt.range(of: #"\b((?:PSST|OK)_[A-Z0-9_]+)\b"#, options: .regularExpression) {
    return String(prompt[r])
  }
  return nil
}()

// MARK: - Generation wait state machine (robust finish detection)
//
// Design (signal-driven, NOT wall-clock "force done"):
//
//   awaitingStart ──▶ active ──▶ settling ──▶ complete
//         │             │            │
//         │             │            └──▶ captureFailed (generation ended, body never complete)
//         │             └── (hours of Pro thinking stay in active; no stagnation exit)
//         └── must see this turn start before any finish (no prior-reply false complete)
//
// UI signals (ChatGPT Chat composer):
//   • Stop without Send  ⇒ generation active (thinking or streaming)
//   • loading chrome     ⇒ generation active
//   • Send without Stop  ⇒ generation ended (composer ready for next message)
//   • Stop + Send both   ⇒ sticky Stop after end; treat as ended if body complete
//
// Finish requires: phase==settling, body not incomplete, content fingerprint stable
// for settleConfirmSnapshots consecutive deep harvests. Never use "N seconds quiet"
// while Stop is active without Send.

func hasLoadingChrome(_ text: String) -> Bool {
  let l = text.lowercased()
  let markers = [
    "chatgpt is responding", "systems are thinking", "thinking a bit more",
    "for a quicker response", "before responding", "may be less capable",
  ]
  return markers.contains(where: { l.contains($0) })
}

/// Snapshot of Chat composer control surface (primary generation signals).
struct ComposerControls: Equatable {
  let stop: Bool
  let send: Bool
  let loadingChrome: Bool

  /// Model is thinking or streaming. Pro may stay here for hours with zero body growth.
  var isGenerationActive: Bool {
    if loadingChrome { return true }
    // Canonical ChatGPT UX: Stop replaces Send while answering.
    if stop && !send { return true }
    return false
  }

  /// Strong evidence the turn ended (user can send again).
  var isGenerationEnded: Bool {
    if loadingChrome { return false }
    if isGenerationActive { return false }
    // Send ready, or Stop gone (Send may lag one frame), or sticky Stop with Send.
    if send { return true }
    if !stop { return true }
    return false
  }
}

/// Lifecycle of one post-send wait. Pure classification for tests + wait loop.
enum GenerationPhase: String {
  case awaitingStart   // after send; no Stop / growth for this turn yet
  case active          // thinking or streaming (unbounded duration)
  case settling        // UI says ended; harvesting until body fingerprint stable
  case complete        // body complete + settled
  case captureFailed   // UI ended but body never became a valid audit capture
}

struct GenerationSample {
  let controls: ComposerControls
  let body: String
  let sawThisTurn: Bool  // Stop/active or body growth observed after this send
  let bodyFingerprint: String
  let consecutiveStableSnapshots: Int  // identical fingerprint while settling
  let consecutiveEndedObservations: Int // consecutive isGenerationEnded samples
}

func bodyFingerprint(_ text: String) -> String {
  // Length + ends: cheap, stable across whitespace-equivalent harvests of same content.
  let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
  let head = String(t.prefix(64))
  let tail = String(t.suffix(64))
  return "\(t.count)|\(head)|\(tail)"
}

/// Whether captured text is acceptable as a finished delivery (audit body or exact token).
func bodyAcceptableForFinish(_ text: String, exactToken: String?) -> Bool {
  if hasLoadingChrome(text) { return false }
  if let token = exactToken {
    let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if t == token || (t.contains(token) && t.count <= token.count + 80) {
      return true
    }
  }
  return !isIncompleteZipReply(text)
}

/// Pure phase transition. Signal-driven — no wall-clock "force done" while active.
func classifyGenerationPhase(_ s: GenerationSample, exactToken: String? = nil) -> GenerationPhase {
  if s.controls.isGenerationActive {
    return .active
  }
  if !s.sawThisTurn {
    return .awaitingStart
  }
  // This turn ran; UI no longer shows active generation.
  guard s.controls.isGenerationEnded else {
    // Ambiguous (e.g. neither Stop nor Send): stay active-safe if we already saw the turn.
    return .active
  }
  let acceptable = bodyAcceptableForFinish(s.body, exactToken: exactToken)
  // Need a few consecutive "ended" observations so one AX flicker does not finish early.
  if s.consecutiveEndedObservations < 2 {
    return .settling
  }
  if acceptable && s.consecutiveStableSnapshots >= 3 {
    return .complete
  }
  // Generation ended but capture never became a full audit — only after long settle.
  // Thresholds are consecutive *observations while ended*, not wall-clock while thinking.
  if !acceptable && s.consecutiveEndedObservations >= 40 && s.consecutiveStableSnapshots >= 20 {
    return .captureFailed
  }
  return .settling
}

func pollInterval(for phase: GenerationPhase, secondsInPhase: TimeInterval) -> TimeInterval {
  switch phase {
  case .awaitingStart, .settling, .complete, .captureFailed:
    return 2.0
  case .active:
    // Back off while Pro thinks for hours (reduces AX thrash; does not decide finish).
    if secondsInPhase < 60 { return 2.5 }
    if secondsInPhase < 300 { return 5.0 }
    if secondsInPhase < 1800 { return 8.0 }
    return 12.0
  }
}

func readComposerControls(bodyHint: String) -> ComposerControls {
  ComposerControls(
    stop: findStopButton() != nil,
    send: findSendButton() != nil,
    loadingChrome: hasLoadingChrome(bodyHint)
  )
}

/// Emit complete result. Only call when phase classification already says complete.
func emitComplete(_ text: String, finishNote: String) -> Never {
  pb.clearContents()
  if let oldString { pb.setString(oldString, forType: .string) }
  let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
  try? text.write(toFile: respPath, atomically: true, encoding: .utf8)
  _ = stageResultForDs([
    "ok": true,
    "status": "complete",
    "finalDeliveryText": text,
    "attached": true,
    "zipPath": zipPath as Any,
    "finishNote": finishNote,
  ], responseText: text)
  emit([
    "ok": true,
    "status": "complete",
    "mode": "chat",
    "workOn": false,
    "attached": true,
    "attachLabels": Array(labels.prefix(5)),
    "zipPath": zipPath as Any,
    "responsePath": respPath,
    "finalDeliveryText": text,
    "mustReturnFinalDelivery": true,
    "mustReturnVerbatim": true,
    "method": "clipboard-file-paste",
    "zipBytes": (try? FileManager.default.attributesOfItem(atPath: zipPath)[.size] as? NSNumber)?.intValue as Any,
    "wakeHoldPid": wakePid as Any,
    "responseChars": text.count,
    "finishNote": finishNote,
  ])
}

func emitStablePartial(_ text: String, code: String, message: String, elapsed: Int) -> Never {
  pb.clearContents()
  if let oldString { pb.setString(oldString, forType: .string) }
  let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
  try? text.write(toFile: respPath, atomically: true, encoding: .utf8)
  log("emitStablePartial code=\(code) chars=\(text.count) elapsed=\(elapsed)s")
  _ = stageResultForDs([
    "ok": false,
    "status": "partial",
    "code": code,
    "partial": text,
    "partialChars": text.count,
    "attached": true,
    "elapsedSec": elapsed,
  ], responseText: text)
  emit([
    "ok": false,
    "code": code,
    "status": "partial",
    "message": message,
    "mode": "chat",
    "workOn": false,
    "attached": true,
    "attachLabels": Array(labels.prefix(5)),
    "zipPath": zipPath as Any,
    "responsePath": respPath,
    "partial": text,
    "partialChars": text.count,
    "finalDeliveryText": text,
    "mustReturnFinalDelivery": true,
    "mustReturnVerbatim": true,
    "method": "clipboard-file-paste",
    "zipBytes": (try? FileManager.default.attributesOfItem(atPath: zipPath)[.size] as? NSNumber)?.intValue as Any,
    "wakeHoldPid": wakePid as Any,
    "elapsedSec": elapsed,
    "responseChars": text.count,
  ], exitCode: 1)
}

/// Deterministic policy selfcheck (no ChatGPT). Invoked via --selfcheck-generation-policy.
func runSelfcheckGenerationPolicy() -> Never {
  struct Case {
    let name: String
    let sample: GenerationSample
    let expect: GenerationPhase
  }
  let auditBody = """
  ## Executive assessment
  The monorepo shows modular crates and solid tests. Top risk: sandbox fail-open
  on unsupported platforms returns a permissive path without hard failure.
  1. Severity high: AlwaysApprove defaults expand privilege.
  2. Recommendation: fail closed when sandbox cannot apply.
  Architecture notes: permission resolver, network hooks, plugin install.
  """
  let chrome = "ChatGPT is responding\nOur systems are thinking a bit more"
  let cases: [Case] = [
    Case(
      name: "awaiting_start",
      sample: GenerationSample(
        controls: ComposerControls(stop: false, send: true, loadingChrome: false),
        body: "", sawThisTurn: false, bodyFingerprint: bodyFingerprint(""),
        consecutiveStableSnapshots: 0, consecutiveEndedObservations: 5
      ),
      expect: .awaitingStart
    ),
    Case(
      name: "active_stop_no_send",
      sample: GenerationSample(
        controls: ComposerControls(stop: true, send: false, loadingChrome: false),
        body: "", sawThisTurn: true, bodyFingerprint: bodyFingerprint(""),
        consecutiveStableSnapshots: 100, consecutiveEndedObservations: 100
      ),
      expect: .active
    ),
    Case(
      name: "active_loading_chrome",
      sample: GenerationSample(
        controls: ComposerControls(stop: false, send: true, loadingChrome: true),
        body: chrome, sawThisTurn: true, bodyFingerprint: bodyFingerprint(chrome),
        consecutiveStableSnapshots: 50, consecutiveEndedObservations: 50
      ),
      expect: .active
    ),
    Case(
      name: "active_hours_no_growth_still_stop",
      sample: GenerationSample(
        controls: ComposerControls(stop: true, send: false, loadingChrome: false),
        body: "outline chip", sawThisTurn: true, bodyFingerprint: "x",
        consecutiveStableSnapshots: 999, consecutiveEndedObservations: 999
      ),
      expect: .active
    ),
    Case(
      name: "settling_after_end",
      sample: GenerationSample(
        controls: ComposerControls(stop: false, send: true, loadingChrome: false),
        body: auditBody, sawThisTurn: true, bodyFingerprint: bodyFingerprint(auditBody),
        consecutiveStableSnapshots: 1, consecutiveEndedObservations: 2
      ),
      expect: .settling
    ),
    Case(
      name: "complete_settled",
      sample: GenerationSample(
        controls: ComposerControls(stop: false, send: true, loadingChrome: false),
        body: auditBody, sawThisTurn: true, bodyFingerprint: bodyFingerprint(auditBody),
        consecutiveStableSnapshots: 3, consecutiveEndedObservations: 5
      ),
      expect: .complete
    ),
    Case(
      name: "complete_sticky_stop_send_ready",
      sample: GenerationSample(
        controls: ComposerControls(stop: true, send: true, loadingChrome: false),
        body: auditBody, sawThisTurn: true, bodyFingerprint: bodyFingerprint(auditBody),
        consecutiveStableSnapshots: 3, consecutiveEndedObservations: 5
      ),
      expect: .complete
    ),
    Case(
      name: "capture_failed_after_end",
      sample: GenerationSample(
        controls: ComposerControls(stop: false, send: true, loadingChrome: false),
        body: "No sources yet", sawThisTurn: true, bodyFingerprint: bodyFingerprint("No sources yet"),
        consecutiveStableSnapshots: 25, consecutiveEndedObservations: 45
      ),
      expect: .captureFailed
    ),
  ]
  var results: [[String: Any]] = []
  var failed = 0
  for c in cases {
    let got = classifyGenerationPhase(c.sample, exactToken: nil)
    let pass = got == c.expect
    if !pass { failed += 1 }
    results.append([
      "name": c.name,
      "expect": c.expect.rawValue,
      "got": got.rawValue,
      "pass": pass,
    ])
  }
  let ok = failed == 0
  let out: [String: Any] = [
    "ok": ok,
    "status": "selfcheck-generation-policy",
    "failed": failed,
    "cases": results,
    "policy": "signal-driven-state-machine",
  ]
  if let d = try? JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys]),
     let str = String(data: d, encoding: .utf8) {
    print(str)
  }
  exit(ok ? 0 : 1)
}

// --- Wait-loop state ---
var best = ""
var sawGrowth = false
var sawThisTurn = false  // must observe active generation or growth after this send
var tick = 0
var lastCopyAt = Date.distantPast
var lastSignificantGrowthAt = Date()
var phase: GenerationPhase = .awaitingStart
var phaseEnteredAt = Date()
var lastFingerprint = ""
var consecutiveStableSnapshots = 0
var consecutiveEndedObservations = 0
// Accumulate every novel StaticText ever seen (streaming chips replace each other in AX).
var accumulatedParts: [String] = []
var accumulatedSet = Set<String>()
let waitStarted = Date()
// --timeout 0 = unlimited (wait until phase complete / captureFailed / kill / lock).
// --timeout N = optional user wall-clock only. No invented 60m hard cap.
let deadline: Date? = timeoutSec > 0 ? Date().addingTimeInterval(timeoutSec) : nil
log(
  "wait-loop start timeoutSec=\(timeoutSec) " +
  "deadline=\(deadline.map { "\($0.timeIntervalSince(waitStarted))s" } ?? "none(unlimited)") " +
  "wakeHoldPid=\(wakePid.map(String.init) ?? "none") " +
  "policy=generation-state-machine"
)

func ingestCaptureTexts(_ texts: [String]) -> (assistant: String, novel: Int, newParts: Int) {
  let novel = texts.filter { !baseline2.contains($0) && !prompt.contains($0) }
  let filtered = novel.filter { t in
    let low = t.lowercased()
    if t.count < 2 { return false }
    if low.hasPrefix("audit only") { return false }
    if low == "audit rust monorepo" || low == "audit request rust repo" { return false }
    // Keep outline chips ("dependencies,", "H-1.") — ChatGPT streams these as short StaticTexts.
    if t.count < 12 && !t.contains(" ") {
      let outlineish =
        t.hasSuffix(",") || t.hasSuffix(":") || t.hasSuffix(".") || t.hasSuffix(";") ||
        t.hasSuffix("-") || t.contains("/") ||
        t.range(of: #"^\d+[\.\)]"#, options: .regularExpression) != nil ||
        t.range(of: #"^[A-Z]-\d+"#, options: .regularExpression) != nil
      if !outlineish { return false }
    }
    return true
  }
  var newParts = 0
  for t in filtered {
    if accumulatedSet.contains(t) { continue }
    if accumulatedParts.contains(where: { $0.contains(t) && $0.count > t.count + 10 }) { continue }
    if let idx = accumulatedParts.firstIndex(where: { t.contains($0) && t.count > $0.count + 10 }) {
      accumulatedSet.remove(accumulatedParts[idx])
      accumulatedParts[idx] = t
      accumulatedSet.insert(t)
      newParts += 1
      continue
    }
    accumulatedSet.insert(t)
    accumulatedParts.append(t)
    newParts += 1
  }
  let live = filtered.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
  let accumulated = accumulatedParts.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
  let assistant = accumulated.count >= live.count ? accumulated : live
  return (assistant, novel.count, newParts)
}

func absorbBody(_ text: String, minDelta: Int = 20) {
  guard text.count > best.count + minDelta else {
    if text.count > best.count { best = text }
    return
  }
  if !text.isEmpty && !accumulatedSet.contains(text) {
    accumulatedSet.insert(text)
    accumulatedParts.append(text)
  }
  if text.count >= best.count + 100 {
    lastSignificantGrowthAt = Date()
  }
  best = text
  sawGrowth = true
  sawThisTurn = true
  // Growth invalidates settle fingerprint stability.
  consecutiveStableSnapshots = 0
}

while true {
  if let deadline, Date() >= deadline { break }

  let secondsInPhase = Date().timeIntervalSince(phaseEnteredAt)
  Thread.sleep(forTimeInterval: pollInterval(for: phase, secondsInPhase: secondsInPhase))
  tick += 1
  _ = WakeHold.shared.ensureAlive()
  let elapsed = Int(Date().timeIntervalSince(waitStarted))

  if isScreenLocked() {
    emit([
      "ok": false,
      "code": "PSST_GPT_SCREEN_LOCKED",
      "message": "Screen locked during wait. Unlock and re-run; attachment may still be in ChatGPT.",
      "partial": best,
      "partialChars": best.count,
      "attached": attached,
      "elapsedSec": elapsed,
      "phase": phase.rawValue,
      "wakeHoldPid": wakePid as Any,
    ], exitCode: 30)
  }
  if let work = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work" }),
     s(work, kAXValueAttribute as String) == "1" {
    emit(["ok": false, "code": "WORK_FLIP", "attached": attached, "phase": phase.rawValue, "wakeHoldPid": wakePid as Any], exitCode: 20)
  }

  // Scroll so long streams stay in AX tree.
  if tick % 3 == 0 { scrollTranscript() }

  var controls = readComposerControls(bodyHint: best)
  if controls.isGenerationActive { sawThisTurn = true }

  // Harvest policy by phase: aggressive when settling; light while active (stream still in AX).
  let sinceCopy = Date().timeIntervalSince(lastCopyAt)
  let shouldCopy: Bool = {
    switch phase {
    case .settling, .complete, .captureFailed:
      return true
    case .awaitingStart:
      return sinceCopy >= 4
    case .active:
      // Occasional copy while streaming; never used as a finish signal alone.
      return sinceCopy >= 15 || (best.count > 500 && sinceCopy >= 8)
    }
  }()
  if shouldCopy { lastCopyAt = Date() }
  let deep = deepHarvestReply(includeCopy: shouldCopy)
  absorbBody(deep, minDelta: 20)

  let texts = allCaptureTexts()
  if let token = exactToken {
    let hits = texts.filter {
      $0 == token || ($0.contains(token) && !$0.lowercased().contains("reply with") && $0.count <= token.count + 40)
    }
    if let a = hits.first(where: { $0 == token }) ?? hits.first {
      absorbBody(a, minDelta: 0)
      sawThisTurn = true
    }
  }
  let ing = ingestCaptureTexts(texts)
  absorbBody(ing.assistant, minDelta: 5)

  // Re-read controls after harvest (chrome may appear in body).
  controls = readComposerControls(bodyHint: best)
  if controls.isGenerationActive { sawThisTurn = true }

  let fp = bodyFingerprint(best)
  if fp == lastFingerprint && !best.isEmpty {
    consecutiveStableSnapshots += 1
  } else {
    consecutiveStableSnapshots = best.isEmpty ? 0 : 1
    lastFingerprint = fp
  }
  if controls.isGenerationEnded {
    consecutiveEndedObservations += 1
  } else {
    consecutiveEndedObservations = 0
  }

  let sample = GenerationSample(
    controls: controls,
    body: best,
    sawThisTurn: sawThisTurn,
    bodyFingerprint: fp,
    consecutiveStableSnapshots: consecutiveStableSnapshots,
    consecutiveEndedObservations: consecutiveEndedObservations
  )
  let newPhase = classifyGenerationPhase(sample, exactToken: exactToken)
  if newPhase != phase {
    log("phase \(phase.rawValue) → \(newPhase.rawValue) elapsed=\(elapsed)s best=\(best.count) stop=\(controls.stop) send=\(controls.send)")
    phase = newPhase
    phaseEnteredAt = Date()
  }

  log(
    "tick=\(tick) elapsed=\(elapsed)s phase=\(phase.rawValue) " +
    "chars=\(best.count) stableSnap=\(consecutiveStableSnapshots) endedObs=\(consecutiveEndedObservations) " +
    "stop=\(controls.stop) send=\(controls.send) chrome=\(controls.loadingChrome) " +
    "sawTurn=\(sawThisTurn) novel=\(ing.novel) newParts=\(ing.newParts)"
  )

  if tick % 3 == 0 {
    _ = stageResultForDs([
      "ok": false,
      "status": "waiting",
      "phase": phase.rawValue,
      "partialChars": best.count,
      "elapsedSec": elapsed,
      "attached": attached,
      "stopPresent": controls.stop,
      "sendPresent": controls.send,
      "generationActive": controls.isGenerationActive,
    ], responseText: best.isEmpty ? nil : best)
  }

  switch phase {
  case .awaitingStart, .active:
    // Unbounded wait. Pro thinking for hours stays in .active.
    if phase == .active, elapsed > 0, elapsed % 300 < 15 {
      log("active generation after \(elapsed)s best=\(best.count) — keep waiting (no force finish)")
    }
    continue

  case .settling:
    // Deep harvest every settle tick until complete or captureFailed.
    let h = deepHarvestReply(includeCopy: true)
    absorbBody(h, minDelta: 10)
    continue

  case .complete:
    let note: String
    if controls.stop && controls.send {
      note = "settled-sticky-stop-send-ready"
    } else if exactToken != nil {
      note = "settled-exact-token"
    } else {
      note = "settled-generation-ended"
    }
    // Final safety: refuse incomplete (classifier should already enforce).
    if !bodyAcceptableForFinish(best, exactToken: exactToken) {
      log("complete phase but body not acceptable — revert to settling")
      phase = .settling
      phaseEnteredAt = Date()
      consecutiveStableSnapshots = 0
      continue
    }
    emitComplete(best, finishNote: note)

  case .captureFailed:
    let h = deepHarvestReply(includeCopy: true)
    absorbBody(h, minDelta: 10)
    // One more classification pass in case harvest fixed it.
    let retry = GenerationSample(
      controls: readComposerControls(bodyHint: best),
      body: best,
      sawThisTurn: sawThisTurn,
      bodyFingerprint: bodyFingerprint(best),
      consecutiveStableSnapshots: max(consecutiveStableSnapshots, 3),
      consecutiveEndedObservations: max(consecutiveEndedObservations, 5)
    )
    if classifyGenerationPhase(retry, exactToken: exactToken) == .complete {
      emitComplete(best, finishNote: "settled-after-capture-retry")
    }
    emitStablePartial(
      best,
      code: "AX_CAPTURE_STALL",
      message: "Generation ended (Send ready / Stop cleared) but captured body never passed complete-audit checks (chars=\(best.count)). Returning best capture after Copy-message harvest.",
      elapsed: elapsed
    )
  }
}

// Only reached when user set --timeout N and wall clock expired.
let elapsedFinal = Int(Date().timeIntervalSince(waitStarted))
log("user timeout reached elapsed=\(elapsedFinal)s best=\(best.count) phase=\(phase.rawValue) timeoutSec=\(timeoutSec)")
let finalControls = readComposerControls(bodyHint: best)
if !finalControls.isGenerationActive {
  let h = deepHarvestReply(includeCopy: true)
  absorbBody(h, minDelta: 10)
  if bodyAcceptableForFinish(best, exactToken: exactToken) && sawThisTurn {
    emitComplete(best, finishNote: "user-timeout-settled-complete")
  }
}
if best.count >= 40 {
  emitStablePartial(
    best,
    code: "TIMEOUT",
    message: "User --timeout \(Int(timeoutSec))s reached (phase=\(phase.rawValue)). Returning best capture (chars=\(best.count)). Use --timeout 0 to wait until generation ends (hours OK).",
    elapsed: elapsedFinal
  )
}
pb.clearContents()
if let oldString { pb.setString(oldString, forType: .string) }
emit([
  "ok": false,
  "code": "TIMEOUT",
  "message": "User --timeout \(Int(timeoutSec))s reached with empty/minimal capture. Use --timeout 0 for unlimited wait.",
  "attached": attached,
  "partial": best,
  "partialChars": best.count,
  "elapsedSec": elapsedFinal,
  "phase": phase.rawValue,
  "wakeHoldPid": wakePid as Any,
], exitCode: 1)
