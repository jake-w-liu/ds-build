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

/// Process-scoped wake hold: `caffeinate -dims -w <self>` ends when we exit.
final class WakeHold {
  static let shared = WakeHold()
  private var process: Process?
  private(set) var caffeinatePid: Int32?

  @discardableResult
  func start() -> Int32? {
    #if os(macOS)
    if process?.isRunning == true { return caffeinatePid }
    let path = "/usr/bin/caffeinate"
    guard FileManager.default.isExecutableFile(atPath: path) else {
      log("wake-hold: caffeinate missing; continuing without hold")
      return nil
    }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    p.arguments = ["-dims", "-w", "\(ProcessInfo.processInfo.processIdentifier)"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
      try p.run()
      process = p
      caffeinatePid = p.processIdentifier
      log("wake-hold: started caffeinate pid=\(p.processIdentifier) for self=\(ProcessInfo.processInfo.processIdentifier)")
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
    guard let p = process else { return }
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
  if a == "--" { promptParts.append(contentsOf: args[(i + 1)...]); break }
  if a.hasPrefix("-") { emit(["ok": false, "code": "BAD_ARGS", "message": "Unknown \(a)"], exitCode: 2) }
  promptParts.append(a); i += 1
}
let prompt = promptParts.joined(separator: " ").trimmingCharacters(in: .whitespacesAndNewlines)
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

_ = setAttr(composer, kAXValueAttribute as String, prompt as CFTypeRef)
Thread.sleep(forTimeInterval: 0.35)
if let send = bfsFirst(root, pred: { el, r in
  r == "AXButton" && (s(el, kAXDescriptionAttribute as String) == "Send" || s(el, kAXTitleAttribute as String) == "Send")
}) {
  _ = press(send)
} else {
  key(36)
}

// Capture response — filter attachment/chrome labels
func isChromeText(_ t: String) -> Bool {
  let l = t.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
  if l.isEmpty { return true }
  let chrome = [
    "new chat", "projects", "plugins", "sites", "scheduled", "message chatgpt",
    "recents", "search", "send", "add files and more", "select chatgpt model",
  ]
  if chrome.contains(l) { return true }
  if l.hasPrefix("pin chat") || l.hasPrefix("archive chat") || l.hasPrefix("remove ") { return true }
  if l == zipName || l.hasSuffix(".zip") && l.count < 80 { return true }
  if l.contains("source-archive") && l.count < 80 { return true }
  return false
}

// Optional exact token from prompt (PSST_… / OK_…)
let exactToken: String? = {
  if let r = prompt.range(of: #"\b((?:PSST|OK)_[A-Z0-9_]+)\b"#, options: .regularExpression) {
    return String(prompt[r])
  }
  return nil
}()

var stable = 0
var last = ""
var best = ""
let fingerprint = String(prompt.prefix(min(48, prompt.count)))
let deadline: Date? = timeoutSec > 0 ? Date().addingTimeInterval(timeoutSec) : nil
while deadline == nil || Date() < deadline! {
  Thread.sleep(forTimeInterval: 2)
  if isScreenLocked() {
    emit([
      "ok": false,
      "code": "PSST_GPT_SCREEN_LOCKED",
      "message": "Screen locked during wait. Unlock and re-run; attachment may still be in ChatGPT.",
      "partial": best,
    ], exitCode: 30)
  }
  if let work = bfsFirst(root, pred: { el, r in r.contains("Check") && s(el, kAXTitleAttribute as String) == "Work" }),
     s(work, kAXValueAttribute as String) == "1" {
    emit(["ok": false, "code": "WORK_FLIP"], exitCode: 20)
  }
  let texts = bfsAll(root, pred: { _, r in r == "AXStaticText" })
    .map { s($0, kAXValueAttribute as String).trimmingCharacters(in: .whitespacesAndNewlines) }
    .filter { !$0.isEmpty && !isChromeText($0) }

  if let token = exactToken {
    let hits = texts.filter {
      $0 == token || ($0.contains(token) && !$0.lowercased().contains("reply with") && $0.count <= token.count + 40)
    }
    log("tick tokenHits=\(hits.count)")
    if let a = hits.first(where: { $0 == token }) ?? hits.first {
      if a == best { stable += 1 } else { stable = 0; best = a }
      if stable >= 2 {
        pb.clearContents()
        if let oldString { pb.setString(oldString, forType: .string) }
        let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
        try? best.write(toFile: respPath, atomically: true, encoding: .utf8)
        emit([
          "ok": true,
          "status": "complete",
          "mode": "chat",
          "workOn": false,
          "attached": true,
          "attachLabels": Array(labels.prefix(5)),
          "zipPath": zipPath,
          "responsePath": respPath,
          "finalDeliveryText": best,
          "mustReturnFinalDelivery": true,
          "mustReturnVerbatim": true,
          "method": "clipboard-file-paste",
        ])
      }
      continue
    }
  }

  var after = false
  var collected: [String] = []
  for t in texts {
    if t.contains(fingerprint) || t == prompt { after = true; collected = []; continue }
    if after { collected.append(t) }
  }
  let assistant = collected.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
  log("tick chars=\(assistant.count)")
  let sig = "\(assistant.count)|\(assistant.prefix(60))"
  if sig == last && !assistant.isEmpty { stable += 1 } else { stable = 0; last = sig; if !assistant.isEmpty { best = assistant } }
  if stable >= 3 && !best.isEmpty && !isChromeText(best) {
    pb.clearContents()
    if let oldString { pb.setString(oldString, forType: .string) }
    let respPath = zipURL.deletingLastPathComponent().appendingPathComponent("chatgpt-zip-response.md").path
    try? best.write(toFile: respPath, atomically: true, encoding: .utf8)
    emit([
      "ok": true,
      "status": "complete",
      "mode": "chat",
      "workOn": false,
      "attached": true,
      "attachLabels": Array(labels.prefix(5)),
      "zipPath": zipPath,
      "responsePath": respPath,
      "finalDeliveryText": best,
      "mustReturnFinalDelivery": true,
      "mustReturnVerbatim": true,
      "method": "clipboard-file-paste",
    ])
  }
}
pb.clearContents()
if let oldString { pb.setString(oldString, forType: .string) }
emit(["ok": false, "code": "TIMEOUT", "attached": attached, "partial": best], exitCode: 1)
