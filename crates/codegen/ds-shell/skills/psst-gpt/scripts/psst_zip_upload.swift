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

/// Process-scoped wake hold. Ends when we exit (`-w <self>`).
///
/// Flags (see `man caffeinate`):
/// - `-d` display sleep  -i idle sleep  -m disk  -s system sleep (AC)
/// - `-u -t <sec>` declare user active for the hold (resets idle/lock timer;
///   bare `-u` only lasts 5s — that was not enough to stop screen lock)
final class WakeHold {
  static let shared = WakeHold()
  private var process: Process?
  private(set) var caffeinatePid: Int32?
  /// How long the user-active assertion may last (helper usually exits sooner).
  private static let userActiveHoldSec = 24 * 60 * 60

  @discardableResult
  func start() -> Int32? {
    #if os(macOS)
    if process?.isRunning == true { return caffeinatePid }
    let path = "/usr/bin/caffeinate"
    guard FileManager.default.isExecutableFile(atPath: path) else {
      log("wake-hold: caffeinate missing; continuing without hold")
      return nil
    }
    let selfPid = ProcessInfo.processInfo.processIdentifier
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    // -u without -t only asserts user-active for 5 seconds (man caffeinate).
    p.arguments = [
      "-dimsu",
      "-t", "\(Self.userActiveHoldSec)",
      "-w", "\(selfPid)",
    ]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
      try p.run()
      process = p
      caffeinatePid = p.processIdentifier
      log("wake-hold: started caffeinate pid=\(p.processIdentifier) for self=\(selfPid) args=-dimsu -t \(Self.userActiveHoldSec) -w \(selfPid)")
      // Verify the child is actually alive (catches silent launch failures).
      Thread.sleep(forTimeInterval: 0.15)
      if !p.isRunning {
        log("wake-hold: WARNING caffeinate exited immediately — screen may lock")
        process = nil
        caffeinatePid = nil
        return nil
      }
      return caffeinatePid
    } catch {
      log("wake-hold: failed to start: \(error)")
      return nil
    }
    #else
    return nil
    #endif
  }

  func stop() {
    guard let p = process else {
      caffeinatePid = nil
      return
    }
    if p.isRunning {
      p.terminate()
      let deadline = Date().addingTimeInterval(1.0)
      while p.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
      if p.isRunning { kill(p.processIdentifier, SIGKILL) }
    }
    log("wake-hold: stopped caffeinate pid=\(caffeinatePid.map(String.init) ?? "?")")
    process = nil
    caffeinatePid = nil
  }

  deinit {
    if let p = process, p.isRunning { p.terminate() }
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

/// Complete only when body is a real reply (not zip-loading chrome). Returns false to keep waiting.
@discardableResult
func finishIfReady(_ text: String) -> Bool {
  if isIncompleteZipReply(text) {
    log("finishIfReady: still loading/incomplete chars=\(text.count)")
    return false
  }
  // Still generating → never finalize (Stop visible means stream not done).
  if findStopButton() != nil {
    log("finishIfReady: Stop still present — waiting for generation to finish")
    return false
  }
  // Zip audits: require a substantive body; short tokens still OK via exactToken path.
  if exactToken == nil && text.count < 300 {
    log("finishIfReady: body too short for zip audit chars=\(text.count)")
    return false
  }
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
  ])
  // emit is Never; keep compiler happy
  return true
}

/// Never hang forever: emit whatever AX captured after a long stall (honest partial).
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

var stable = 0
var lastSig = ""
var best = ""
var sawGrowth = false
var tick = 0
var stopGoneTicks = 0
var lastCopyAttemptTick = -100
var lastSignificantGrowthAt = Date()
// Accumulate every novel StaticText ever seen (streaming chips replace each other in AX).
var accumulatedParts: [String] = []
var accumulatedSet = Set<String>()
let waitStarted = Date()
// --timeout 0 means "long audit", not "hang forever": hard cap 60 min for full zip audits.
let hardCapSec: Double = 60 * 60
let effectiveTimeoutSec: Double = timeoutSec > 0 ? timeoutSec : hardCapSec
let deadline = Date().addingTimeInterval(effectiveTimeoutSec)
// After Stop gone: allow deep harvest for ~45s then require complete or partial.
let stopGoneHarvestTicks = 6
// Stable after full harvest attempts (~2.5 min at 2.5s) before stall partial.
let stallTicksStopGone = 60
let stallTicksNoGrowth = 150
// Wall-clock: no +100 char growth for this long → force harvest / exit (Stop can stick true).
let noGrowthForceHarvestSec: Double = 90
let noGrowthForceExitSec: Double = 180
log("wait-loop start timeoutSec=\(timeoutSec) effectiveTimeoutSec=\(effectiveTimeoutSec) hardCap=\(timeoutSec <= 0) wakeHoldPid=\(wakePid.map(String.init) ?? "none")")

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

while Date() < deadline {
  Thread.sleep(forTimeInterval: 2.5)
  tick += 1
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
      "wakeHoldPid": wakePid as Any,
    ], exitCode: 30)
  }
  if let work = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work" }),
     s(work, kAXValueAttribute as String) == "1" {
    emit(["ok": false, "code": "WORK_FLIP", "attached": attached, "wakeHoldPid": wakePid as Any], exitCode: 20)
  }

  let stopPresent = findStopButton() != nil
  if stopPresent { stopGoneTicks = 0 } else { stopGoneTicks += 1 }

  // Periodically scroll while streaming so long replies stay in the AX tree.
  if tick % 3 == 0 { scrollTranscript() }

  // Copy harvest: always when Stop gone; also when body is large & frozen (Stop can stick true).
  let stagnantSec = Date().timeIntervalSince(lastSignificantGrowthAt)
  let forceCopyDespiteStop = stopPresent && best.count >= 800 && stagnantSec >= noGrowthForceHarvestSec
  let shouldCopy = !stopPresent || forceCopyDespiteStop || (tick - lastCopyAttemptTick >= 10 && best.count > 500)
  if shouldCopy { lastCopyAttemptTick = tick }
  let deep = deepHarvestReply(includeCopy: shouldCopy)
  if deep.count > best.count + 20 {
    log("deepHarvest grew best \(best.count) -> \(deep.count) (copy=\(shouldCopy) forceStop=\(forceCopyDespiteStop))")
    // Feed clipboard body into accumulation as one blob
    if !deep.isEmpty && !accumulatedSet.contains(deep) {
      accumulatedSet.insert(deep)
      accumulatedParts.append(deep)
    }
    if deep.count >= best.count + 100 {
      lastSignificantGrowthAt = Date()
    }
    best = deep
    sawGrowth = true
    stable = 0
  }

  let texts = allCaptureTexts()
  if let token = exactToken {
    let hits = texts.filter {
      $0 == token || ($0.contains(token) && !$0.lowercased().contains("reply with") && $0.count <= token.count + 40)
    }
    log("tick=\(tick) elapsed=\(elapsed)s tokenHits=\(hits.count) bestChars=\(best.count) stop=\(stopPresent)")
    if let a = hits.first(where: { $0 == token }) ?? hits.first {
      if a == best { stable += 1 } else { stable = 0; best = a }
      if stable >= 2 { _ = finishIfReady(best) }
      continue
    }
  }

  let ing = ingestCaptureTexts(texts)
  var assistant = ing.assistant
  if deep.count > assistant.count { assistant = deep }
  if best.count > assistant.count { assistant = best }

  log("tick=\(tick) elapsed=\(elapsed)s novel=\(ing.novel) kept newParts=\(ing.newParts) chars=\(assistant.count) best=\(best.count) stable=\(stable) stop=\(stopPresent) stopGone=\(stopGoneTicks) sawGrowth=\(sawGrowth)")

  if tick % 4 == 0, best.count > 0 {
    _ = stageResultForDs([
      "ok": false,
      "status": "waiting",
      "partialChars": best.count,
      "elapsedSec": elapsed,
      "attached": attached,
      "stopPresent": stopPresent,
    ], responseText: best)
  }

  let lower = assistant.lowercased()
  func hasWB(_ word: String) -> Bool {
    lower.range(of: "\\b\(NSRegularExpression.escapedPattern(for: word))\\b", options: .regularExpression) != nil
  }
  let looksComplete = assistant.count >= 300 && (
    hasWB("work mode") || lower.contains("continue with work") ||
    hasWB("cannot") || lower.contains("can't open") ||
    lower.contains("unable to") || lower.contains("out of") ||
    hasWB("risk") || hasWB("risks") || hasWB("finding") || hasWB("findings") ||
    assistant.contains("##") || hasWB("severity") || hasWB("recommend") || hasWB("recommendation") ||
    lower.contains("executive") || lower.contains("architecture")
  )

  if assistant.count > best.count + 5 {
    if assistant.count >= best.count + 100 {
      lastSignificantGrowthAt = Date()
    }
    sawGrowth = true
    best = assistant
    stable = 0
    lastSig = "\(assistant.count)"
  } else {
    let sig = "\(assistant.count)"
    if sig == lastSig && assistant.count > 20 {
      stable += 1
      if assistant.count > best.count { best = assistant }
    } else if assistant.count >= best.count && !assistant.isEmpty {
      best = assistant
      lastSig = sig
      stable = sawGrowth ? 1 : 0
    } else {
      if assistant.isEmpty || assistant.count + 80 < best.count { stable = 0 }
      lastSig = sig
    }
  }

  // Prefer finishing after Stop is gone + harvest ticks (full body via Copy).
  if !stopPresent && stopGoneTicks >= stopGoneHarvestTicks {
    if stopGoneTicks == stopGoneHarvestTicks || stopGoneTicks % 4 == 0 {
      let h = deepHarvestReply(includeCopy: true)
      if h.count > best.count {
        if h.count >= best.count + 100 { lastSignificantGrowthAt = Date() }
        best = h
        sawGrowth = true
        stable = 0
        log("post-stop harvest chars=\(h.count)")
      }
    }
    if looksComplete && stable >= 2 {
      _ = finishIfReady(best.count >= assistant.count ? best : assistant)
    }
    if sawGrowth && stable >= 3 && best.count >= 300 {
      _ = finishIfReady(best)
    }
    if stable >= 4 && best.count >= 300 {
      _ = finishIfReady(best)
    }
  }

  // Stop button can stick true after ChatGPT finished — don't wait forever.
  // After 90s no significant growth with a large body: harvest+try complete ignoring Stop.
  let stagnant = Date().timeIntervalSince(lastSignificantGrowthAt)
  if sawGrowth && best.count >= 800 && stagnant >= noGrowthForceHarvestSec {
    log("force path: stagnant \(Int(stagnant))s best=\(best.count) stop=\(stopPresent) — copy harvest")
    let h = deepHarvestReply(includeCopy: true)
    if h.count > best.count {
      if h.count >= best.count + 100 { lastSignificantGrowthAt = Date() }
      best = h
      stable = 0
      log("force harvest chars=\(h.count)")
    }
    // If still complete-looking and Stop may be false-positive, try finish.
    if best.count >= 1000 && !isIncompleteZipReply(best) {
      // Temporarily allow finish even if Stop still listed (sticky AX).
      if findStopButton() != nil {
        log("force finish attempt despite Stop (sticky AX) chars=\(best.count)")
      }
      // Bypass Stop check only after long stagnation + large body
      if stagnant >= noGrowthForceExitSec {
        pb.clearContents()
        if let oldString { pb.setString(oldString, forType: .string) }
        let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
        try? best.write(toFile: respPath, atomically: true, encoding: .utf8)
        if !isIncompleteZipReply(best) && best.count >= 300 {
          _ = stageResultForDs([
            "ok": true,
            "status": "complete",
            "finalDeliveryText": best,
            "attached": true,
            "zipPath": zipPath as Any,
            "note": "finished after Stop stuck + copy harvest",
          ], responseText: best)
          emit([
            "ok": true,
            "status": "complete",
            "mode": "chat",
            "workOn": false,
            "attached": true,
            "attachLabels": Array(labels.prefix(5)),
            "zipPath": zipPath as Any,
            "responsePath": respPath,
            "finalDeliveryText": best,
            "mustReturnFinalDelivery": true,
            "mustReturnVerbatim": true,
            "method": "clipboard-file-paste",
            "zipBytes": (try? FileManager.default.attributesOfItem(atPath: zipPath)[.size] as? NSNumber)?.intValue as Any,
            "wakeHoldPid": wakePid as Any,
            "responseChars": best.count,
            "finishNote": "stop-sticky-force",
          ])
        }
      }
    }
  }

  // --- Stall backstops: deep harvest first, then honest partial (never hang) ---
  if !stopPresent && sawGrowth && stable >= stallTicksStopGone && best.count >= 40 {
    log("stall backstop: Stop gone + stable=\(stable) — final deep harvest")
    let h = deepHarvestReply(includeCopy: true)
    if h.count > best.count { best = h }
    if finishIfReady(best) { /* Never */ }
    emitStablePartial(
      best,
      code: "AX_CAPTURE_STALL",
      message: "ChatGPT finished (Stop gone) but captured body did not pass complete-audit checks (chars=\(best.count)). Returning best capture after Copy-message harvest.",
      elapsed: elapsed
    )
  }
  if sawGrowth && (stable >= stallTicksNoGrowth || stagnant >= noGrowthForceExitSec + 60) && best.count >= 40 {
    log("stall backstop: no-growth stable=\(stable) stagnant=\(Int(stagnant))s stop=\(stopPresent)")
    let h = deepHarvestReply(includeCopy: true)
    if h.count > best.count { best = h }
    if finishIfReady(best) { /* Never */ }
    // Large frozen body after harvest: accept as complete if substantive
    if best.count >= 1000 && !isIncompleteZipReply(best) {
      pb.clearContents()
      if let oldString { pb.setString(oldString, forType: .string) }
      let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
      try? best.write(toFile: respPath, atomically: true, encoding: .utf8)
      _ = stageResultForDs([
        "ok": true,
        "status": "complete",
        "finalDeliveryText": best,
        "attached": true,
        "zipPath": zipPath as Any,
      ], responseText: best)
      emit([
        "ok": true,
        "status": "complete",
        "mode": "chat",
        "workOn": false,
        "attached": true,
        "attachLabels": Array(labels.prefix(5)),
        "zipPath": zipPath as Any,
        "responsePath": respPath,
        "finalDeliveryText": best,
        "mustReturnFinalDelivery": true,
        "mustReturnVerbatim": true,
        "method": "clipboard-file-paste",
        "zipBytes": (try? FileManager.default.attributesOfItem(atPath: zipPath)[.size] as? NSNumber)?.intValue as Any,
        "wakeHoldPid": wakePid as Any,
        "responseChars": best.count,
        "finishNote": "stagnant-large-body",
      ])
    }
    emitStablePartial(
      best,
      code: "AX_CAPTURE_STALL",
      message: "No significant body growth for \(Int(stagnant))s. Returning best capture (chars=\(best.count)).",
      elapsed: elapsed
    )
  }
  if tick >= 40 && best.count < 40 && !sawGrowth {
    log("still waiting: only chrome/title chips after \(elapsed)s best=\(best.count) stop=\(stopPresent)")
  }
}
// Hard deadline
let elapsedFinal = Int(Date().timeIntervalSince(waitStarted))
log("deadline reached elapsed=\(elapsedFinal)s best=\(best.count) — final harvest")
if findStopButton() == nil {
  let h = deepHarvestReply(includeCopy: true)
  if h.count > best.count { best = h }
  if finishIfReady(best) { /* Never */ }
}
if best.count >= 40 {
  emitStablePartial(
    best,
    code: "TIMEOUT",
    message: "Wait deadline reached (effectiveTimeoutSec=\(Int(effectiveTimeoutSec))). Returning best capture (chars=\(best.count)).",
    elapsed: elapsedFinal
  )
}
pb.clearContents()
if let oldString { pb.setString(oldString, forType: .string) }
emit([
  "ok": false,
  "code": "TIMEOUT",
  "attached": attached,
  "partial": best,
  "partialChars": best.count,
  "elapsedSec": elapsedFinal,
  "wakeHoldPid": wakePid as Any,
], exitCode: 1)
