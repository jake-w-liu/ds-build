import { constants as FS_CONSTANTS } from "node:fs";
import { access, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const DIRECT_AX_UPLOAD_HELPER_PATH = path.join(SCRIPT_DIR, "psst_ax_upload.swift");

const APP_BUNDLE_ID_LEGACY = "com.openai.chat";
const APP_BUNDLE_ID = "com.openai.codex";
const APP_BUNDLE_IDS = [APP_BUNDLE_ID, APP_BUNDLE_ID_LEGACY];
const APP_NAME = "ChatGPT";
const APP_PROCESS_NAME = "ChatGPT";
const APP_SURFACE = "psst-gpt";
// Main relay calls wait until ChatGPT finishes unless the caller supplies an
// explicit response timeout. This avoids cutting off long GPT Pro runs.
const DEFAULT_TIMEOUT_MS = 0;
const DEFAULT_WAIT_CHUNK_MS = 90000;
const POLL_INTERVAL_MS = 2000;
const RESPONSE_STABLE_MS = 8000;
const DEFAULT_RESPONSE_START_TIMEOUT_MS = 90 * 1000;
// Use an unlimited JXA timeout so long-thinking prompts are not killed by the
// bridge before the app-level wait loop decides whether to continue.
const JXA_TIMEOUT_MS = 0;
const DEFAULT_BACKGROUND = true;
// Attachment/upload readiness is signal-driven by default. Zero means unlimited;
// callers may still provide an explicit positive cap.
const DEFAULT_UPLOAD_OPERATION_TIMEOUT_MS = 0;
const DEFAULT_UPLOAD_TIMEOUT_MS = DEFAULT_UPLOAD_OPERATION_TIMEOUT_MS;
const DEFAULT_UPLOAD_RESPONSE_START_TIMEOUT_MS = 0;
const DIRECT_AX_PROBE_TIMEOUT_MS = 30 * 1000;
const DIRECT_AX_PROBE_TIMEOUT_RETRY_MS = 2 * 60 * 1000;
const DIRECT_AX_PROBE_PROCESS_OVERHEAD_MS = 15 * 1000;
const DIRECT_AX_UPLOAD_PROCESS_OVERHEAD_MS = 120 * 1000;
// Rate-limit setup dialogs so missing permissions do not interrupt every run.
const ACCESSIBILITY_REMINDER_COOLDOWN_MS = 24 * 60 * 60 * 1000;
const ACCESSIBILITY_REMINDER_DIALOG_TIMEOUT_MS = 25000;
const ACCESSIBILITY_REMINDER_GIVEUP_AFTER_SEC = 20;
// AXTextArea can report the full scrollable text height for long prompts while
// ChatGPT's controls remain in the visible composer shell.
const MAX_VISIBLE_COMPOSER_HEIGHT = 360;
const DEFAULT_AUDIT_CHUNK_CHARS = 45000;
const DEFAULT_AUDIT_MAX_FILE_BYTES = 512 * 1024;
const DEFAULT_AUDIT_MAX_TOTAL_BYTES = 6 * 1024 * 1024;
const DEFAULT_AUDIT_ACK_RETRY_LIMIT = 2;
const DEFAULT_UPLOAD_SHARD_BYTES = 20 * 1024 * 1024;
const DEFAULT_UPLOAD_MAX_ATTACHMENTS_PER_MESSAGE = 5;
const DEFAULT_UPLOAD_MAX_SINGLE_FILE_BYTES = DEFAULT_UPLOAD_SHARD_BYTES;
const DEFAULT_HARNESS_TIMEOUT_MS = 5 * 60 * 1000;
const UPLOAD_VERIFICATION_OK_PREFIX = "PsstGPT upload verification: OK";
const UPLOAD_VERIFICATION_FAIL_PREFIX = "PsstGPT upload verification: FAILED";
const PSST_BUNDLE_MARKER_FILENAME = ".psst-gpt-bundle.json";
const AUDIT_TEXT_EXTENSIONS = new Set([
  ".c",
  ".cc",
  ".cpp",
  ".css",
  ".go",
  ".h",
  ".hpp",
  ".html",
  ".ini",
  ".java",
  ".jl",
  ".js",
  ".jsx",
  ".json",
  ".jsonl",
  ".kt",
  ".lock",
  ".m",
  ".md",
  ".mjs",
  ".mm",
  ".php",
  ".py",
  ".rb",
  ".rs",
  ".sql",
  ".sh",
  ".svelte",
  ".swift",
  ".toml",
  ".ts",
  ".tsx",
  ".vue",
  ".txt",
  ".xml",
  ".yaml",
  ".yml",
]);
const AUDIT_TEXT_FILENAMES = new Set([
  ".gitignore",
  "AGENTS.md",
  "CLAUDE.md",
  "LICENSE",
  "Makefile",
  "README",
]);
const AUDIT_EXCLUDED_DIRS = new Set([
  ".git",
  "node_modules",
  "dist",
  "build",
  "coverage",
  "target",
  ".next",
  ".nuxt",
  ".cache",
]);
const UPLOAD_EXCLUDED_DIRS = new Set([
  ...AUDIT_EXCLUDED_DIRS,
  ".agents",
  ".arc",
  ".parcel-cache",
  ".svelte-kit",
  ".turbo",
  ".venv",
  "__pycache__",
  "vendor/bundle",
]);
const UPLOAD_EXCLUDED_FILENAMES = new Set([
  ".DS_Store",
]);

export async function runPsstGPT(options = {}) {
  return relayPromptToChatGPTApp(options);
}

export async function startPsstGPT(options = {}) {
  return relayPromptToChatGPTApp({
    ...options,
    returnPending: true,
    returnAfterSend: options.returnAfterSend ?? true,
    timeoutMs: options.timeoutMs ?? DEFAULT_WAIT_CHUNK_MS,
  });
}

export async function continuePsstGPT(options = {}) {
  return relayPromptToChatGPTApp({
    ...options,
    newChat: false,
  });
}

export async function pollPsstGPT(options = {}) {
  const {
    sessionId,
    query,
    statePath,
    timeoutMs = DEFAULT_WAIT_CHUNK_MS,
    returnPending = true,
    background = DEFAULT_BACKGROUND,
  } = options;
  assertStrictBackgroundOptions(options);
  const session = await findStoredAppSession({ sessionId, query, statePath });

  if (!session) {
    throw codedError(
      "PSST_GPT_SESSION_NOT_FOUND",
      "No stored PsstGPT session matched the request."
    );
  }

  await ensureChatGPTAppReady({ background, verify: false });
  const currentState = await readPsstGPTState({ background });
  if (!transcriptContainsPrompt(currentState, session.prompt)) {
    const storedResult = storedCompleteAppRelayResult(session, currentState);
    if (storedResult) {
      return {
        ...storedResult,
        recoveredFromStore: true,
        recoveryReason: "active prompt was not visible in the ChatGPT app transcript",
      };
    }
    throw codedError(
      "PSST_GPT_SESSION_NOT_ACTIVE",
      "The stored PsstGPT session is not visible in the active ChatGPT app window and no complete stored assistant response is available. PsstGPT cannot reopen prior conversations by URL.",
      { session: publicAppSession(session) }
    );
  }

  const result = await waitForAppAssistantResponse({
    prompt: session.prompt,
    timeoutMs,
    allowPending: returnPending,
    background,
  });
  const messages = messagesForAppRelay(session.prompt, result.assistantText, session.messages);
  const record = await upsertAppSessionRecord({
    statePath,
    relaySessionId: session.relaySessionId,
    prompt: session.prompt,
    title: result.state.title,
    mode: result.state.visibleModelLabel || session.mode,
    background,
    status: result.status,
    messages,
    tags: session.tags ?? [],
  });

  return appRelayResult({
    status: result.status,
    assistantText: result.assistantText,
    state: result.state,
    record,
  });
}

export async function listPsstGPTSessions(options = {}) {
  const { query = "", limit = 20, statePath } = options;
  const store = await loadAppSessionStore(statePath);
  return filterAppSessions(store.sessions, query)
    .slice(0, limit)
    .map((session) => publicAppSession(session));
}

export async function getPsstGPTSession(options = {}) {
  const session = await findStoredAppSession(options);
  return session ? publicAppSession(session) : null;
}

export async function readPsstGPTState(options = {}) {
  if (options.allowForeground !== true) {
    assertStrictBackgroundOptions(options);
  }
  const background = options.background ?? DEFAULT_BACKGROUND;
  if (options.ensure !== false) {
    await ensureChatGPTAppReady({ background });
  }
  return runJxa("snapshot", { background });
}

export async function doctorPsstGPT() {
  const checks = [];
  checks.push(buildDoctorPlatformCheck());
  if (!doctorCheckReady(checks, "platform")) {
    return buildDoctorResult(checks);
  }

  try {
    await assertChatGPTAppInstalled();
    checks.push({
      name: "chatgptApp",
      status: "pass",
      ready: true,
      message: "ChatGPT.app is installed in a supported location.",
      bundleId: APP_BUNDLE_ID,
      nextSteps: [],
    });
  } catch (error) {
    checks.push(buildDoctorFailureCheck("chatgptApp", error));
    return buildDoctorResult(checks);
  }

  checks.push(await probeDoctorRelayMode({
    name: "strictBackgroundTextRelay",
    background: true,
    allowWindowRecovery: false,
    message: "Strict-background text relay is ready.",
  }));
  checks.push(await probeDoctorRelayMode({
    name: "foregroundUploadRelay",
    background: false,
    allowWindowRecovery: true,
    message: "Foreground upload relay is ready.",
    ensureReady: ensureForegroundUploadRelayReady,
  }));

  return buildDoctorResult(checks);
}

export function planPsstGPTTask(options = {}) {
  return resolvePsstGPTTaskPlan(options);
}

export async function runPsstGPTTask(options = {}) {
  const plan = resolvePsstGPTTaskPlan(options);
  const {
    prompt,
    root = process.cwd(),
    timeoutMs = DEFAULT_TIMEOUT_MS,
    waitChunkMs = DEFAULT_WAIT_CHUNK_MS,
    statePath,
    tags = [],
  } = options;

  if (plan.action === "upload-audit") {
    const result = await uploadAuditPsstGPT({
      root,
      outputDir: options.outputDir,
      bundle: options.bundle,
      uploadBundle: options.uploadBundle,
      prompt: buildUploadTaskPrompt(prompt),
      timeoutMs,
      waitChunkMs,
      uploadTimeoutMs: options.uploadTimeoutMs,
      maxAttachmentsPerMessage: options.maxAttachmentsPerMessage,
      statePath,
      tags: dedupe([...tags, "task-router", "upload-audit"]),
      maxSingleFileBytes: options.maxSingleFileBytes,
      includeDefaultExcludes: options.includeDefaultExcludes,
      excludeDirs: options.excludeDirs,
      excludeFilenames: options.excludeFilenames,
    });
    return { ...result, taskPlan: plan };
  }

  if (plan.action === "audit") {
    const result = await auditPsstGPT({
      root,
      outputDir: options.outputDir,
      prompt: buildTextAuditTaskPrompt(prompt),
      timeoutMs,
      waitChunkMs,
      chunkChars: options.chunkChars,
      maxFileBytes: options.maxFileBytes,
      maxTotalBytes: options.maxTotalBytes,
      background: true,
      statePath,
      tags: dedupe([...tags, "task-router", "text-audit"]),
    });
    return { ...result, taskPlan: plan };
  }

  const result = await runPsstGPT({
    prompt,
    timeoutMs,
    waitChunkMs,
    returnPending: options.returnPending,
    statePath,
    newChat: options.newChat,
    relaySessionId: options.relaySessionId,
    background: true,
    returnAfterSend: options.returnAfterSend,
    tags: dedupe([...tags, "task-router", "text-relay"]),
  });
  return { ...result, taskPlan: plan };
}

export async function harnessPsstGPT(options = {}) {
  const marker = options.marker || `PSST_HARNESS_UPLOAD_OK_${Date.now()}`;
  const root = options.root
    ? path.resolve(options.root)
    : await makeHarnessProject(marker);
  const prompt = [
    "debug audit the full codebase",
    `Read the uploaded source files and reply exactly with the marker string ${marker} and no other text.`,
  ].join(". ");
  const result = await runPsstGPTTask({
    root,
    prompt,
    timeoutMs: options.timeoutMs ?? DEFAULT_HARNESS_TIMEOUT_MS,
    uploadTimeoutMs: options.uploadTimeoutMs ?? DEFAULT_UPLOAD_TIMEOUT_MS,
    maxSingleFileBytes: options.maxSingleFileBytes,
    maxAttachmentsPerMessage: options.maxAttachmentsPerMessage,
    statePath: options.statePath,
    tags: dedupe([...(options.tags ?? []), "harness"]),
  });
  const responseText = String(result.assistantText ?? "");
  const responseFileText = result.responsePath
    ? await readFile(result.responsePath, "utf8").catch(() => "")
    : "";
  const checks = {
    routedToUpload: result.taskPlan?.action === "upload-audit",
    createdUploadBundle: Boolean(result.bundle?.archivePath && result.bundle?.shards?.length),
    assistantContainsMarker: responseText.includes(marker),
    responseFileContainsMarker: responseFileText.includes(marker),
    resultFileWritten: result.resultPath ? await fileExists(result.resultPath) : false,
  };
  const verified = Object.values(checks).every(Boolean);

  if (!verified && options.throwOnFailure !== false) {
    throw codedError(
      "PSST_GPT_HARNESS_FAILED",
      "PsstGPT harness did not verify the full upload and response persistence path.",
      { marker, root, checks, result }
    );
  }

  return {
    ok: verified,
    marker,
    root,
    checks,
    taskPlan: result.taskPlan,
    bundle: result.bundle,
    responsePath: result.responsePath,
    resultPath: result.resultPath,
    assistantText: result.assistantText,
    finalDeliveryText: result.finalDeliveryText,
    session: result.session,
  };
}

function resolvePsstGPTTaskPlan(options = {}) {
  const prompt = String(options.prompt ?? "");
  if (!prompt.trim()) {
    throw codedError("PROMPT_MISSING", "A non-empty prompt is required.");
  }

  const explicitTransport = normalizeWhitespace(
    options.transport ?? options.workflow ?? options.mode ?? ""
  ).toLowerCase();
  const text = normalizeWhitespace(prompt).toLowerCase();
  const strictRequested = /\b(strict[-\s]?background|background only|no popups?|no foreground|do not foreground|don't foreground|without foreground|without popups?)\b/i.test(text);
  const uploadRequested =
    /\b(upload|uploads?|zip|zips?|zipped|archive|archives?|full[-\s]?file|full codebase|full repo|entire codebase|whole codebase|whole repo|all files|large codebase|no truncation|untruncated)\b/i.test(text);
  const codebaseRequested =
    /\b(codebase|repo|repository|project|source tree|source files?|workspace|current code|current repo|current project)\b/i.test(text);
  const auditRequested =
    /\b(audit|debug|review|inspect|analy[sz]e|correctness|find bugs?|fix bugs?|diagnose)\b/i.test(text);

  if (/^(upload|upload-audit|file-upload|full-upload|zip|archive)$/.test(explicitTransport)) {
    return {
      action: "upload-audit",
      transport: "foreground-upload",
      requiresForeground: true,
      reason: "explicit upload transport requested",
    };
  }
  if (/^(audit|text-audit|text-bundle|strict-background|background)$/.test(explicitTransport)) {
    return {
      action: "audit",
      transport: "strict-background-text-bundle",
      requiresForeground: false,
      reason: "explicit strict-background text audit transport requested",
    };
  }
  if (strictRequested && auditRequested && codebaseRequested) {
    return {
      action: "audit",
      transport: "strict-background-text-bundle",
      requiresForeground: false,
      reason: "strict-background codebase audit requested",
    };
  }
  if ((auditRequested && codebaseRequested) || uploadRequested) {
    return {
      action: "upload-audit",
      transport: "foreground-upload",
      requiresForeground: true,
      reason: uploadRequested
        ? "prompt requests full/upload/archive codebase handling"
        : "prompt requests codebase audit/debugging",
    };
  }
  return {
    action: "run",
    transport: "strict-background-text",
    requiresForeground: false,
    reason: "plain ChatGPT app prompt",
  };
}

function buildUploadTaskPrompt(userPrompt) {
  return [
    "You are receiving a PsstGPT upload bundle as one uploaded source-archive.zip file.",
    "Use the uploaded archive as the source of truth.",
    "If the user asks for an exact output string or format, follow that request exactly.",
    "If the request is a code audit/debug task, lead with confirmed findings ordered by severity and cite exact file paths and line numbers when possible.",
    "",
    `User request: ${String(userPrompt ?? "").trim()}`,
  ].join("\n");
}

function buildTextAuditTaskPrompt(userPrompt) {
  return [
    "Audit the provided PsstGPT line-numbered text bundle.",
    "Use the bundle as the source of truth.",
    "If the user asks for an exact output string or format, follow that request exactly.",
    "For audit/debug tasks, lead with confirmed findings ordered by severity and cite exact file paths and line numbers.",
    "",
    `User request: ${String(userPrompt ?? "").trim()}`,
  ].join("\n");
}

function buildUploadVerificationInstruction(filePaths = []) {
  const fileNames = dedupe(filePaths.map((filePath) => path.basename(filePath))).filter(Boolean);
  const fileList = fileNames.length > 0 ? fileNames.join(", ") : "the uploaded source archive";
  return [
    `On the first line, write exactly "${UPLOAD_VERIFICATION_OK_PREFIX} ${fileList}" only if you successfully inspected ${fileList}.`,
    `Otherwise, write exactly "${UPLOAD_VERIFICATION_FAIL_PREFIX} <reason>" on the first line.`,
  ].join(" ");
}

function buildVerifiedUploadAuditPrompt(prompt, filePaths = []) {
  const trimmedPrompt = String(prompt ?? "").trim();
  if (!trimmedPrompt || isExactOutputRequest(trimmedPrompt)) {
    return trimmedPrompt;
  }
  return [
    buildUploadVerificationInstruction(filePaths),
    "After that first verification line, provide the full requested answer.",
    "",
    trimmedPrompt,
  ].join("\n");
}

async function makeHarnessProject(marker) {
  const root = await mkdir(path.join(os.tmpdir(), "psst-gpt-harness"), { recursive: true })
    .then(() => path.join(os.tmpdir(), "psst-gpt-harness", `project-${Date.now()}`));
  await mkdir(path.join(root, "src"), { recursive: true });
  await writeFile(path.join(root, "README.md"), `# PsstGPT Harness\n\nMarker: ${marker}\n`, "utf8");
  await writeFile(path.join(root, "src", "marker.mjs"), `export const marker = "${marker}";\n`, "utf8");
  return root;
}

export async function createPsstGPTAuditBundle(options = {}) {
  const {
    root = process.cwd(),
    outputDir,
    maxFileBytes = DEFAULT_AUDIT_MAX_FILE_BYTES,
    maxTotalBytes = DEFAULT_AUDIT_MAX_TOTAL_BYTES,
  } = options;
  const resolvedRoot = path.resolve(root);
  const rootStat = await stat(resolvedRoot);
  if (!rootStat.isDirectory()) {
    throw codedError(
      "PSST_GPT_AUDIT_ROOT_NOT_DIRECTORY",
      `Audit bundle root is not a directory: ${resolvedRoot}`
    );
  }

  const createdAt = new Date().toISOString();
  const bundleId = `psst-gpt-audit-${createdAt.replace(/[:.]/g, "-")}`;
  const bundleDirFallback = path.join(os.tmpdir(), "psst-gpt", "audit-bundles", bundleId);

  const { files, skipped } = await collectAuditFiles(resolvedRoot, {
    maxFileBytes,
    maxTotalBytes,
  });
  if (files.length === 0) {
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_EMPTY",
      "No auditable text files were found for the requested root.",
      { root: resolvedRoot, skipped }
    );
  }
  const { bundleDir, bundleDirExisted } = await ensureBundleOutputDir({
    outputDir,
    fallbackDir: bundleDirFallback,
    invalidCode: "PSST_GPT_AUDIT_OUTPUT_DIR_INVALID",
    invalidLabel: "audit bundle outputDir",
    disallowedPath: resolvedRoot,
    disallowedMessage: `Audit bundle outputDir must not be the same directory as the audit root: ${resolvedRoot}`,
  });
  const markdown = formatAuditBundleMarkdown({
    bundleId,
    createdAt,
    root: resolvedRoot,
    files,
    skipped,
    maxFileBytes,
    maxTotalBytes,
  });
  const manifest = {
    bundleId,
    createdAt,
    root: resolvedRoot,
    maxFileBytes,
    maxTotalBytes,
    files: files.map(({ path: filePath, bytes, lines }) => ({ path: filePath, bytes, lines })),
    skipped,
  };
  const markdownPath = path.join(bundleDir, "audit-bundle.md");
  const manifestPath = path.join(bundleDir, "manifest.json");
  const zipPath = path.join(bundleDir, "audit-bundle.zip");

  try {
    await writeFile(markdownPath, markdown, "utf8");
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
    await writeBundleMarker(bundleDir, {
      kind: "audit",
      bundleId,
      createdAt,
    });

    let zipCreated = false;
    try {
      await execFileAsync("zip", ["-q", "audit-bundle.zip", "audit-bundle.md", "manifest.json"], {
        cwd: bundleDir,
        timeout: 30000,
      });
      zipCreated = true;
    } catch {
      // The Markdown bundle is the primary artifact; zip is a convenience when available.
    }

    return {
      ok: true,
      bundleId,
      root: resolvedRoot,
      outputDir: bundleDir,
      markdownPath,
      manifestPath,
      zipPath: zipCreated ? zipPath : null,
      files: manifest.files,
      skipped,
    };
  } catch (error) {
    if (!bundleDirExisted) {
      await rm(bundleDir, { recursive: true, force: true });
    }
    throw error;
  }
}

export async function auditPsstGPT(options = {}) {
  const {
    bundle: bundleOptions,
    auditBundle,
    chunkChars = DEFAULT_AUDIT_CHUNK_CHARS,
    prompt: requestedPrompt,
    root,
    outputDir,
    maxFileBytes,
    maxTotalBytes,
    preflight = ensureStrictBackgroundRelayReady,
    bundleFactory = createPsstGPTAuditBundle,
    ...relayOptions
  } = options;
  await preflight();
  const bundle = looksLikeAuditBundleRecord(auditBundle)
    ? await validateProvidedAuditBundle(auditBundle)
    : await bundleFactory(
      bundleOptions ?? auditBundle ?? { root, outputDir, maxFileBytes, maxTotalBytes }
    );
  const bundleText = await readFile(bundle.markdownPath, "utf8");
  const prompt = requestedPrompt || [
    "Audit the provided PsstGPT audit bundle for correctness.",
    "Use the line-numbered source in the bundle as the source of truth.",
    "Lead with confirmed findings ordered by severity.",
    "For each finding, cite exact file and line numbers, explain the realistic failure mode, and propose a precise fix.",
    "Separate confirmed findings from hypotheses or live-UI verification gaps.",
    "If you find no confirmed correctness bugs, say so clearly and list the highest-value residual risks or missing tests.",
  ].join(" ");

  const result = await relayAuditBundleText({
    ...relayOptions,
    bundle,
    bundleText,
    prompt,
    chunkChars,
    tags: dedupe([...(relayOptions.tags ?? []), "audit-bundle"]),
  });

  return {
    ...result,
    bundle,
  };
}

export async function createPsstGPTUploadBundle(options = {}) {
  const {
    root = process.cwd(),
    outputDir,
    maxSingleFileBytes = DEFAULT_UPLOAD_MAX_SINGLE_FILE_BYTES,
    includeDefaultExcludes = true,
    excludeDirs = [],
    excludeFilenames = [],
  } = options;
  const resolvedRoot = path.resolve(root);
  const rootStat = await stat(resolvedRoot);
  if (!rootStat.isDirectory()) {
    throw codedError(
      "PSST_GPT_UPLOAD_ROOT_NOT_DIRECTORY",
      `Upload bundle root is not a directory: ${resolvedRoot}`
    );
  }

  const normalizedMaxSingleFileBytes = Math.max(
    1024,
    Number(maxSingleFileBytes) || DEFAULT_UPLOAD_MAX_SINGLE_FILE_BYTES
  );
  const createdAt = new Date().toISOString();
  const bundleId = `psst-gpt-upload-${createdAt.replace(/[:.]/g, "-")}`;
  const bundleDirFallback = path.join(os.tmpdir(), "psst-gpt", "upload-bundles", bundleId);

  const { files, skipped } = await collectUploadFiles(resolvedRoot, {
    includeDefaultExcludes,
    excludeDirs,
    excludeFilenames,
    maxSingleFileBytes: normalizedMaxSingleFileBytes,
  });
  if (files.length === 0) {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_EMPTY",
      "No uploadable files were found for the requested root.",
      { root: resolvedRoot, skipped }
    );
  }
  const { bundleDir, bundleDirExisted } = await ensureBundleOutputDir({
    outputDir,
    fallbackDir: bundleDirFallback,
    invalidCode: "PSST_GPT_UPLOAD_OUTPUT_DIR_INVALID",
    invalidLabel: "upload bundle outputDir",
    disallowedPath: resolvedRoot,
    disallowedMessage: `Upload bundle outputDir must not be the same directory as the upload root: ${resolvedRoot}`,
  });

  const archiveName = "source-archive.zip";
  const archivePath = path.join(bundleDir, archiveName);
  try {
    await zipRelativeFiles({
      root: resolvedRoot,
      zipPath: archivePath,
      relativePaths: files.map((file) => file.path),
    });
    await writeBundleMarker(bundleDir, {
      kind: "upload",
      bundleId,
      createdAt,
    });
    const archiveStat = await stat(archivePath);
    const archive = buildUploadArchiveRecord({
      index: 1,
      name: archiveName,
      archivePath,
      archiveBytes: archiveStat.size,
      files,
    });

    const result = {
      ok: true,
      bundleId,
      root: resolvedRoot,
      outputDir: bundleDir,
      archivePath,
      attachmentPaths: [archivePath],
      files: files.map((file) => ({ path: file.path, bytes: file.bytes })),
      skipped,
      shards: [archive],
      archives: [archive],
    };
    return result;
  } catch (error) {
    if (!bundleDirExisted) {
      await rm(bundleDir, { recursive: true, force: true });
    }
    throw error;
  }
}

export async function uploadAuditPsstGPT(options = {}) {
  const {
    bundle: bundleOptions,
    uploadBundle,
    root,
    outputDir,
    prompt: requestedPrompt,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    waitChunkMs = DEFAULT_WAIT_CHUNK_MS,
    uploadTimeoutMs = DEFAULT_UPLOAD_TIMEOUT_MS,
    maxAttachmentsPerMessage = DEFAULT_UPLOAD_MAX_ATTACHMENTS_PER_MESSAGE,
    statePath,
    relaySessionId = `app-${Date.now()}`,
    tags = [],
    preflight = ensureForegroundUploadRelayReady,
    bundleFactory = createPsstGPTUploadBundle,
    ...bundleOptionOverrides
  } = options;
  if (!uploadBundle) {
    await preflight();
  }
  const bundle = uploadBundle
    ? await validateProvidedUploadBundle(uploadBundle)
    : await bundleFactory(
    bundleOptions ?? { root, outputDir, ...bundleOptionOverrides }
  );
  const finalPrompt = requestedPrompt || [
    "Audit the uploaded PsstGPT source-archive.zip for correctness.",
    "Use the uploaded archive to inspect the source tree and cite exact file paths and line numbers when possible.",
    "Lead with confirmed correctness findings ordered by severity.",
    "For each finding, explain the realistic failure mode and propose a precise fix.",
    "If you cannot inspect an uploaded archive, say exactly which uploaded file failed and why.",
    "Return the audit as Markdown.",
  ].join(" ");
  const attachmentPaths = bundle.attachmentPaths?.length
    ? bundle.attachmentPaths
    : [bundle.archivePath, ...bundle.shards.map((shard) => shard.path)].filter(Boolean);
  const verifiedFinalPrompt = buildVerifiedUploadAuditPrompt(finalPrompt, attachmentPaths);
  const finalAuditResponseAcceptable = isExactOutputRequest(finalPrompt)
    ? undefined
    : isSubstantiveAuditFinalResponse;
  const groups = chunkArray(
    attachmentPaths,
    Math.max(1, Number(maxAttachmentsPerMessage) || DEFAULT_UPLOAD_MAX_ATTACHMENTS_PER_MESSAGE)
  );
  try {
    if (groups.length === 1) {
      const result = await relayPromptWithFileUploads({
        prompt: [
          verifiedFinalPrompt,
          "",
          `PsstGPT upload bundle: ${bundle.bundleId}`,
        ].join("\n"),
        filePaths: groups[0],
        newChat: true,
        timeoutMs,
        waitChunkMs,
        uploadTimeoutMs,
        statePath,
        relaySessionId,
        tags: dedupe([...tags, "upload-bundle"]),
        isFinalResponseAcceptable: finalAuditResponseAcceptable,
      });
      const substantiveResult = await ensureSubstantiveAuditResult({
        result,
        requestedPrompt: verifiedFinalPrompt,
        bundleId: bundle.bundleId,
        retryLimit: options.auditAckRetryLimit,
        relayOptions: {
          timeoutMs,
          waitChunkMs,
          statePath,
          relaySessionId,
          background: false,
          useUploadRetryRelay: true,
          uploadTimeoutMs,
          tags: dedupe([...tags, "upload-bundle", "audit-ack-retry"]),
          isFinalResponseAcceptable: finalAuditResponseAcceptable,
        },
      });
      const normalizedResult = await normalizeUploadAuditResult({
        result: substantiveResult,
        attachmentPaths,
        requireVerification: !isExactOutputRequest(finalPrompt),
        statePath,
        relaySessionId,
      });
      return persistUploadAuditResult({ result: normalizedResult, bundle });
    }

    await relayPromptWithFileUploads({
      prompt: [
        `You will receive a PsstGPT upload bundle in ${groups.length} attachment groups.`,
        "Do not audit until I send FINAL UPLOAD AUDIT REQUEST.",
        "Read and retain the uploaded source archive zip files from each group.",
        `Bundle ID: ${bundle.bundleId}`,
        "Reply exactly: READY",
      ].join("\n"),
      filePaths: groups[0],
      newChat: true,
      timeoutMs,
      waitChunkMs,
      uploadTimeoutMs,
      statePath,
      relaySessionId,
      tags: dedupe([...tags, "upload-bundle"]),
    });

    for (let index = 1; index < groups.length; index += 1) {
      await relayPromptWithFileUploads({
        prompt: [
          `PsstGPT upload bundle attachment group ${index + 1}/${groups.length}.`,
          `Bundle ID: ${bundle.bundleId}`,
          `Reply exactly: ACK ${index + 1}`,
        ].join("\n"),
        filePaths: groups[index],
        newChat: false,
        timeoutMs,
        waitChunkMs,
        uploadTimeoutMs,
        statePath,
        relaySessionId,
        tags: dedupe([...tags, "upload-bundle"]),
      });
    }

    const result = await continuePsstGPTAfterUpload({
      prompt: [
        "FINAL UPLOAD AUDIT REQUEST.",
        `Bundle ID: ${bundle.bundleId}`,
        verifiedFinalPrompt,
        "Use only the uploaded source archive zip files already provided in this chat.",
      ].join("\n"),
      timeoutMs,
      waitChunkMs,
      statePath,
      relaySessionId,
      uploadTimeoutMs,
      tags: dedupe([...tags, "upload-bundle"]),
      isFinalResponseAcceptable: finalAuditResponseAcceptable,
    });
    const substantiveResult = await ensureSubstantiveAuditResult({
      result,
      requestedPrompt: verifiedFinalPrompt,
      bundleId: bundle.bundleId,
      retryLimit: options.auditAckRetryLimit,
      relayOptions: {
        timeoutMs,
        waitChunkMs,
        statePath,
        relaySessionId,
        background: false,
        useUploadRetryRelay: true,
        uploadTimeoutMs,
        tags: dedupe([...tags, "upload-bundle", "audit-ack-retry"]),
        isFinalResponseAcceptable: finalAuditResponseAcceptable,
      },
    });
    const normalizedResult = await normalizeUploadAuditResult({
      result: substantiveResult,
      attachmentPaths,
      requireVerification: !isExactOutputRequest(finalPrompt),
      statePath,
      relaySessionId,
    });
    return persistUploadAuditResult({ result: normalizedResult, bundle });
  } catch (error) {
    const failure = await persistUploadAuditFailure({
      error,
      bundle,
      requestedPrompt: verifiedFinalPrompt,
      attachmentPaths,
      relaySessionId,
    });
    const enrichedError = codedError(
      failure.code,
      failure.message,
      {
        cause: error,
        bundle: failure.bundle,
        responsePath: failure.responsePath,
        resultPath: failure.resultPath,
        parsed: error?.parsed ?? null,
        details: error?.details ?? null,
      }
    );
    throw enrichedError;
  }
}

async function collectUploadFiles(root, {
  includeDefaultExcludes,
  excludeDirs,
  excludeFilenames,
  maxSingleFileBytes,
}) {
  const skipped = [];
  const files = [];
  const directoryExcludes = new Set([
    ...(includeDefaultExcludes ? UPLOAD_EXCLUDED_DIRS : []),
    ...excludeDirs,
  ]);
  const filenameExcludes = new Set([
    ...(includeDefaultExcludes ? UPLOAD_EXCLUDED_FILENAMES : []),
    ...excludeFilenames,
  ]);

  async function walk(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch (error) {
      skipped.push({
        path: path.relative(root, dir) || ".",
        reason: `Could not read directory: ${error?.code ?? error?.message ?? error}`,
      });
      return;
    }

    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      const absolutePath = path.join(dir, entry.name);
      const relativePath = path.relative(root, absolutePath);
      if (/[\r\n]/.test(relativePath)) {
        skipped.push({ path: relativePath, reason: "path contains a newline" });
        continue;
      }
      if (entry.isDirectory()) {
        if (directoryExcludes.has(entry.name) || directoryExcludes.has(relativePath)) {
          skipped.push({ path: relativePath, reason: "excluded directory" });
          continue;
        }
        const bundleSkipReason = await generatedPsstBundleSkipReason(absolutePath);
        if (bundleSkipReason) {
          skipped.push({ path: relativePath, reason: bundleSkipReason });
          continue;
        }
        await walk(absolutePath);
        continue;
      }
      if (!entry.isFile()) {
        skipped.push({ path: relativePath, reason: "not a regular file" });
        continue;
      }
      if (filenameExcludes.has(entry.name) || filenameExcludes.has(relativePath)) {
        skipped.push({ path: relativePath, reason: "excluded file" });
        continue;
      }

      try {
        const fileStat = await stat(absolutePath);
        if (fileStat.size > maxSingleFileBytes) {
          skipped.push({
            path: relativePath,
            reason: `larger than maxSingleFileBytes (${fileStat.size} > ${maxSingleFileBytes})`,
          });
          continue;
        }
        await access(absolutePath, FS_CONSTANTS.R_OK);
        files.push({ path: relativePath, absolutePath, bytes: fileStat.size });
      } catch (error) {
        skipped.push({
          path: relativePath,
          reason: `Could not read file: ${error?.code ?? error?.message ?? error}`,
        });
        continue;
      }
    }
  }

  await walk(root);
  return { files, skipped };
}

function buildUploadArchiveRecord({
  index,
  name,
  archivePath,
  archiveBytes,
  files,
}) {
  return {
    index,
    name,
    path: archivePath,
    bytes: archiveBytes,
    fileCount: files.length,
    uncompressedBytes: files.reduce((sum, file) => sum + file.bytes, 0),
    files: files.map((file) => file.path),
  };
}

async function zipRelativeFiles({ root, zipPath, relativePaths }) {
  await new Promise((resolve, reject) => {
    const child = spawn("zip", ["-q", "-X", zipPath, "-@"], {
      cwd: root,
      stdio: ["pipe", "ignore", "pipe"],
    });
    let stderr = "";
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", (error) => {
      reject(codedError(
        "PSST_GPT_ZIP_FAILED",
        "Could not start the zip command for the upload bundle archive.",
        { cause: error }
      ));
    });
    child.on("close", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(codedError(
        "PSST_GPT_ZIP_FAILED",
        "The zip command failed while creating an upload bundle archive.",
        { code, stderr }
      ));
    });
    child.stdin.end(`${relativePaths.join("\n")}\n`);
  });
}

async function relayPromptWithFileUploads({
  prompt,
  filePaths,
  newChat = true,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  waitChunkMs = DEFAULT_WAIT_CHUNK_MS,
  uploadTimeoutMs = DEFAULT_UPLOAD_TIMEOUT_MS,
  responseStartTimeoutMs,
  statePath,
  relaySessionId,
  tags = [],
  returnAfterSend = false,
  waitInBackground = true,
  skipFileValidation = false,
  isFinalResponseAcceptable,
}) {
  if (typeof prompt !== "string" || !prompt.trim()) {
    throw codedError("PROMPT_MISSING", "A non-empty prompt is required.");
  }
  const normalizedFilePaths = skipFileValidation
    ? Array.isArray(filePaths) ? filePaths : []
    : await validateUploadFilePaths(filePaths);
  if (normalizedFilePaths.length === 0) {
    if (!skipFileValidation) {
      throw codedError(
        "PSST_GPT_UPLOAD_FILES_MISSING",
        "At least one absolute file path is required for upload relay."
      );
    }
  }

  await ensureForegroundUploadRelayReady();
  const promptText = prompt.trim();
  const normalizedResponseStartTimeoutMs = normalizeResponseStartTimeoutMs(responseStartTimeoutMs, {
    overallTimeoutMs: timeoutMs,
    fallback: DEFAULT_UPLOAD_RESPONSE_START_TIMEOUT_MS,
  });
  const existingRecord = relaySessionId
    ? await findStoredAppSessionByRelayId(relaySessionId, statePath)
    : null;
  const baseMessages = messagesForAppRelay(promptText, "", existingRecord?.messages);
  const directResult = await runDirectAxUploadRelay({
    prompt: promptText,
    filePaths: normalizedFilePaths,
    newChat: newChat !== false,
    timeoutMs,
    uploadTimeoutMs,
    responseStartTimeoutMs: normalizedResponseStartTimeoutMs,
    returnAfterSend: true,
    restoreFrontmostOnExit: true,
  });
  const initialState = directResult.state ?? {};
  assertNoFatalAppBlocker(initialState);
  const pendingRecord = await upsertAppSessionRecord({
    statePath,
    relaySessionId,
    prompt: promptText,
    title: initialState.title ?? "ChatGPT",
    mode: initialState.visibleModelLabel,
    background: false,
    status: "pending",
    messages: baseMessages,
    tags,
  });

  if (returnAfterSend) {
    return appRelayResult({
      status: "pending",
      assistantText: "",
      state: initialState,
      record: pendingRecord,
    });
  }

  const result = await waitForAppAssistantResponseInChunks({
    prompt: promptText,
    timeoutMs,
    waitChunkMs,
    allowPending: false,
    background: waitInBackground !== false,
    allowForegroundRecovery: true,
    responseStartTimeoutMs: normalizedResponseStartTimeoutMs,
    isFinalResponseAcceptable,
    initialState,
    onPending: async (pendingResult) => {
      await upsertAppSessionRecord({
        statePath,
        relaySessionId: pendingRecord.relaySessionId,
        prompt: promptText,
        title: pendingResult.state.title,
        mode: pendingResult.state.visibleModelLabel || pendingRecord.mode,
        background: pendingResult.background !== false,
        status: pendingResult.status,
        messages: messagesForAppRelay(promptText, pendingResult.assistantText, baseMessages),
        tags,
      });
    },
  });
  const messages = messagesForAppRelay(promptText, result.assistantText, baseMessages);
  const record = await upsertAppSessionRecord({
    statePath,
    relaySessionId: pendingRecord.relaySessionId,
    prompt: promptText,
    title: result.state.title,
    mode: result.state.visibleModelLabel || pendingRecord.mode,
    background: result.background !== false,
    status: result.status,
    messages,
    tags,
  });

  return appRelayResult({
    status: result.status,
    assistantText: result.assistantText,
    state: result.state,
    record,
  });
}

async function runDirectAxUploadRelay({
  prompt,
  filePaths,
  newChat,
  timeoutMs,
  uploadTimeoutMs,
  responseStartTimeoutMs = DEFAULT_UPLOAD_RESPONSE_START_TIMEOUT_MS,
  returnAfterSend = false,
  restoreFrontmostOnExit = false,
}) {
  const payload = {
    prompt,
    filePaths,
    newChat,
    timeoutMs,
    uploadTimeoutMs,
    responseStartTimeoutMs,
    responseStableMs: RESPONSE_STABLE_MS,
    pollIntervalMs: POLL_INTERVAL_MS,
    returnAfterSend,
    restoreFrontmostOnExit,
  };
  return runDirectAxHelper(payload, {
    timeoutMs: calculateDirectAxRelayTimeoutMs({
      timeoutMs,
      uploadTimeoutMs,
      fileCount: filePaths.length,
      returnAfterSend,
    }),
    genericFailureCode: "PSST_GPT_DIRECT_AX_UPLOAD_FAILED",
    genericFailureMessage: "ChatGPT app upload relay failed in the direct Accessibility backend.",
    invalidResponseCode: "PSST_GPT_DIRECT_AX_INVALID_RESPONSE",
    invalidResponseMessage: "The direct Accessibility upload backend returned invalid JSON.",
  });
}

async function continuePsstGPTAfterUpload({
  prompt,
  timeoutMs,
  waitChunkMs = DEFAULT_WAIT_CHUNK_MS,
  statePath,
  relaySessionId,
  tags = [],
  uploadTimeoutMs = DEFAULT_UPLOAD_TIMEOUT_MS,
  isFinalResponseAcceptable,
}) {
  if (typeof prompt !== "string" || !prompt.trim()) {
    throw codedError("PROMPT_MISSING", "A non-empty prompt is required.");
  }
  const promptText = prompt.trim();
  const existingRecord = relaySessionId
    ? await findStoredAppSessionByRelayId(relaySessionId, statePath)
    : null;
  const baseMessages = messagesForAppRelay(promptText, "", existingRecord?.messages);

  const directResult = await runDirectAxUploadRelay({
    prompt: promptText,
    filePaths: [],
    newChat: false,
    timeoutMs,
    uploadTimeoutMs,
    responseStartTimeoutMs: 0,
    returnAfterSend: true,
    restoreFrontmostOnExit: true,
  });
  const initialState = directResult.state ?? {};
  assertNoFatalAppBlocker(initialState);

  const pendingRecord = await upsertAppSessionRecord({
    statePath,
    relaySessionId,
    prompt: promptText,
    title: initialState.title ?? "ChatGPT",
    mode: initialState.visibleModelLabel,
    background: false,
    status: "pending",
    messages: baseMessages,
    tags,
  });

  const result = await waitForAppAssistantResponseInChunks({
    prompt: promptText,
    timeoutMs,
    waitChunkMs,
    allowPending: false,
    background: true,
    allowForegroundRecovery: true,
    responseStartTimeoutMs: 0,
    isFinalResponseAcceptable,
    initialState,
    onPending: async (pendingResult) => {
      await upsertAppSessionRecord({
        statePath,
        relaySessionId: pendingRecord.relaySessionId,
        prompt: promptText,
        title: pendingResult.state.title,
        mode: pendingResult.state.visibleModelLabel || pendingRecord.mode,
        background: pendingResult.background !== false,
        status: pendingResult.status,
        messages: messagesForAppRelay(promptText, pendingResult.assistantText, baseMessages),
        tags,
      });
    },
  });

  const record = await upsertAppSessionRecord({
    statePath,
    relaySessionId: pendingRecord.relaySessionId,
    prompt: promptText,
    title: result.state.title,
    mode: result.state.visibleModelLabel || pendingRecord.mode,
    background: result.background !== false,
    status: result.status,
    messages: messagesForAppRelay(promptText, result.assistantText, baseMessages),
    tags,
  });

  return appRelayResult({
    status: result.status,
    assistantText: result.assistantText,
    state: result.state,
    record,
  });
}

async function probeDirectAxUploadRelayReady({
  restoreFrontmostOnExit = true,
  timeoutMs = DIRECT_AX_PROBE_TIMEOUT_MS,
} = {}) {
  const normalizedTimeoutMs = normalizeOverallTimeoutMs(timeoutMs, DIRECT_AX_PROBE_TIMEOUT_MS);
  const attemptTimeouts = [normalizedTimeoutMs];
  if (normalizedTimeoutMs > 0 && normalizedTimeoutMs < DIRECT_AX_PROBE_TIMEOUT_RETRY_MS) {
    attemptTimeouts.push(DIRECT_AX_PROBE_TIMEOUT_RETRY_MS);
  }
  let lastFailure;
  for (const attemptTimeoutMs of attemptTimeouts) {
    try {
      const helperTimeoutMs = attemptTimeoutMs === 0
        ? 0
        : attemptTimeoutMs + DIRECT_AX_PROBE_PROCESS_OVERHEAD_MS;
      const result = await runDirectAxHelper({
        action: "probe",
        prompt: "",
        filePaths: [],
        uploadTimeoutMs: attemptTimeoutMs,
        restoreFrontmostOnExit,
      }, {
        timeoutMs: helperTimeoutMs,
        genericFailureCode: "PSST_GPT_DIRECT_AX_PROBE_FAILED",
        genericFailureMessage: "ChatGPT app foreground upload readiness probe failed in the direct Accessibility backend.",
        invalidResponseCode: "PSST_GPT_DIRECT_AX_INVALID_RESPONSE",
        invalidResponseMessage: "The direct Accessibility foreground probe returned invalid JSON.",
      });
      return result?.state ?? result;
    } catch (error) {
      lastFailure = error;
      if (!isDirectAxProbeRetryableTimeout(error) || attemptTimeoutMs === attemptTimeouts.at(-1)) {
        throw error;
      }
    }
  }
  throw lastFailure;
}

function isDirectAxProbeRetryableTimeout(error = {}) {
  return isDirectAxRetryableTimeout(error, "PSST_GPT_DIRECT_AX_PROBE_FAILED");
}

function isDirectAxStateRetryableTimeout(error = {}) {
  return isDirectAxRetryableTimeout(error, "PSST_GPT_DIRECT_AX_STATE_FAILED");
}

function isDirectAxRetryableTimeout(error = {}, code) {
  if (error?.code === "PSST_GPT_DIRECT_AX_TIMEOUT") {
    return true;
  }
  if (error?.code !== code) {
    return false;
  }
  return error?.cause?.killed === true || String(error?.cause?.signal || "").toUpperCase() === "SIGTERM";
}

async function readDirectAxPsstGPTState({
  restoreFrontmostOnExit = false,
  timeoutMs = DIRECT_AX_PROBE_TIMEOUT_MS,
  genericFailureCode = "PSST_GPT_DIRECT_AX_STATE_FAILED",
  genericFailureMessage = "ChatGPT app foreground state snapshot failed in the direct Accessibility backend.",
  invalidResponseCode = "PSST_GPT_DIRECT_AX_INVALID_RESPONSE",
  invalidResponseMessage = "The direct Accessibility state snapshot returned invalid JSON.",
} = {}) {
  const normalizedTimeoutMs = normalizeOverallTimeoutMs(timeoutMs, DIRECT_AX_PROBE_TIMEOUT_MS);
  const attemptTimeouts = normalizedTimeoutMs > 0 ? [normalizedTimeoutMs, 0] : [0];
  let lastFailure;
  for (const attemptTimeoutMs of attemptTimeouts) {
    try {
      const helperTimeoutMs = attemptTimeoutMs === 0
        ? 0
        : attemptTimeoutMs + DIRECT_AX_PROBE_PROCESS_OVERHEAD_MS;
      const result = await runDirectAxHelper({
        action: "snapshot",
        prompt: "",
        filePaths: [],
        uploadTimeoutMs: attemptTimeoutMs,
        restoreFrontmostOnExit,
      }, {
        timeoutMs: helperTimeoutMs,
        genericFailureCode,
        genericFailureMessage,
        invalidResponseCode,
        invalidResponseMessage,
      });
      return result?.state ?? result;
    } catch (error) {
      lastFailure = error;
      if (!isDirectAxStateRetryableTimeout(error) || attemptTimeoutMs === attemptTimeouts.at(-1)) {
        throw error;
      }
    }
  }
  throw lastFailure;
}

async function copyDirectAxCurrentAssistant({
  baselineCopyMessageCount = 0,
  prompt = "",
  restoreFrontmostOnExit = true,
} = {}) {
  const result = await runDirectAxHelper({
    action: "copyLatestAssistant",
    prompt,
    filePaths: [],
    baselineCopyMessageCount,
    restoreFrontmostOnExit,
  }, {
    timeoutMs: DIRECT_AX_PROBE_TIMEOUT_MS + DIRECT_AX_PROBE_PROCESS_OVERHEAD_MS,
    genericFailureCode: "PSST_GPT_DIRECT_AX_COPY_FAILED",
    genericFailureMessage: "Could not copy the current ChatGPT assistant message.",
    invalidResponseCode: "PSST_GPT_DIRECT_AX_INVALID_RESPONSE",
    invalidResponseMessage: "The direct Accessibility copy backend returned invalid JSON.",
  });
  return typeof result?.assistantText === "string" ? result.assistantText.trim() : "";
}

async function runDirectAxHelper(payload, {
  timeoutMs,
  genericFailureCode,
  genericFailureMessage,
  invalidResponseCode,
  invalidResponseMessage,
}) {
  let stdout = "";
  let stderr = "";
  try {
    ({ stdout, stderr } = await execFileAsync(
      "/usr/bin/swift",
      [DIRECT_AX_UPLOAD_HELPER_PATH, JSON.stringify(payload)],
      {
        timeout: timeoutMs,
        maxBuffer: 10 * 1024 * 1024,
      }
    ));
  } catch (error) {
    const failure = parseDirectAxHelperJson(error?.stdout || stdout) ??
      parseDirectAxHelperJson(error?.stderr || stderr);
    if (failure) {
      const codedFailure = codedError(
        failure.code || genericFailureCode,
        failure.message || genericFailureMessage,
        {
          stdout: error?.stdout || stdout,
          stderr: error?.stderr || stderr,
          parsed: failure,
          cause: error,
        }
      );
      if (isAccessibilityError(codedFailure)) {
        await maybeShowAccessibilityReminder();
        throw decorateAccessibilityError(codedFailure);
      }
      throw codedFailure;
    }
    throw codedError(
      genericFailureCode,
      genericFailureMessage,
      {
        stdout: error?.stdout || stdout,
        stderr: error?.stderr || stderr,
        cause: error,
      }
    );
  }

  const parsed = parseDirectAxHelperJson(stdout);
  if (!parsed) {
    throw codedError(
      invalidResponseCode,
      invalidResponseMessage,
      { stdout, stderr }
    );
  }

  if (!parsed?.ok) {
    const error = codedError(
      parsed?.code || genericFailureCode,
      parsed?.message || genericFailureMessage,
      { stdout, stderr, parsed }
    );
    if (isAccessibilityError(error)) {
      await maybeShowAccessibilityReminder();
      throw decorateAccessibilityError(error);
    }
    throw error;
  }
  return parsed;
}

function parseDirectAxHelperJson(output) {
  const text = String(output ?? "").trim();
  if (!text) {
    return null;
  }
  try {
    return JSON.parse(text);
  } catch {
    // Swift normally emits only JSON. This fallback keeps helper output
    // parseable if compiler/runtime warnings appear before the payload.
  }
  for (let index = text.indexOf("{"); index >= 0; index = text.indexOf("{", index + 1)) {
    try {
      return JSON.parse(text.slice(index));
    } catch {
      // Keep scanning for a later object start.
    }
  }
  return null;
}

function calculateDirectAxRelayTimeoutMs({
  timeoutMs = DEFAULT_TIMEOUT_MS,
  uploadTimeoutMs = DEFAULT_UPLOAD_TIMEOUT_MS,
  fileCount = 0,
  returnAfterSend = false,
} = {}) {
  const normalizedUploadTimeoutMs = normalizeOverallTimeoutMs(uploadTimeoutMs, DEFAULT_UPLOAD_TIMEOUT_MS);
  const normalizedFileCount = Math.max(1, Number(fileCount) || 0);
  if (returnAfterSend) {
    if (normalizedUploadTimeoutMs === 0) {
      return 0;
    }
    return (normalizedUploadTimeoutMs * normalizedFileCount) + DIRECT_AX_UPLOAD_PROCESS_OVERHEAD_MS;
  }
  const normalizedResponseTimeoutMs = normalizeOverallTimeoutMs(timeoutMs, DEFAULT_TIMEOUT_MS);
  if (normalizedResponseTimeoutMs === 0 || normalizedUploadTimeoutMs === 0) {
    return 0;
  }
  return normalizedResponseTimeoutMs +
    (normalizedUploadTimeoutMs * normalizedFileCount) +
    DIRECT_AX_UPLOAD_PROCESS_OVERHEAD_MS;
}

function buildAccessibilitySetupHelpText() {
  return [
    "Enable Accessibility for the app running Codex, such as the Codex app, Terminal, iTerm, VS Code, or Cursor.",
    "If macOS separately prompts, also allow /usr/bin/osascript for text relay and /usr/bin/swift for file uploads.",
    "Then rerun the PsstGPT command.",
  ].join(" ");
}

function buildAccessibilityReminderMessage() {
  return [
    "PsstGPT needs macOS Accessibility before it can control the ChatGPT app.",
    "",
    "Turn on Accessibility for the app running Codex, such as the Codex app, Terminal, iTerm, VS Code, or Cursor.",
    "If macOS separately prompts, also allow /usr/bin/osascript for text relay and /usr/bin/swift for file uploads.",
    "",
    "Then rerun the PsstGPT command.",
  ].join("\n");
}

function isAccessibilityError(error) {
  if (!error) {
    return false;
  }
  if (error.code === "MACOS_ACCESSIBILITY_DISABLED") {
    return true;
  }
  const message = [
    error.message,
    error.details?.message,
    error.parsed?.message,
    error.stdout,
    error.stderr,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return message.includes("accessibility") &&
    (message.includes("not trusted") || message.includes("not enabled"));
}

function decorateAccessibilityError(error) {
  const baseMessage = normalizeWhitespace(
    error?.message || "macOS Accessibility automation is not enabled."
  ).replace(/[. ]+$/, "");
  return codedError(
    "MACOS_ACCESSIBILITY_DISABLED",
    `${baseMessage}. ${buildAccessibilitySetupHelpText()}`,
    {
      ...error,
      originalCode: error?.code,
    }
  );
}

async function maybeShowAccessibilityReminder() {
  try {
    const store = await loadAccessibilityReminderStore();
    if (!shouldShowAccessibilityReminder(store.lastShownAt)) {
      return false;
    }
    await execFileAsync(
      "/usr/bin/osascript",
      ["-l", "JavaScript", "-e", ACCESSIBILITY_REMINDER_JXA],
      {
        env: {
          ...process.env,
          PSST_GPT_ACCESSIBILITY_REMINDER: JSON.stringify({
            title: "PsstGPT Accessibility Setup",
            message: buildAccessibilityReminderMessage(),
            givingUpAfterSeconds: ACCESSIBILITY_REMINDER_GIVEUP_AFTER_SEC,
          }),
        },
        timeout: ACCESSIBILITY_REMINDER_DIALOG_TIMEOUT_MS,
        maxBuffer: 1024 * 1024,
      }
    );
    await saveAccessibilityReminderStore({
      lastShownAt: new Date().toISOString(),
    });
    return true;
  } catch {
    return false;
  }
}

async function loadAccessibilityReminderStore() {
  const reminderPath = getAccessibilityReminderPath();
  try {
    const raw = await readFile(reminderPath, "utf8");
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === "object") {
      return parsed;
    }
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw codedError(
        "PSST_GPT_ACCESSIBILITY_REMINDER_READ_FAILED",
        "Could not read the PsstGPT Accessibility reminder state.",
        { cause: error }
      );
    }
  }
  return {};
}

async function saveAccessibilityReminderStore(store) {
  const reminderPath = getAccessibilityReminderPath();
  await mkdir(path.dirname(reminderPath), { recursive: true });
  await writeFile(reminderPath, `${JSON.stringify(store, null, 2)}\n`, "utf8");
}

function shouldShowAccessibilityReminder(lastShownAt, now = Date.now()) {
  if (!lastShownAt) {
    return true;
  }
  const lastShownAtMs = new Date(lastShownAt).getTime();
  if (!Number.isFinite(lastShownAtMs)) {
    return true;
  }
  return now - lastShownAtMs >= ACCESSIBILITY_REMINDER_COOLDOWN_MS;
}

async function validateUploadFilePaths(filePaths = []) {
  if (!Array.isArray(filePaths)) {
    throw codedError("PSST_GPT_UPLOAD_FILES_INVALID", "filePaths must be an array.");
  }
  const output = [];
  for (const filePath of filePaths) {
    if (typeof filePath !== "string" || !path.isAbsolute(filePath)) {
      throw codedError(
        "PSST_GPT_UPLOAD_FILE_PATH_INVALID",
        "Each upload file path must be absolute.",
        { filePath }
      );
    }
    try {
      const fileStat = await stat(filePath);
      if (!fileStat.isFile()) {
        throw codedError(
          "PSST_GPT_UPLOAD_FILE_PATH_INVALID",
          `Upload path is not a file: ${filePath}`
        );
      }
      await access(filePath, FS_CONSTANTS.R_OK);
    } catch (error) {
      if (error?.code === "PSST_GPT_UPLOAD_FILE_PATH_INVALID") {
        throw error;
      }
      throw codedError(
        "PSST_GPT_UPLOAD_FILE_PATH_INVALID",
        `Upload path is not readable: ${filePath}`,
        { filePath, cause: error }
      );
    }
    output.push(filePath);
  }
  return output;
}

async function persistUploadAuditResult({ result, bundle }) {
  const responsePath = path.join(bundle.outputDir, "chatgpt-audit-response.md");
  const resultPath = path.join(bundle.outputDir, "chatgpt-audit-result.json");
  const bundlePath = path.join(bundle.outputDir, "upload-bundle.json");
  await writeFile(bundlePath, `${JSON.stringify(bundle, null, 2)}\n`, "utf8");
  await writeFile(responsePath, `${String(result.assistantText ?? "").trimEnd()}\n`, "utf8");
  const enriched = {
    ...result,
    bundle: {
      ...bundle,
      metadataPath: bundlePath,
    },
    responsePath,
    resultPath,
  };
  await writeFile(resultPath, `${JSON.stringify(enriched, null, 2)}\n`, "utf8");
  return enriched;
}

async function persistUploadAuditFailure({
  error,
  bundle,
  requestedPrompt,
  attachmentPaths = [],
  relaySessionId,
}) {
  const responsePath = path.join(bundle.outputDir, "chatgpt-audit-response.md");
  const resultPath = path.join(bundle.outputDir, "chatgpt-audit-result.json");
  const bundlePath = path.join(bundle.outputDir, "upload-bundle.json");
  await writeFile(bundlePath, `${JSON.stringify(bundle, null, 2)}\n`, "utf8");
  const code = error?.code ?? "PSST_GPT_UPLOAD_AUDIT_FAILED";
  const message = error?.message ?? String(error);
  const responseText = [
    "# PsstGPT Upload Audit Failed",
    "",
    `Code: ${code}`,
    "",
    message,
    "",
    "No verified ChatGPT audit response was captured.",
  ].join("\n");
  await writeFile(responsePath, `${responseText}\n`, "utf8");
  const enriched = {
    ok: false,
    status: "failed",
    code,
    message,
    assistantText: "",
    requestedPrompt,
    relaySessionId: relaySessionId ?? null,
    attachmentPaths,
    error: {
      code,
      message,
      details: error?.details ?? null,
      parsed: error?.parsed ?? null,
      stdout: error?.stdout ?? null,
      stderr: error?.stderr ?? null,
      cause: serializeErrorCause(error?.cause),
    },
    bundle: {
      ...bundle,
      metadataPath: bundlePath,
    },
    responsePath,
    resultPath,
  };
  await writeFile(resultPath, `${JSON.stringify(enriched, null, 2)}\n`, "utf8");
  return enriched;
}

function serializeErrorCause(error) {
  if (!error) {
    return null;
  }
  return {
    name: error.name ?? null,
    message: error.message ?? String(error),
    code: error.code ?? null,
    signal: error.signal ?? null,
    killed: error.killed ?? null,
    errno: error.errno ?? null,
    syscall: error.syscall ?? null,
    path: error.path ?? null,
    spawnargs: error.spawnargs ?? null,
    stdout: error.stdout ?? null,
    stderr: error.stderr ?? null,
  };
}

async function normalizeUploadAuditResult({
  result,
  attachmentPaths,
  requireVerification,
  statePath,
  relaySessionId,
}) {
  if (!requireVerification) {
    return result;
  }
  const normalized = normalizeUploadAuditAssistantText(result?.assistantText, attachmentPaths);
  if (relaySessionId) {
    await replaceStoredAssistantText({
      relaySessionId,
      statePath,
      assistantText: normalized.assistantText,
    });
  }
  return rewriteRelayResultAssistantText(result, normalized.assistantText, {
    uploadVerification: normalized.verification,
  });
}

function normalizeUploadAuditAssistantText(assistantText, attachmentPaths = []) {
  const verification = parseUploadVerificationHeader(assistantText, attachmentPaths);
  if (!verification.ok) {
    throw codedError(
      "PSST_GPT_UPLOAD_NOT_VERIFIED",
      verification.message,
      {
        assistantText,
        verification,
      }
    );
  }
  const normalizedAssistantText = normalizeAssistantText(verification.bodyText);
  if (!normalizedAssistantText) {
    throw codedError(
      "PSST_GPT_UPLOAD_NOT_VERIFIED",
      "ChatGPT reported that it inspected the uploaded archive, but the audit body was empty.",
      {
        assistantText,
        verification,
      }
    );
  }
  return {
    assistantText: normalizedAssistantText,
    verification,
  };
}

function parseUploadVerificationHeader(assistantText, attachmentPaths = []) {
  const text = String(assistantText ?? "").replace(/\r/g, "").trim();
  const expectedFiles = dedupe(attachmentPaths.map((filePath) => path.basename(filePath))).filter(Boolean);
  if (!text) {
    return {
      ok: false,
      verified: false,
      header: "",
      bodyText: "",
      expectedFiles,
      message: "ChatGPT returned an empty response after the upload audit.",
    };
  }
  const [firstLineRaw, ...bodyLines] = text.split("\n");
  const firstLine = firstLineRaw.trim();
  if (firstLine.startsWith(UPLOAD_VERIFICATION_FAIL_PREFIX)) {
    const reason = firstLine.slice(UPLOAD_VERIFICATION_FAIL_PREFIX.length).trim();
    return {
      ok: false,
      verified: false,
      header: firstLine,
      bodyText: bodyLines.join("\n"),
      expectedFiles,
      message: reason
        ? `ChatGPT reported that it could not inspect the uploaded archive: ${reason}`
        : "ChatGPT reported that it could not inspect the uploaded archive.",
    };
  }
  if (!firstLine.startsWith(`${UPLOAD_VERIFICATION_OK_PREFIX} `)) {
    return {
      ok: false,
      verified: false,
      header: firstLine,
      bodyText: bodyLines.join("\n"),
      expectedFiles,
      message:
        "The upload audit response did not include the required upload verification header, so PsstGPT cannot prove that ChatGPT inspected the uploaded archive.",
    };
  }
  const verifiedFiles = dedupe(
    firstLine
      .slice(UPLOAD_VERIFICATION_OK_PREFIX.length)
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean)
  );
  if (expectedFiles.length > 0 && expectedFiles.some((fileName) => !verifiedFiles.includes(fileName))) {
    return {
      ok: false,
      verified: false,
      header: firstLine,
      bodyText: bodyLines.join("\n"),
      expectedFiles,
      verifiedFiles,
      message:
        "The upload audit response did not confirm inspection of every uploaded archive file that PsstGPT attached.",
    };
  }
  return {
    ok: true,
    verified: true,
    header: firstLine,
    bodyText: bodyLines.join("\n").trimStart(),
    expectedFiles,
    verifiedFiles,
  };
}

async function replaceStoredAssistantText({ relaySessionId, statePath, assistantText }) {
  const store = await loadAppSessionStore(statePath);
  const index = store.sessions.findIndex((session) => session?.relaySessionId === relaySessionId);
  if (index < 0) {
    return;
  }
  const session = store.sessions[index];
  const messages = Array.isArray(session.messages)
    ? session.messages.map((message) => ({ ...message }))
    : [];
  const lastAssistantIndex = [...messages]
    .reverse()
    .findIndex((message) => message?.role === "assistant");
  if (lastAssistantIndex < 0) {
    messages.push({
      index: messages.length,
      role: "assistant",
      text: assistantText,
    });
  } else {
    const indexFromStart = messages.length - 1 - lastAssistantIndex;
    messages[indexFromStart] = {
      ...messages[indexFromStart],
      text: assistantText,
    };
  }
  store.sessions[index] = {
    ...session,
    messages,
    summary: summarizeMessages(messages),
    keywords: extractKeywords(messages),
    updatedAt: new Date().toISOString(),
  };
  await saveAppSessionStore(statePath, store);
}

function rewriteRelayResultAssistantText(result, assistantText, extra = {}) {
  const relaySessionId = result?.session?.relaySessionId || "";
  const finalDeliveryText = formatAppFinalDeliveryText({
    assistantText,
    relaySessionId,
  });
  const mustReturnFinalDelivery =
    result?.status === "complete" &&
    finalDeliveryText.trim().length > 0;
  return {
    ...result,
    ...extra,
    assistantText,
    finalDeliveryText,
    finalResponseText: finalDeliveryText,
    mustReturnFinalDelivery,
    mustReturnVerbatim: mustReturnFinalDelivery,
  };
}

async function collectAuditFiles(root, { maxFileBytes, maxTotalBytes }) {
  const discovered = [];
  const skipped = [];

  async function walk(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch (error) {
      skipped.push({
        path: path.relative(root, dir) || ".",
        reason: `Could not read directory: ${error?.code ?? error?.message ?? error}`,
      });
      return;
    }

    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      const absolutePath = path.join(dir, entry.name);
      const relativePath = path.relative(root, absolutePath);
      if (/[\r\n]/.test(relativePath)) {
        skipped.push({ path: relativePath, reason: "path contains a newline" });
        continue;
      }
      if (entry.isDirectory()) {
        if (AUDIT_EXCLUDED_DIRS.has(entry.name)) {
          skipped.push({ path: relativePath, reason: "excluded directory" });
          continue;
        }
        const bundleSkipReason = await generatedPsstBundleSkipReason(absolutePath);
        if (bundleSkipReason) {
          skipped.push({ path: relativePath, reason: bundleSkipReason });
          continue;
        }
        await walk(absolutePath);
        continue;
      }
      if (!entry.isFile()) {
        skipped.push({ path: relativePath, reason: "not a regular file" });
        continue;
      }
      if (!isAuditTextFile(relativePath)) {
        skipped.push({ path: relativePath, reason: "non-text extension" });
        continue;
      }
      discovered.push({ absolutePath, relativePath });
    }
  }

  await walk(root);

  const files = [];
  let totalBytes = 0;
  for (const file of discovered) {
    try {
      const fileStat = await stat(file.absolutePath);
      if (fileStat.size > maxFileBytes) {
        skipped.push({
          path: file.relativePath,
          reason: `larger than maxFileBytes (${fileStat.size} > ${maxFileBytes})`,
        });
        continue;
      }
      if (totalBytes + fileStat.size > maxTotalBytes) {
        skipped.push({
          path: file.relativePath,
          reason: `would exceed maxTotalBytes (${totalBytes + fileStat.size} > ${maxTotalBytes})`,
        });
        continue;
      }

      await access(file.absolutePath, FS_CONSTANTS.R_OK);
      const buffer = await readFile(file.absolutePath);
      if (buffer.includes(0)) {
        skipped.push({ path: file.relativePath, reason: "contains NUL byte" });
        continue;
      }
      const text = buffer.toString("utf8").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
      const splitLines = text.split("\n");
      files.push({
        path: file.relativePath,
        bytes: fileStat.size,
        lines: text.endsWith("\n") ? Math.max(0, splitLines.length - 1) : splitLines.length,
        text,
      });
      totalBytes += fileStat.size;
    } catch (error) {
      skipped.push({
        path: file.relativePath,
        reason: `Could not read file: ${error?.code ?? error?.message ?? error}`,
      });
      continue;
    }
  }

  return { files, skipped };
}

function isAuditTextFile(relativePath) {
  const name = path.basename(relativePath);
  return AUDIT_TEXT_FILENAMES.has(name) || AUDIT_TEXT_EXTENSIONS.has(path.extname(name).toLowerCase());
}

function formatAuditBundleMarkdown({
  bundleId,
  createdAt,
  root,
  files,
  skipped,
  maxFileBytes,
  maxTotalBytes,
}) {
  const lines = [
    "# PsstGPT Audit Bundle",
    "",
    `Bundle ID: ${bundleId}`,
    `Created At: ${createdAt}`,
    `Root: ${root}`,
    `Included Files: ${files.length}`,
    `Skipped Entries: ${skipped.length}`,
    `Max File Bytes: ${maxFileBytes}`,
    `Max Total Bytes: ${maxTotalBytes}`,
    "",
    "## Instructions",
    "",
    "Use this bundle as the source of truth for code review or debugging. File contents are line-numbered. Cite findings with the displayed file path and line number.",
    "",
    "## Included Files",
    "",
  ];

  for (const file of files) {
    lines.push(`- ${file.path} (${file.lines} lines, ${file.bytes} bytes)`);
  }

  if (skipped.length > 0) {
    lines.push("", "## Skipped Entries", "");
    for (const item of skipped) {
      lines.push(`- ${item.path}: ${item.reason}`);
    }
  }

  lines.push("", "## Source", "");
  for (const file of files) {
    lines.push(`### ${file.path}`, "", "```text");
    const sourceLines = file.text.split("\n");
    const printableLines = file.text.endsWith("\n") ? sourceLines.slice(0, -1) : sourceLines;
    printableLines.forEach((line, index) => {
      lines.push(`${String(index + 1).padStart(5, " ")}: ${line}`);
    });
    lines.push("```", "");
  }

  return `${lines.join("\n")}\n`;
}

async function relayAuditBundleText({
  bundle,
  bundleText,
  prompt,
  chunkChars = DEFAULT_AUDIT_CHUNK_CHARS,
  tags = [],
  auditAckRetryLimit,
  ...relayOptions
}) {
  const normalizedChunkChars = Math.max(8000, Number(chunkChars) || DEFAULT_AUDIT_CHUNK_CHARS);
  const chunks = chunkTextByLines(bundleText, normalizedChunkChars);
  if (chunks.length === 0) {
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_EMPTY",
      "The generated PsstGPT audit bundle did not contain any text."
    );
  }

  const relaySessionId = relayOptions.relaySessionId || `app-${Date.now()}`;
  const sharedRelayOptions = {
    ...relayOptions,
    relaySessionId,
  };

  if (`${prompt}\n\n${bundleText}`.length <= normalizedChunkChars) {
    const result = await runPsstGPT({
      ...sharedRelayOptions,
      prompt: `${prompt}\n\nBEGIN AUDIT BUNDLE ${bundle.bundleId}\n\n${bundleText}\nEND AUDIT BUNDLE ${bundle.bundleId}`,
      tags,
    });
    return ensureSubstantiveAuditResult({
      result,
      requestedPrompt: prompt,
      bundleId: bundle.bundleId,
      retryLimit: auditAckRetryLimit,
      relayOptions: {
        ...sharedRelayOptions,
        tags: dedupe([...tags, "audit-ack-retry"]),
      },
    });
  }

  await runPsstGPT({
    ...sharedRelayOptions,
    prompt: [
      `You will receive a PsstGPT audit bundle in ${chunks.length} chunks.`,
      "Do not audit until I send FINAL AUDIT REQUEST.",
      "Store the line-numbered source text from each chunk.",
      "Reply exactly: READY",
      "",
      `Bundle ID: ${bundle.bundleId}`,
    ].join("\n"),
    tags,
  });

  for (let index = 0; index < chunks.length; index += 1) {
    await continuePsstGPT({
      ...sharedRelayOptions,
      prompt: [
        `AUDIT BUNDLE CHUNK ${index + 1}/${chunks.length}`,
        `Bundle ID: ${bundle.bundleId}`,
        "",
        chunks[index],
        "",
        `END AUDIT BUNDLE CHUNK ${index + 1}/${chunks.length}`,
        `Reply exactly: ACK ${index + 1}`,
      ].join("\n"),
      tags,
    });
  }

  const result = await continuePsstGPT({
    ...sharedRelayOptions,
    prompt: [
      "FINAL AUDIT REQUEST.",
      `Bundle ID: ${bundle.bundleId}`,
      prompt,
      "Use only the audit bundle chunks already provided in this chat.",
      "Return only the audit.",
    ].join("\n"),
    tags,
  });
  return ensureSubstantiveAuditResult({
    result,
    requestedPrompt: prompt,
    bundleId: bundle.bundleId,
    retryLimit: auditAckRetryLimit,
    relayOptions: {
      ...sharedRelayOptions,
      tags: dedupe([...tags, "audit-ack-retry"]),
    },
  });
}

function looksLikeAuditBundleRecord(bundle) {
  return Boolean(bundle) &&
    typeof bundle === "object" &&
    (
      typeof bundle.markdownPath === "string" ||
      typeof bundle.manifestPath === "string" ||
      typeof bundle.bundleId === "string"
    );
}

async function validateProvidedAuditBundle(bundle) {
  if (!bundle || typeof bundle !== "object") {
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_INVALID",
      "Provided audit bundle must be an object."
    );
  }
  if (typeof bundle.bundleId !== "string" || !bundle.bundleId.trim()) {
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_INVALID",
      "Provided audit bundle is missing bundleId."
    );
  }
  if (typeof bundle.markdownPath !== "string" || !path.isAbsolute(bundle.markdownPath)) {
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_INVALID",
      "Provided audit bundle must include an absolute markdownPath.",
      { bundleId: bundle.bundleId }
    );
  }
  try {
    const markdownStat = await stat(bundle.markdownPath);
    if (!markdownStat.isFile()) {
      throw codedError(
        "PSST_GPT_AUDIT_BUNDLE_INVALID",
        `Provided audit bundle markdownPath is not a file: ${bundle.markdownPath}`,
        { bundleId: bundle.bundleId }
      );
    }
    await access(bundle.markdownPath, FS_CONSTANTS.R_OK);
  } catch (error) {
    if (error?.code === "PSST_GPT_AUDIT_BUNDLE_INVALID") {
      throw error;
    }
    throw codedError(
      "PSST_GPT_AUDIT_BUNDLE_INVALID",
      `Provided audit bundle markdownPath is not readable: ${bundle.markdownPath}`,
      { bundleId: bundle.bundleId, cause: error }
    );
  }
  return bundle;
}

async function validateProvidedUploadBundle(bundle) {
  if (!bundle || typeof bundle !== "object") {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      "Provided upload bundle must be an object."
    );
  }
  if (typeof bundle.bundleId !== "string" || !bundle.bundleId.trim()) {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      "Provided upload bundle is missing bundleId."
    );
  }
  if (typeof bundle.outputDir !== "string" || !path.isAbsolute(bundle.outputDir)) {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      "Provided upload bundle must include an absolute outputDir.",
      { bundleId: bundle.bundleId }
    );
  }
  if (
    typeof bundle.root === "string" &&
    path.isAbsolute(bundle.root) &&
    path.resolve(bundle.root) === path.resolve(bundle.outputDir)
  ) {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      "Provided upload bundle outputDir must not be the same directory as the bundle root.",
      { bundleId: bundle.bundleId }
    );
  }
  try {
    const outputDirStat = await stat(bundle.outputDir);
    if (!outputDirStat.isDirectory()) {
      throw codedError(
        "PSST_GPT_UPLOAD_BUNDLE_INVALID",
        `Provided upload bundle outputDir is not a directory: ${bundle.outputDir}`,
        { bundleId: bundle.bundleId }
      );
    }
    await access(bundle.outputDir, FS_CONSTANTS.R_OK | FS_CONSTANTS.W_OK);
  } catch (error) {
    if (error?.code === "PSST_GPT_UPLOAD_BUNDLE_INVALID") {
      throw error;
    }
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      `Provided upload bundle outputDir is not readable and writable: ${bundle.outputDir}`,
      { bundleId: bundle.bundleId, cause: error }
    );
  }

  const candidateAttachmentPaths = Array.isArray(bundle.attachmentPaths) && bundle.attachmentPaths.length > 0
    ? bundle.attachmentPaths
    : [bundle.archivePath, ...(Array.isArray(bundle.shards) ? bundle.shards.map((shard) => shard?.path) : [])]
      .filter(Boolean);
  if (candidateAttachmentPaths.length === 0) {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      "Provided upload bundle did not include any attachment file paths.",
      { bundleId: bundle.bundleId }
    );
  }

  const attachmentPaths = await validateUploadFilePaths(candidateAttachmentPaths).catch((error) => {
    throw codedError(
      "PSST_GPT_UPLOAD_BUNDLE_INVALID",
      error?.message || "Provided upload bundle attachment paths are invalid.",
      { bundleId: bundle.bundleId, cause: error }
    );
  });

  return {
    ...bundle,
    attachmentPaths,
  };
}

async function ensureBundleOutputDir({
  outputDir,
  fallbackDir,
  invalidCode,
  invalidLabel,
  disallowedPath,
  disallowedMessage,
}) {
  const bundleDir = outputDir ? path.resolve(outputDir) : fallbackDir;
  let bundleDirExisted = false;

  if (disallowedPath && path.resolve(bundleDir) === path.resolve(disallowedPath)) {
    throw codedError(
      invalidCode,
      disallowedMessage || `Provided ${invalidLabel} is not allowed: ${bundleDir}`,
      { outputDir: bundleDir }
    );
  }

  try {
    const bundleDirStat = await stat(bundleDir);
    if (!bundleDirStat.isDirectory()) {
      throw codedError(
        invalidCode,
        `Provided ${invalidLabel} is not a directory: ${bundleDir}`,
        { outputDir: bundleDir }
      );
    }
    await access(bundleDir, FS_CONSTANTS.R_OK | FS_CONSTANTS.W_OK);
    bundleDirExisted = true;
  } catch (error) {
    if (error?.code === "ENOENT") {
      bundleDirExisted = false;
    } else if (error?.code === invalidCode) {
      throw error;
    } else {
      throw codedError(
        invalidCode,
        `Provided ${invalidLabel} is not readable and writable: ${bundleDir}`,
        { outputDir: bundleDir, cause: error }
      );
    }
  }

  if (!bundleDirExisted) {
    try {
      await mkdir(bundleDir, { recursive: true });
    } catch (error) {
      throw codedError(
        invalidCode,
        `Could not create ${invalidLabel}: ${bundleDir}`,
        { outputDir: bundleDir, cause: error }
      );
    }
  }

  return { bundleDir, bundleDirExisted };
}

async function writeBundleMarker(bundleDir, marker = {}) {
  const markerPath = path.join(bundleDir, PSST_BUNDLE_MARKER_FILENAME);
  await writeFile(markerPath, `${JSON.stringify({
    surface: APP_SURFACE,
    ...marker,
  }, null, 2)}\n`, "utf8");
}

async function generatedPsstBundleSkipReason(directoryPath) {
  try {
    const entries = await readdir(directoryPath, { withFileTypes: true });
    const names = new Set(entries.map((entry) => entry.name));
    if (names.has(PSST_BUNDLE_MARKER_FILENAME)) {
      return "generated PsstGPT bundle directory";
    }
    if (names.has("audit-bundle.md") && names.has("manifest.json")) {
      return "legacy PsstGPT audit bundle directory";
    }
    if (
      names.has("source-archive.zip") &&
      (
        names.has("upload-bundle.json") ||
        names.has("chatgpt-audit-response.md") ||
        names.has("chatgpt-audit-result.json")
      )
    ) {
      return "legacy PsstGPT upload bundle directory";
    }
  } catch {
    return "";
  }
  return "";
}

function chunkTextByLines(text, maxChars) {
  const lines = String(text ?? "").split("\n");
  const chunks = [];
  let current = "";

  for (const line of lines) {
    const next = current ? `${current}\n${line}` : line;
    if (current && next.length > maxChars) {
      chunks.push(current);
      current = line;
    } else {
      current = next;
    }
  }

  if (current) {
    chunks.push(current);
  }
  return chunks;
}

function chunkArray(values, size) {
  const chunks = [];
  for (let index = 0; index < values.length; index += size) {
    chunks.push(values.slice(index, index + size));
  }
  return chunks;
}

async function ensureSubstantiveAuditResult({
  result,
  requestedPrompt,
  bundleId,
  relayOptions = {},
  retryLimit = DEFAULT_AUDIT_ACK_RETRY_LIMIT,
} = {}) {
  const normalizedRetryLimit = Math.max(0, Math.min(5, Number(retryLimit) || 0));
  if (
    !result ||
    result.status !== "complete" ||
    normalizedRetryLimit === 0 ||
    isExactOutputRequest(requestedPrompt) ||
    !isLikelyAcknowledgementOnlyAuditResponse(result.assistantText)
  ) {
    return result;
  }

  let latest = result;
  for (let attempt = 1; attempt <= normalizedRetryLimit; attempt += 1) {
    const continueFn = relayOptions?.useUploadRetryRelay === true
      ? continuePsstGPTAfterUpload
      : continuePsstGPT;
    const commonRelayOptions = {
      ...relayOptions,
      prompt: buildAuditRetryPrompt({
        bundleId,
        requestedPrompt,
        attempt,
      }),
    };
    if (relayOptions?.useUploadRetryRelay !== true) {
      commonRelayOptions.background = true;
    }
    latest = await continueFn(commonRelayOptions);
    if (
      latest.status !== "complete" ||
      !isLikelyAcknowledgementOnlyAuditResponse(latest.assistantText)
    ) {
      return latest;
    }
  }
  return latest;
}

function buildAuditRetryPrompt({ bundleId, requestedPrompt, attempt } = {}) {
  return [
    "The previous answer only acknowledged the audit request.",
    "Do the audit now in this message.",
    bundleId ? `Bundle ID: ${bundleId}` : "",
    "Use only the already provided PsstGPT bundle or uploaded files.",
    "Return confirmed findings ordered by severity with exact file and line citations when findings exist.",
    "If there are no confirmed correctness bugs, say that clearly and list residual risks or missing tests.",
    "Do not acknowledge, promise, plan, or say you will audit later.",
    attempt ? `Retry attempt: ${attempt}` : "",
    requestedPrompt ? `Original audit request:\n${String(requestedPrompt).trim()}` : "",
  ].filter(Boolean).join("\n");
}

function isExactOutputRequest(text = "") {
  const normalized = normalizeWhitespace(text).toLowerCase();
  const userRequestMatch = normalized.match(/\buser request:\s*(.*)$/);
  const requestText = userRequestMatch?.[1] ?? normalized;
  return /(?:^|[\s.;:!?])(?:reply|return|output|print|respond)\s+exactly\b/.test(requestText) ||
    /\bno other text\b/.test(requestText) ||
    /\bexact(?: output|string| format)\b/.test(requestText);
}

function isSubstantiveAuditFinalResponse(text = "") {
  const normalized = normalizeWhitespace(text).toLowerCase();
  if (
    !normalized ||
    isLikelyAcknowledgementOnlyAuditResponse(text) ||
    (
      /\b(?:am|still)\s+(?:auditing|reviewing|inspecting|analy[sz]ing)\b/.test(normalized) &&
      !/\b(?:no confirmed|finding|findings|bug|bugs|issue|issues|risk|risks|src\/|test\/|:\d+)\b/.test(normalized)
    )
  ) {
    return false;
  }
  if (normalized.startsWith(UPLOAD_VERIFICATION_FAIL_PREFIX.toLowerCase())) {
    return true;
  }
  if (/\bno confirmed (?:correctness )?(?:bugs|issues|findings)\b/.test(normalized)) {
    return true;
  }
  if (/\b(?:finding|findings|bug|bugs|issue|issues|risk|risks|severity|minimal fix|failure mode)\b/.test(normalized)) {
    return true;
  }
  if (/(?:^|\s)(?:src|test|docs|plugins|scripts|validation)\/\S+:\d+\b/.test(normalized)) {
    return true;
  }
  return normalized.length >= 1200;
}

function isLikelyAcknowledgementOnlyAuditResponse(text = "") {
  const normalized = normalizeWhitespace(text).toLowerCase();
  if (!normalized || normalized.length > 700) {
    return false;
  }
  if (/^(?:ready|ack(?:\s+\d+)?|okay|ok|understood|got it|noted|sure)\.?$/.test(normalized)) {
    return true;
  }
  if (/^(?:sure|okay|ok|understood|got it|noted)[,!. ]+(?:i|i'll|i’ll|i will|will)\b/.test(normalized)) {
    return true;
  }
  if (/^(?:i(?:'ll|’ll| will| can| am going to)|we(?:'ll|’ll| will| can))\b/.test(normalized)) {
    return /\b(?:audit|review|inspect|analy[sz]e|check|report|look at|use only|provided bundle|bundle chunks|uploaded files)\b/.test(normalized);
  }
  if (
    /\bi(?:'ve| have)?\s+(?:inspected|unpacked|loaded|read|opened|extracted)\b/.test(normalized) &&
    /\b(?:am auditing|am reviewing|will prioritize|will provide|will report|cannot run|can't run|can not run)\b/.test(normalized) &&
    !/\b(?:no confirmed|found|confirmed finding|bug at|issue at|src\/|test\/|:\d+|line \d+)\b/.test(normalized)
  ) {
    return true;
  }
  return /\b(?:i(?:'ll|’ll| will)|we(?:'ll|’ll| will))\b.*\b(?:audit|review|inspect|analy[sz]e|report)\b/.test(normalized) &&
    !/\b(?:finding|findings|bug|bugs|issue|issues|risk|risks|line|lines|src\/|test\/|readme|no confirmed)\b/.test(normalized);
}

async function relayPromptToChatGPTApp(options = {}) {
  const {
    prompt,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    waitChunkMs = DEFAULT_WAIT_CHUNK_MS,
    returnPending = false,
    statePath,
    newChat = true,
    tags = [],
    relaySessionId,
    background = DEFAULT_BACKGROUND,
    returnAfterSend = false,
  } = options;

  if (typeof prompt !== "string" || !prompt.trim()) {
    throw codedError("PROMPT_MISSING", "A non-empty prompt is required.");
  }

  assertSupportedAppRelayOptions(options);
  await ensureChatGPTAppReady({ background, verify: false });

  const promptText = prompt.trim();
  const existingRecord = relaySessionId
    ? await findStoredAppSessionByRelayId(relaySessionId, statePath)
    : null;
  const baseMessages = messagesForAppRelay(promptText, "", existingRecord?.messages);
  const initialState = await runJxa("sendPrompt", {
    prompt: promptText,
    newChat: newChat !== false,
    background,
  });
  assertNoFatalAppBlocker(initialState);

  const pendingRecord = await upsertAppSessionRecord({
    statePath,
    relaySessionId,
    prompt: promptText,
    title: initialState.title,
    mode: initialState.visibleModelLabel,
    background,
    status: "pending",
    messages: baseMessages,
    tags,
  });

  if (returnAfterSend) {
    return appRelayResult({
      status: "pending",
      assistantText: "",
      state: initialState,
      record: pendingRecord,
    });
  }

  const result = await waitForAppAssistantResponseInChunks({
    prompt: promptText,
    timeoutMs,
    waitChunkMs,
    allowPending: returnPending,
    background,
    initialState,
    onPending: async (pendingResult) => {
      await upsertAppSessionRecord({
        statePath,
        relaySessionId: pendingRecord.relaySessionId,
        prompt: promptText,
        title: pendingResult.state.title,
        mode: pendingResult.state.visibleModelLabel || pendingRecord.mode,
        background,
        status: pendingResult.status,
        messages: messagesForAppRelay(promptText, pendingResult.assistantText, baseMessages),
        tags,
      });
    },
  });
  const messages = messagesForAppRelay(promptText, result.assistantText, baseMessages);
  const record = await upsertAppSessionRecord({
    statePath,
    relaySessionId: pendingRecord.relaySessionId,
    prompt: promptText,
    title: result.state.title,
    mode: result.state.visibleModelLabel || pendingRecord.mode,
    background,
    status: result.status,
    messages,
    tags,
  });

  return appRelayResult({
    status: result.status,
    assistantText: result.assistantText,
    state: result.state,
    record,
  });
}

function assertSupportedAppRelayOptions(options = {}) {
  const unsupported = [];
  if ((options.filePaths ?? []).length > 0 || (options.attachments ?? []).length > 0) {
    unsupported.push("attachments");
  }
  if (options.feature) unsupported.push("feature");
  if (options.projectName) unsupported.push("projectName");
  if (options.appName) unsupported.push("appName");
  if (options.conversationUrl) unsupported.push("conversationUrl");
  if (options.background === false) unsupported.push("foreground mode");
  if (options.allowWindowRecovery === true) unsupported.push("window recovery");
  if (hasExplicitIntelligenceOption(options)) {
    unsupported.push("model/mode/effort selection");
  }

  if (unsupported.length > 0) {
    throw codedError(
      "PSST_GPT_UNSUPPORTED_OPTION",
      `PsstGPT supports verified text prompt relay through the active ChatGPT app only. Unsupported option(s): ${unsupported.join(", ")}.`,
      { unsupported }
    );
  }
}

function assertStrictBackgroundOptions(options = {}) {
  if (options.background === false) {
    throw codedError(
      "PSST_GPT_FOREGROUND_DISABLED",
      "PsstGPT is strict-background only. It will not activate ChatGPT or steal focus."
    );
  }
  if (options.allowWindowRecovery === true) {
    throw codedError(
      "PSST_GPT_WINDOW_RECOVERY_DISABLED",
      "PsstGPT will not open or recover ChatGPT windows because that can interrupt other work. Open a ChatGPT app window manually before starting background relay."
    );
  }
}

function hasExplicitIntelligenceOption(options = {}) {
  return [
    options.model,
    options.mode,
    options.intelligenceMode,
    options.reasoningMode,
    options.thinkingMode,
    options.reasoningEffort,
    options.thinkingEffort,
    options.proEffort,
    options.effort,
  ].some((value) => value !== undefined && value !== null && String(value).trim());
}

async function ensureChatGPTAppReady({
  background = DEFAULT_BACKGROUND,
  verify = true,
  allowWindowRecovery = false,
  returnState = false,
  restoreFrontmostOnExit = false,
} = {}) {
  if (process.platform !== "darwin") {
    throw codedError(
      "PSST_GPT_UNSUPPORTED_PLATFORM",
      "PsstGPT currently supports macOS only."
    );
  }

  await assertChatGPTAppInstalled();

  let launched = false;
  let lastLaunchError;
  for (const bundleId of APP_BUNDLE_IDS) {
    try {
      await execFileAsync("/usr/bin/open", ["-g", "-b", bundleId], {
        timeout: 10000,
        maxBuffer: 1024 * 1024,
      });
      launched = true;
      break;
    } catch (error) {
      lastLaunchError = error;
    }
  }
  if (!launched) {
    throw codedError(
      "PSST_GPT_LAUNCH_FAILED",
      "Could not launch the ChatGPT desktop app.",
      { cause: lastLaunchError }
    );
  }

  if (verify) {
    const readyState = await runJxa("waitReady", {
      background,
      allowWindowRecovery,
      restoreFrontmostOnExit,
    });
    if (returnState) {
      return readyState;
    }
  }

  return undefined;
}

async function ensureStrictBackgroundRelayReady({ ensureReady = ensureChatGPTAppReady } = {}) {
  await ensureReady({
    background: true,
    verify: true,
    allowWindowRecovery: false,
  });
}

async function ensureForegroundUploadRelayReady({
  probe = probeDirectAxUploadRelayReady,
  restoreFrontmostOnExit = true,
} = {}) {
  return probe({ restoreFrontmostOnExit });
}

async function assertChatGPTAppInstalled() {
  const candidatePaths = [
    "/Applications/ChatGPT.app",
    path.join(os.homedir(), "Applications", "ChatGPT.app"),
  ];

  for (const candidate of candidatePaths) {
    try {
      await access(candidate);
      return;
    } catch {
      // Try the next standard install location.
    }
  }

  throw codedError(
    "PSST_GPT_NOT_INSTALLED",
    "Could not find ChatGPT.app in /Applications or ~/Applications. Install it from https://chatgpt.com/download/."
  );
}

async function waitForAppAssistantResponseInChunks({
  prompt,
  timeoutMs,
  waitChunkMs,
  allowPending,
  background,
  allowForegroundRecovery,
  responseStartTimeoutMs,
  isFinalResponseAcceptable,
  initialState,
  onPending,
}) {
  const normalizedTimeoutMs = normalizeOverallTimeoutMs(timeoutMs, DEFAULT_TIMEOUT_MS);
  const normalizedWaitChunkMs = normalizePositiveDurationMs(waitChunkMs, DEFAULT_WAIT_CHUNK_MS);
  const normalizedResponseStartTimeoutMs = normalizeChunkedResponseStartTimeoutMs(
    responseStartTimeoutMs,
    {
      overallTimeoutMs: timeoutMs,
      fallback: DEFAULT_RESPONSE_START_TIMEOUT_MS,
    }
  );
  const started = Date.now();
  let progress = createAssistantWaitProgress(started);
  if (initialState && typeof initialState === "object") {
    progress = advanceAssistantWaitProgress(progress, initialState, prompt, started);
  }
  let effectiveBackground = background !== false;
  let lastPending = null;

  while (normalizedTimeoutMs === 0 || Date.now() - started < normalizedTimeoutMs) {
    const elapsedMs = Date.now() - started;
    const remainingMs = normalizedTimeoutMs === 0 ? normalizedWaitChunkMs : normalizedTimeoutMs - elapsedMs;
    const chunkMs = Math.max(1, Math.min(normalizedWaitChunkMs, remainingMs));
    const result = await waitForAppAssistantResponse({
      prompt,
      timeoutMs: chunkMs,
      allowPending: true,
      background: effectiveBackground,
      allowForegroundRecovery,
      responseStartTimeoutMs: normalizedResponseStartTimeoutMs,
      isFinalResponseAcceptable,
      progress,
    });
    progress = result.progress ?? progress;
    effectiveBackground = result.background !== false;

    if (result.status === "complete") {
      return result;
    }

    lastPending = result;
    await onPending?.(result);

    if (allowPending) {
      return result;
    }
  }

  if (allowPending && lastPending) {
    return lastPending;
  }

  throw codedError(
    "PSST_GPT_RESPONSE_TIMEOUT",
    "ChatGPT app did not finish answering before the timeout.",
    { lastState: lastPending?.state ?? null }
  );
}

async function waitForAppAssistantResponse({
  prompt,
  timeoutMs,
  allowPending,
  background,
  allowForegroundRecovery = false,
  responseStartTimeoutMs,
  isFinalResponseAcceptable,
  progress = createAssistantWaitProgress(),
}) {
  const normalizedTimeoutMs = normalizeOverallTimeoutMs(timeoutMs, DEFAULT_TIMEOUT_MS);
  const normalizedResponseStartTimeoutMs = normalizeResponseStartTimeoutMs(
    responseStartTimeoutMs,
    {
      overallTimeoutMs: timeoutMs,
      fallback: DEFAULT_RESPONSE_START_TIMEOUT_MS,
    }
  );
  const start = Date.now();
  let lastState = null;
  let currentProgress = normalizeAssistantWaitProgress(progress, start);
  let effectiveBackground = background !== false;

  while (normalizedTimeoutMs === 0 || Date.now() - start < normalizedTimeoutMs) {
    await sleep(POLL_INTERVAL_MS);
    let state;
    try {
      state = await readStateForAssistantWait({
        background: effectiveBackground,
        allowForegroundRecovery,
      });
    } catch (error) {
      if (!shouldAttemptForegroundRecoveryForWait({
        allowForegroundRecovery,
        background: effectiveBackground,
        error,
      })) {
        throw error;
      }
      effectiveBackground = false;
      state = await readStateForAssistantWait({
        background: false,
        allowForegroundRecovery: true,
      });
    }
    assertNoFatalAppBlocker(state);
    lastState = state;
    currentProgress = advanceAssistantWaitProgress(currentProgress, state, prompt, Date.now());
    const captureState = currentProgress.captureState;
    const stableForMs = assistantWaitProgressStableForMs(currentProgress, Date.now());
    const controls = appResponseControls(state);
    if (shouldFailResponseStart({
      responseStartedEver: currentProgress.responseStartedEver,
      state,
      prompt,
      captureState,
      stableForMs,
      responseStartTimeoutMs: normalizedResponseStartTimeoutMs,
    })) {
      throw codedError(
        "PSST_GPT_RESPONSE_NOT_STARTED",
        "ChatGPT accepted the prompt but never started answering.",
        {
          captureState,
          lastState: state,
        }
      );
    }

    if (
      captureState.incomplete &&
      controls.ended &&
      currentProgress.consecutiveEndedObservations >= 2 &&
      stableForMs >= RESPONSE_STABLE_MS
    ) {
      throw codedError(
        "PSST_GPT_RESPONSE_CAPTURE_INCOMPLETE",
        captureState.incompleteReason ||
          "PsstGPT could not verify that it captured the full assistant response from the ChatGPT app.",
        {
          captureState,
          lastState: state,
        }
      );
    }

    if (isAppResponseCompleteSnapshot({
      assistantText: captureState.assistantText,
      textStableForMs: stableForMs,
      isAnswering: controls.active,
      sendReady: controls.ended,
      endedObservations: currentProgress.consecutiveEndedObservations,
      captureState,
    })) {
      let finalAssistantText = captureState.assistantText;
      if (allowForegroundRecovery === true) {
        const copied = await copyDirectAxCurrentAssistant({
          baselineCopyMessageCount: currentProgress.baselineCopyMessageCount ?? 0,
          prompt,
          restoreFrontmostOnExit: true,
        });
        if (copied) finalAssistantText = copied;
      }
      if (
        typeof isFinalResponseAcceptable === "function" &&
        !isFinalResponseAcceptable(finalAssistantText)
      ) {
        return {
          status: "pending",
          assistantText: finalAssistantText,
          state,
          background: effectiveBackground,
          progress: currentProgress,
          pendingReason: "assistant response was stable but not an acceptable final answer",
        };
      }
      return {
        status: "complete",
        assistantText: finalAssistantText,
        state,
        background: effectiveBackground,
        progress: currentProgress,
      };
    }
  }

  if (allowPending) {
    return {
      status: "pending",
      assistantText: currentProgress.captureState.assistantText,
      state: lastState,
      background: effectiveBackground,
      progress: currentProgress,
    };
  }

  throw codedError(
    "PSST_GPT_RESPONSE_TIMEOUT",
    "ChatGPT app did not finish answering before the timeout.",
    { lastState }
  );
}

async function readStateForAssistantWait({
  background,
  allowForegroundRecovery = false,
} = {}) {
  if (shouldUseDirectAxStateReaderForWait({ background, allowForegroundRecovery })) {
    return readDirectAxPsstGPTState({ restoreFrontmostOnExit: true });
  }
  return readPsstGPTState({
    background,
    ensure: false,
    allowForeground: background === false,
  });
}

function isAppResponseCompleteSnapshot({
  assistantText,
  textStableForMs,
  isAnswering,
  sendReady,
  endedObservations,
  captureState,
}) {
  return Boolean(
    assistantText?.trim() &&
    !isAppTransientText(assistantText) &&
    textStableForMs >= RESPONSE_STABLE_MS &&
    !isAnswering &&
    sendReady === true &&
    endedObservations >= 2 &&
    assistantCaptureCanComplete(captureState)
  );
}

function appResponseControls(state = {}) {
  const labels = Array.isArray(state.buttonLabels)
    ? state.buttonLabels.map((label) => String(label ?? ""))
    : [];
  const stopPresent = state.stopPresent === true || state.isAnswering === true ||
    labels.some((label) => /\b(stop|cancel)\b/i.test(label));
  const sendPresent = state.sendReady === true || state.sendPresent === true ||
    labels.some((label) => /\b(send|submit)\b/i.test(label));
  return {
    active: stopPresent,
    ended: sendPresent && !stopPresent,
  };
}

function responseAcceptedFromAppState(state = {}, prompt = "", captureState = createAssistantCaptureState()) {
  if (captureState.promptVisibleEver) {
    return true;
  }
  const composerValue = normalizeWhitespace(String(state.composerValue ?? ""));
  const normalizedPrompt = normalizeWhitespace(String(prompt ?? ""));
  if (!composerValue) {
    return true;
  }
  return composerValue !== normalizedPrompt;
}

function shouldFailResponseStart({
  responseStartedEver,
  state,
  prompt,
  captureState,
  stableForMs,
  responseStartTimeoutMs,
} = {}) {
  return !responseStartedEver &&
    responseStartTimeoutMs > 0 &&
    responseAcceptedFromAppState(state, prompt, captureState) &&
    !captureState.incomplete &&
    stableForMs >= responseStartTimeoutMs;
}

function isRecoverableBackgroundAppStateError(error = {}) {
  return error?.code === "PSST_GPT_WINDOW_SHELL_ONLY_BACKGROUND" ||
    error?.code === "PSST_GPT_WINDOW_MISSING_BACKGROUND";
}

function shouldAttemptForegroundRecoveryForWait({
  allowForegroundRecovery = false,
  background = true,
  error,
} = {}) {
  return allowForegroundRecovery === true &&
    background !== false &&
    isRecoverableBackgroundAppStateError(error);
}

function shouldUseDirectAxStateReaderForWait({
  background = true,
  allowForegroundRecovery = false,
} = {}) {
  return allowForegroundRecovery === true && background === false;
}

function extractAssistantTextFromAppState(state = {}, prompt = "") {
  return extractAssistantCaptureSnapshot(state, prompt).assistantText;
}

function extractAssistantCaptureSnapshot(state = {}, prompt = "") {
  const promptNeedle = normalizeForMatch(prompt);
  const transcript = Array.isArray(state.transcriptTexts)
    ? state.transcriptTexts
    : [];
  let promptIndex = -1;

  for (let index = transcript.length - 1; index >= 0; index -= 1) {
    const text = normalizeForMatch(transcript[index]?.text ?? transcript[index]);
    if (!text) {
      continue;
    }
    if (
      text === promptNeedle ||
      (promptNeedle.length >= 80 && text.includes(promptNeedle.slice(0, 80))) ||
      (text.length >= 80 && promptNeedle.includes(text.slice(0, 80)))
    ) {
      promptIndex = index;
      break;
    }
  }

  const candidates = (promptIndex >= 0 ? transcript.slice(promptIndex + 1) : transcript)
    .map((entry) => String(entry?.text ?? entry ?? "").trim())
    .filter(Boolean)
    .filter((text) => !isAppUiText(text))
    .filter((text) => normalizeForMatch(text) !== promptNeedle)
    .filter((text) => !isAppTransientText(text));

  return {
    promptVisible: promptIndex >= 0,
    assistantText: normalizeAssistantText(dedupeAdjacent(candidates).join("\n")),
  };
}

function createAssistantCaptureState() {
  return {
    assistantText: "",
    promptVisibleEver: false,
    promptVisibleNow: false,
    incomplete: false,
    incompleteReason: "",
    lastVisibleAssistantText: "",
  };
}

function createAssistantWaitProgress(now = Date.now()) {
  return {
    captureState: createAssistantCaptureState(),
    lastChangedAt: Number.isFinite(Number(now)) ? Math.floor(Number(now)) : Date.now(),
    responseStartedEver: false,
    consecutiveEndedObservations: 0,
    baselineCopyMessageCount: null,
  };
}

function normalizeAssistantWaitProgress(progress = {}, now = Date.now()) {
  const fallbackNow = Number.isFinite(Number(now)) ? Math.floor(Number(now)) : Date.now();
  const captureState = {
    ...createAssistantCaptureState(),
    ...(progress.captureState ?? {}),
  };
  const lastChangedAt = Number.isFinite(Number(progress.lastChangedAt))
    ? Math.floor(Number(progress.lastChangedAt))
    : fallbackNow;
  return {
    captureState,
    lastChangedAt,
    responseStartedEver: progress.responseStartedEver === true || Boolean(captureState.assistantText),
    consecutiveEndedObservations: Number.isInteger(progress.consecutiveEndedObservations)
      ? Math.max(0, progress.consecutiveEndedObservations)
      : 0,
    baselineCopyMessageCount: Number.isInteger(progress.baselineCopyMessageCount)
      ? Math.max(0, progress.baselineCopyMessageCount)
      : null,
  };
}

function advanceAssistantWaitProgress(progress = {}, state = {}, prompt = "", now = Date.now()) {
  const current = normalizeAssistantWaitProgress(progress, now);
  const controls = appResponseControls(state);
  const nextCaptureState = advanceAssistantCaptureState(
    current.captureState,
    extractAssistantCaptureSnapshot(state, prompt)
  );
  return {
    captureState: nextCaptureState,
    lastChangedAt: didAssistantCaptureStateChange(current.captureState, nextCaptureState)
      ? Number.isFinite(Number(now)) ? Math.floor(Number(now)) : Date.now()
      : current.lastChangedAt,
    responseStartedEver:
      current.responseStartedEver ||
      controls.active ||
      Boolean(nextCaptureState.assistantText),
    consecutiveEndedObservations: controls.ended
      ? current.consecutiveEndedObservations + 1
      : 0,
    baselineCopyMessageCount: current.baselineCopyMessageCount ??
      (Number.isInteger(state.copyMessageCount) ? Math.max(0, state.copyMessageCount) : null),
  };
}

function assistantWaitProgressStableForMs(progress = {}, now = Date.now()) {
  const current = normalizeAssistantWaitProgress(progress, now);
  const currentNow = Number.isFinite(Number(now)) ? Math.floor(Number(now)) : Date.now();
  return Math.max(0, currentNow - current.lastChangedAt);
}

function advanceAssistantCaptureState(currentState = createAssistantCaptureState(), snapshot = {}) {
  const nextState = {
    ...createAssistantCaptureState(),
    ...currentState,
  };
  const promptVisible = snapshot.promptVisible === true;
  const visibleAssistantText = normalizeAssistantText(snapshot.assistantText);
  nextState.promptVisibleNow = promptVisible;
  if (visibleAssistantText) {
    nextState.lastVisibleAssistantText = visibleAssistantText;
  }

  if (promptVisible) {
    nextState.promptVisibleEver = true;
  }
  if (!visibleAssistantText) {
    return nextState;
  }

  if (promptVisible) {
    if (
      !nextState.assistantText ||
      visibleAssistantText.length >= nextState.assistantText.length ||
      visibleAssistantText.includes(nextState.assistantText)
    ) {
      nextState.assistantText = visibleAssistantText;
    } else if (!nextState.assistantText.includes(visibleAssistantText)) {
      nextState.assistantText = visibleAssistantText;
    }
    nextState.incomplete = false;
    nextState.incompleteReason = "";
    return nextState;
  }

  if (!nextState.assistantText) {
    nextState.incomplete = true;
    nextState.incompleteReason =
      "The active prompt scrolled out of the visible ChatGPT transcript before any assistant text was captured.";
    return nextState;
  }

  const merged = mergeAssistantTails(nextState.assistantText, visibleAssistantText);
  if (merged.ok) {
    nextState.assistantText = merged.text;
    nextState.incomplete = false;
    nextState.incompleteReason = "";
    return nextState;
  }

  nextState.incomplete = true;
  nextState.incompleteReason =
    "The assistant response grew beyond the visible ChatGPT transcript and the newly visible tail could not be aligned with the previously captured text.";
  return nextState;
}

function mergeAssistantTails(previousText = "", currentVisibleText = "") {
  const previous = normalizeAssistantText(previousText);
  const current = normalizeAssistantText(currentVisibleText);
  if (!current) {
    return { ok: true, text: previous, strategy: "empty-current" };
  }
  if (!previous) {
    return { ok: false, reason: "no-prior-capture" };
  }
  if (current === previous || previous.endsWith(current) || previous.includes(current)) {
    return { ok: true, text: previous, strategy: "current-within-previous" };
  }
  if (current.includes(previous)) {
    return { ok: true, text: current, strategy: "previous-within-current" };
  }
  const maxOverlap = Math.min(previous.length, current.length);
  const minOverlap = minimumAssistantTailOverlap(previous, current);
  for (let length = maxOverlap; length >= minOverlap; length -= 1) {
    if (previous.slice(-length) === current.slice(0, length)) {
      return {
        ok: true,
        text: normalizeAssistantText(previous + current.slice(length)),
        strategy: "suffix-prefix-overlap",
        overlapLength: length,
      };
    }
  }
  return {
    ok: false,
    reason: "tail-could-not-be-aligned",
  };
}

function minimumAssistantTailOverlap(previousText = "", currentText = "") {
  const shortest = Math.min(previousText.length, currentText.length);
  if (shortest <= 24) {
    return shortest;
  }
  if (shortest <= 80) {
    return Math.max(12, Math.floor(shortest / 2));
  }
  return Math.max(24, Math.floor(shortest * 0.2));
}

function assistantCaptureCanComplete(captureState = {}) {
  if (!captureState || Object.keys(captureState).length === 0) {
    return true;
  }
  return Boolean(
    captureState?.assistantText?.trim() &&
    captureState?.promptVisibleEver &&
    !captureState?.incomplete
  );
}

function didAssistantCaptureStateChange(previousState = {}, nextState = {}) {
  return previousState.assistantText !== nextState.assistantText ||
    previousState.promptVisibleEver !== nextState.promptVisibleEver ||
    previousState.promptVisibleNow !== nextState.promptVisibleNow ||
    previousState.incomplete !== nextState.incomplete ||
    previousState.incompleteReason !== nextState.incompleteReason;
}

function transcriptContainsPrompt(state = {}, prompt = "") {
  const promptNeedle = normalizeForMatch(prompt);
  return (state.transcriptTexts ?? []).some((entry) => {
    const text = normalizeForMatch(entry?.text ?? entry);
    return text === promptNeedle ||
      (promptNeedle.length >= 80 && text.includes(promptNeedle.slice(0, 80))) ||
      (text.length >= 80 && promptNeedle.includes(text.slice(0, 80)));
  });
}

function isAppUiText(text = "") {
  const normalized = normalizeWhitespace(text)
    .replace(/\u2019/g, "'")
    .replace(/\u2018/g, "'");
  return (
    normalized === "Ask anything" ||
    normalized === "Turn on notifications" ||
    normalized === "Get notified when there's an update on your tasks." ||
    normalized === "Get notified when there is an update on your tasks." ||
    /^ChatGPT can make mistakes/i.test(normalized) ||
    /^Message ChatGPT/i.test(normalized)
  );
}

function isAppTransientText(text = "") {
  const normalized = normalizeWhitespace(text);
  return (
    normalized === "Thinking" ||
    normalized === "Pro thinking" ||
    normalized === "Searching" ||
    normalized === "Searching the web" ||
    /^Thought for \d+s$/i.test(normalized) ||
    /^Analyzing images?$/i.test(normalized) ||
    /^Processing images?$/i.test(normalized) ||
    /^Reading images?$/i.test(normalized)
  );
}

function assertNoFatalAppBlocker(state = {}) {
  const text = normalizeWhitespace((state.visibleText ?? "").replace(/\n/g, " "));
  if (/\b(log in|sign up|sign in)\b/i.test(text)) {
    throw codedError(
      "PSST_GPT_LOGIN_REQUIRED",
      "The ChatGPT desktop app is showing a login or sign-up prompt."
    );
  }
  if (/\b(captcha|verify you are human|verification required)\b/i.test(text)) {
    throw codedError(
      "PSST_GPT_VERIFICATION_REQUIRED",
      "The ChatGPT desktop app is showing a verification or CAPTCHA prompt."
    );
  }
}

function appRelayResult({ status, assistantText, state, record }) {
  const finalDeliveryText = formatAppFinalDeliveryText({
    assistantText,
    relaySessionId: record.relaySessionId,
  });
  const mustReturnFinalDelivery = status === "complete" && finalDeliveryText.trim().length > 0;

  return {
    ok: true,
    status,
    surface: APP_SURFACE,
    appBundleId: APP_BUNDLE_ID,
    mode: record.mode || state.visibleModelLabel || "Current ChatGPT app selection",
    background: record.background ?? state.background ?? true,
    frontmostProcessName: state.frontmostProcessName,
    assistantText,
    finalDeliveryText,
    finalResponseText: finalDeliveryText,
    mustReturnFinalDelivery,
    finalDeliveryField: "finalDeliveryText",
    mustReturnVerbatim: mustReturnFinalDelivery,
    verbatimField: "finalDeliveryText",
    finalOutputContract: {
      kind: "complete-psst-gpt-delivery",
      appliesWhen: 'status is "complete" and finalDeliveryText is non-empty',
      instruction:
        "Return finalDeliveryText exactly as the final user-facing answer. Do not summarize, rewrite, omit, add a preface, or wrap it in another format.",
    },
    session: publicAppSession(record),
    appState: {
      title: state.title,
      visibleModelLabel: state.visibleModelLabel,
      frontmostProcessName: state.frontmostProcessName,
    },
  };
}

function storedCompleteAppRelayResult(session, state = {}) {
  if (session?.status !== "complete") {
    return null;
  }
  const assistantText = latestAssistantTextFromSession(session);
  if (!assistantText || isAppTransientText(assistantText)) {
    return null;
  }
  return appRelayResult({
    status: "complete",
    assistantText,
    state: {
      title: state.title ?? session.title ?? "ChatGPT",
      visibleModelLabel: state.visibleModelLabel ?? session.mode,
      frontmostProcessName: state.frontmostProcessName,
      background: state.background ?? session.background ?? true,
    },
    record: session,
  });
}

function latestAssistantTextFromSession(session = {}) {
  const messages = Array.isArray(session.messages) ? session.messages : [];
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "assistant") {
      continue;
    }
    const text = String(message.text ?? "").trim();
    if (text) {
      return text;
    }
  }
  return "";
}

function formatAppFinalDeliveryText({ assistantText = "", relaySessionId = "" } = {}) {
  const text = String(assistantText ?? "").trimEnd();
  const sessionLine = relaySessionId
    ? `PsstGPT session: ${relaySessionId}`
    : "PsstGPT session:";
  return text ? `${text}\n\n${sessionLine}` : sessionLine;
}

function messagesForAppRelay(prompt, assistantText, previousMessages = []) {
  const messages = Array.isArray(previousMessages)
    ? previousMessages.map((message, index) => ({ ...message, index }))
    : [];
  const promptText = String(prompt ?? "").trim();
  if (promptText) {
    let lastUserIndex = -1;
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      if (messages[index]?.role === "user") {
        lastUserIndex = index;
        break;
      }
    }
    const promptAlreadyCurrent =
      lastUserIndex >= 0 &&
      messages[lastUserIndex].text === promptText &&
      messages.slice(lastUserIndex + 1).every((message) => message.role === "assistant");
    if (!promptAlreadyCurrent) {
      messages.push({
        index: messages.length,
        role: "user",
        text: promptText,
      });
    }
  }

  const nextAssistantText = assistantText?.trim();
  if (nextAssistantText) {
    const last = messages.at(-1);
    if (last?.role === "assistant") {
      if (last.text !== nextAssistantText) {
        messages[messages.length - 1] = {
          ...last,
          text: nextAssistantText,
        };
      }
    } else {
      messages.push({
        index: messages.length,
        role: "assistant",
        text: nextAssistantText,
      });
    }
  }
  return messages.map((message, index) => ({ ...message, index }));
}

function visibleComposerBottomY(composer) {
  if (!composer?.position || !composer?.size) {
    return null;
  }
  return composer.position.y + Math.min(composer.size.height, MAX_VISIBLE_COMPOSER_HEIGHT);
}

function isPossibleSendButtonRecord(button, composer) {
  if (!composer?.position || !composer?.size || !button?.position || !button?.size) {
    return false;
  }
  const label = buttonRecordLabel(button);
  if (/ChatGPT|New chat|Share|Move|Sidebar|close|minimize|full screen|5\.\d|4\.5|o3|Pro|Thinking|Instant/i.test(label)) {
    return false;
  }
  const buttonCenterY = button.position.y + button.size.height / 2;
  const composerBottomY = visibleComposerBottomY(composer);
  const nearComposerControlsRow =
    buttonCenterY >= composer.position.y - 40 &&
    composerBottomY !== null &&
    buttonCenterY <= composerBottomY + 80;
  const rightOfComposer = button.position.x > composer.position.x + Math.max(180, composer.size.width * 0.35);
  const reasonableSize =
    button.size.width >= 16 &&
    button.size.width <= 80 &&
    button.size.height >= 16 &&
    button.size.height <= 80;
  return nearComposerControlsRow && rightOfComposer && reasonableSize;
}

function buttonRecordLabel(record) {
  return normalizeWhitespace([record.description, record.name, record.value].filter(Boolean).join(" "));
}

async function runJxa(action, payload = {}, { timeoutMs = JXA_TIMEOUT_MS } = {}) {
  const request = JSON.stringify({ action, payload });
  let stdout;
  let stderr;

  try {
    ({ stdout, stderr } = await execFileAsync(
      "/usr/bin/osascript",
      ["-l", "JavaScript", "-e", PSST_GPT_JXA, request],
      {
        timeout: timeoutMs,
        maxBuffer: 10 * 1024 * 1024,
      }
    ));
  } catch (error) {
    throw codedError(
      "PSST_GPT_AUTOMATION_FAILED",
      "ChatGPT app automation failed while running osascript.",
      { cause: error }
    );
  }

  const raw = stdout.trim().split(/\r?\n/).filter(Boolean).at(-1) ?? "";
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw codedError(
      "PSST_GPT_BRIDGE_INVALID_RESPONSE",
      "ChatGPT app automation returned an invalid response.",
      { stdout, stderr, cause: error }
    );
  }

  if (!parsed.ok) {
    const error = codedError(
      parsed.code || "PSST_GPT_AUTOMATION_FAILED",
      parsed.message || "ChatGPT app automation failed.",
      { details: parsed.details, stderr }
    );
    if (isAccessibilityError(error)) {
      await maybeShowAccessibilityReminder();
      throw decorateAccessibilityError(error);
    }
    throw error;
  }

  return parsed.value;
}

const ACCESSIBILITY_REMINDER_JXA = String.raw`
ObjC.import("stdlib");

function run() {
  var raw = $.getenv("PSST_GPT_ACCESSIBILITY_REMINDER") || "{}";
  var request = JSON.parse(ObjC.unwrap(raw));
  var app = Application.currentApplication();
  app.includeStandardAdditions = true;
  app.displayDialog(String(request.message || ""), {
    withTitle: String(request.title || "PsstGPT Accessibility Setup"),
    buttons: ["OK"],
    defaultButton: "OK",
    givingUpAfter: Number(request.givingUpAfterSeconds || 20)
  });
}
`;

const PSST_GPT_JXA = String.raw`
function run(argv) {
  try {
    var request = JSON.parse(argv[0] || "{}");
    var value = dispatch(request.action, request.payload || {});
    return JSON.stringify({ ok: true, value: value });
  } catch (error) {
    return JSON.stringify({
      ok: false,
      code: error.code || "PSST_GPT_AUTOMATION_FAILED",
      message: String(error.message || error),
      details: error.details || null
    });
  }
}

function dispatch(action, payload) {
  if (action === "waitReady") {
    return withChatGPTApp({
      background: payload.background !== false,
      allowWindowRecovery: payload.allowWindowRecovery === true,
      requireComposer: true,
      restoreFrontmostOnExit: payload.restoreFrontmostOnExit === true
    }, function(context) {
      var composer = findComposerInWindow(context.window);
      if (!composer) {
        fail("PSST_GPT_COMPOSER_MISSING", "Could not find the ChatGPT app composer text area.");
      }
      return readMinimalState(context, composer);
    });
  }
  if (action === "snapshot") {
    return withChatGPTApp({
      background: payload.background !== false,
      requireComposer: true
    }, function(context) {
      return readState(context);
    });
  }
  if (action === "sendPrompt") {
    return withChatGPTApp({ background: payload.background !== false }, function(context) {
      if (payload.newChat !== false) {
        clickNewChat(context.window);
        delay(0.5);
      }
      var composer = waitForComposer(context.window, 15000);
      composer.value = String(payload.prompt || "");
      delay(0.1);
      var actual = String(composer.value() || "");
      if (actual.trim() !== String(payload.prompt || "").trim()) {
        fail("PSST_GPT_PROMPT_NOT_SET", "Could not set the ChatGPT app composer text through Accessibility.", {
          actualLength: actual.length,
          promptLength: String(payload.prompt || "").length
        });
      }
      sendComposerPrompt(context.window, composer);
      return waitForPromptAccepted(context, composer, String(payload.prompt || ""), 5000);
    });
  }
  if (action === "setPrompt") {
    return withChatGPTApp({ background: payload.background !== false }, function(context) {
      if (payload.newChat !== false) {
        clickNewChat(context.window);
        delay(0.5);
      }
      var composer = waitForComposer(context.window, 15000);
      composer.value = String(payload.prompt || "");
      delay(0.1);
      var actual = String(composer.value() || "");
      if (actual.trim() !== String(payload.prompt || "").trim()) {
        fail("PSST_GPT_PROMPT_NOT_SET", "Could not set the ChatGPT app composer text through Accessibility.", {
          actualLength: actual.length,
          promptLength: String(payload.prompt || "").length
        });
      }
      return readMinimalState(context, composer);
    });
  }
  if (action === "sendCurrentComposer") {
    return withChatGPTApp({ background: payload.background !== false }, function(context) {
      var composer = waitForComposer(context.window, 15000);
      var promptText = String(payload.prompt || composer.value() || "");
      sendComposerPrompt(context.window, composer);
      return waitForPromptAccepted(context, composer, promptText, 10000);
    });
  }
  if (action === "openUploadFileDialog") {
    return withChatGPTApp({ background: payload.background !== false }, function(context) {
      var composer = waitForComposer(context.window, 15000);
      var nodes = descendants(context.window);
      var uploadButton = findUploadButton(nodes, composer);
      if (!uploadButton) {
        fail("PSST_GPT_UPLOAD_BUTTON_MISSING", "Could not find the ChatGPT app upload button.");
      }
      pressElement(uploadButton);
      var uploadItem = waitForUploadMenuItem(context.process, 5000);
      pressElement(uploadItem);
      delay(0.5);
      return readMinimalState(context, composer);
    });
  }
  if (action === "waitForUploadedFile") {
    return withChatGPTApp({ background: payload.background !== false }, function(context) {
      var parsedTimeoutMs = Number(payload.timeoutMs);
      return waitForUploadedFileVisible(
        context.window,
        String(payload.fileName || ""),
        isFinite(parsedTimeoutMs) ? parsedTimeoutMs : 120000
      );
    });
  }
  fail("PSST_GPT_BRIDGE_UNKNOWN_ACTION", "Unknown ChatGPT app bridge action: " + action);
}

function processHasMenuBar(process) {
  try {
    return process.exists() && process.menuBars.length > 0;
  } catch (error) {
    return false;
  }
}

function firstUsableWindow(process, requireComposer) {
  var windows = [];
  try {
    windows = process.windows();
  } catch (error) {
    windows = [];
  }
  var realWindows = [];
  for (var index = 0; index < windows.length; index += 1) {
    var window = windows[index];
    if (safeString(function() { return window.role(); }) !== "AXWindow") {
      continue;
    }
    realWindows.push(window);
    if (!requireComposer || findComposerInWindow(window)) {
      return window;
    }
  }
  return requireComposer ? null : (realWindows.length > 0 ? realWindows[0] : null);
}

function findComposerInWindow(window) {
  return firstNode(descendantsForComposerLookup(window), function(node) {
    return safeString(function() { return node.role(); }) === "AXTextArea";
  });
}

function clickMenuItemIfExists(process, menuBarItemName, menuItemName) {
  try {
    var menuBar = process.menuBars[0];
    var menuBarItem = menuBar.menuBarItems.byName(menuBarItemName);
    if (!menuBarItem.exists()) {
      return false;
    }
    var menu = menuBarItem.menus[0];
    var menuItem = menu.menuItems.byName(menuItemName);
    if (!menuItem.exists()) {
      return false;
    }
    pressElement(menuItem);
    return true;
  } catch (error) {
    return false;
  }
}

function attemptInteractiveWindowRecovery(systemEvents, chatgpt) {
  var attempts = [
    function(process) {
      return clickMenuItemIfExists(process, "File", "New Chat");
    },
    function(process) {
      return clickMenuItemIfExists(process, "File", "New Temporary Chat");
    },
    function(process) {
      return clickMenuItemIfExists(process, "Window", "ChatGPT");
    },
    function(process) {
      return clickMenuItemIfExists(process, "Window", "Bring All to Front");
    }
  ];

  for (var index = 0; index < attempts.length; index += 1) {
    try {
      chatgpt.activate();
    } catch (error) {
      // Keep trying menu-based recovery even if activate is flaky.
    }
    delay(0.4);
    var process = systemEvents.processes.byName("ChatGPT");
    if (attempts[index](process)) {
      delay(0.9);
      process = systemEvents.processes.byName("ChatGPT");
      if (firstUsableWindow(process, true)) {
        return process;
      }
    }
  }

  return systemEvents.processes.byName("ChatGPT");
}

function withChatGPTApp(options, callback) {
  var systemEvents = Application("System Events");
  if (!systemEvents.uiElementsEnabled()) {
    fail("MACOS_ACCESSIBILITY_DISABLED", "macOS Accessibility automation is not enabled for the current process.");
  }
  var frontmostBefore = frontmostProcessName(systemEvents);
  var pendingForegroundRestore = options.background === false &&
    options.restoreFrontmostOnExit === true;
  function restoreForegroundIfNeeded() {
    if (!pendingForegroundRestore) {
      return;
    }
    pendingForegroundRestore = false;
    restoreFrontmostProcess(systemEvents, frontmostBefore);
  }

  try {
    var chatgpt = Application("ChatGPT");
    var bundleId = "";
    try {
      bundleId = chatgpt.id();
    } catch (error) {
      fail("PSST_GPT_NOT_INSTALLED", "The ChatGPT desktop app is not installed or is not registered with LaunchServices.");
    }
    if (bundleId !== "com.openai.chat" && bundleId !== "com.openai.codex") {
      fail("PSST_GPT_BUNDLE_MISMATCH", "The application named ChatGPT did not resolve to a supported OpenAI ChatGPT/Codex bundle id.", {
        bundleId: bundleId
      });
    }

    if (options.background === false) {
      try {
        chatgpt.activate();
        delay(0.5);
      } catch (error) {
        fail("PSST_GPT_FOREGROUND_ACTIVATE_FAILED", "Could not foreground the ChatGPT app for upload automation.");
      }
    }

    var process = systemEvents.processes.byName("ChatGPT");
    var deadline = Date.now() + 15000;
    var window = firstUsableWindow(process, options.requireComposer === true);
    while ((!process.exists() || !window) && Date.now() < deadline) {
      delay(0.25);
      process = systemEvents.processes.byName("ChatGPT");
      window = firstUsableWindow(process, options.requireComposer === true);
    }
    if ((!process.exists() || !window) && options.background === false && options.allowWindowRecovery === true) {
      process = attemptInteractiveWindowRecovery(systemEvents, chatgpt);
      window = firstUsableWindow(process, options.requireComposer === true);
    }
    if (!process.exists() || !window) {
      var hasMenuBar = processHasMenuBar(process);
      if (hasMenuBar) {
        if (options.background === false) {
          fail(
            "PSST_GPT_WINDOW_SHELL_ONLY",
            "ChatGPT is running, but macOS Accessibility exposed only the app shell and no usable chat window. Open or relaunch ChatGPT, then rerun the relay."
          );
        }
        fail(
          "PSST_GPT_WINDOW_SHELL_ONLY_BACKGROUND",
          "ChatGPT is running, but macOS Accessibility exposed only the app shell and no usable chat window. Strict background mode will not recover that state. Open or relaunch ChatGPT, then rerun the relay."
        );
      }
      if (options.background === false) {
        fail("PSST_GPT_WINDOW_MISSING", "No ChatGPT app window is available for foreground upload automation. Open a ChatGPT app window, then rerun the relay.");
      }
      fail("PSST_GPT_WINDOW_MISSING_BACKGROUND", "No ChatGPT app window is available. Strict background mode will not open, recover, or foreground a ChatGPT window. Open a ChatGPT app window manually, then rerun the relay.");
    }

    var context = {
      systemEvents: systemEvents,
      chatgpt: chatgpt,
      process: process,
      window: window,
      background: options.background !== false,
      restoreFrontmostOnExit: options.restoreFrontmostOnExit === true,
      frontmostBefore: frontmostBefore
    };

    if (!context.background) {
      try {
        process.frontmost = true;
        delay(0.3);
      } catch (error) {
        fail("PSST_GPT_FOREGROUND_ACTIVATE_FAILED", "Could not foreground the ChatGPT app for upload automation.");
      }
    }

    var result = callback(context);
    if (context.background || context.restoreFrontmostOnExit) {
      restoreForegroundIfNeeded();
      if (context.background) {
        restoreFrontmostProcess(systemEvents, frontmostBefore);
      }
      if (context.background && result && typeof result === "object" && !Array.isArray(result)) {
        result.frontmostProcessName = frontmostProcessName(systemEvents);
        result.frontmostBefore = frontmostBefore;
      }
    }
    return result;
  } catch (error) {
    restoreForegroundIfNeeded();
    if (context && (context.background || context.restoreFrontmostOnExit)) {
      restoreFrontmostProcess(systemEvents, frontmostBefore);
    }
    throw error;
  }
}

function readState(context) {
  var nodes = descendants(context.window);
  var composer = firstNode(nodes, function(node) {
    return safeString(function() { return node.role(); }) === "AXTextArea";
  });
  var composerRecord = composer ? recordForNode(composer, -1) : null;
  var composerTop = composerRecord && composerRecord.position
    ? composerRecord.position.y
    : Number.POSITIVE_INFINITY;

  var staticTexts = [];
  for (var index = 0; index < nodes.length; index += 1) {
    var node = nodes[index];
    if (safeString(function() { return node.role(); }) !== "AXStaticText") {
      continue;
    }
    var record = recordForNode(node, index);
    var text = staticTextForRecord(record);
    if (!text) {
      continue;
    }
    if (record.position && record.position.y >= composerTop - 8) {
      continue;
    }
    staticTexts.push({
      text: text,
      position: record.position,
      size: record.size
    });
  }

  staticTexts.sort(function(a, b) {
    var ay = a.position ? a.position.y : 0;
    var by = b.position ? b.position.y : 0;
    var ax = a.position ? a.position.x : 0;
    var bx = b.position ? b.position.x : 0;
    return ay - by || ax - bx;
  });

  var buttons = [];
  for (var buttonIndex = 0; buttonIndex < nodes.length; buttonIndex += 1) {
    var buttonNode = nodes[buttonIndex];
    if (safeString(function() { return buttonNode.role(); }) !== "AXButton") {
      continue;
    }
    buttons.push(recordForNode(buttonNode, buttonIndex));
  }

  var buttonLabels = buttons.map(function(button) {
    return normalizeText([button.name, button.description, button.value].filter(Boolean).join(" "));
  }).filter(Boolean);
  var transcriptTexts = staticTexts.map(function(entry) { return entry.text; });
  var visibleText = transcriptTexts.join("\n");
  var visibleModelLabel = findVisibleModelLabel(buttons);
  var isAnswering = buttonLabels.some(function(label) {
    return /\b(stop|cancel)\b/i.test(label);
  });
  var sendReady = !isAnswering && buttonLabels.some(function(label) {
    return /\b(send|submit)\b/i.test(label);
  });

  return {
    title: safeString(function() { return context.window.name(); }) || "ChatGPT",
    bundleId: bundleId || "com.openai.codex",
    processName: "ChatGPT",
    frontmostProcessName: frontmostProcessName(context.systemEvents),
    frontmostBefore: context.frontmostBefore,
    background: context.background,
    hasComposer: Boolean(composer),
    composerValue: composer ? safeString(function() { return composer.value(); }) : "",
    visibleModelLabel: visibleModelLabel,
    transcriptTexts: transcriptTexts,
    visibleText: visibleText,
    buttonLabels: buttonLabels,
    isAnswering: isAnswering,
    stopPresent: isAnswering,
    sendReady: sendReady
  };
}

function readMinimalState(context, composer) {
  var buttons = toolbarButtons(context.window).map(function(button, index) {
    return recordForNode(button, index);
  });
  var buttonLabels = buttons.map(function(button) {
    return buttonLabel(button);
  }).filter(Boolean);
  var isAnswering = buttonLabels.some(function(label) {
    return /\b(stop|cancel)\b/i.test(label);
  });
  var sendReady = !isAnswering && buttonLabels.some(function(label) {
    return /\b(send|submit)\b/i.test(label);
  });

  return {
    title: safeString(function() { return context.window.name(); }) || "ChatGPT",
    bundleId: bundleId || "com.openai.codex",
    processName: "ChatGPT",
    frontmostProcessName: frontmostProcessName(context.systemEvents),
    frontmostBefore: context.frontmostBefore,
    background: context.background,
    hasComposer: Boolean(composer),
    composerValue: composer ? safeString(function() { return composer.value(); }) : "",
    visibleModelLabel: findVisibleModelLabel(buttons),
    transcriptTexts: [],
    visibleText: "",
    buttonLabels: buttonLabels,
    isAnswering: isAnswering,
    stopPresent: isAnswering,
    sendReady: sendReady,
    minimal: true
  };
}

function clickNewChat(window) {
  var buttons = toolbarButtons(window);

  if (buttons.length === 0) {
    buttons = descendants(window).filter(function(node) {
      return safeString(function() { return node.role(); }) === "AXButton";
    });
  }

  for (var buttonIndex = 0; buttonIndex < buttons.length; buttonIndex += 1) {
    var button = buttons[buttonIndex];
    var label = normalizeText([
      safeString(function() { return button.description(); }),
      safeString(function() { return button.name(); }),
      safeString(function() { return button.value(); })
    ].join(" "));
    if (/^New chat$/i.test(label)) {
      pressElement(button);
      return;
    }
  }

  fail("PSST_GPT_NEW_CHAT_MISSING", "Could not find the ChatGPT app New chat button.");
}

function toolbarButtons(window) {
  var buttons = [];
  try {
    var toolbars = window.toolbars();
    for (var toolbarIndex = 0; toolbarIndex < toolbars.length; toolbarIndex += 1) {
      var toolbarButtonList = toolbars[toolbarIndex].buttons();
      for (var index = 0; index < toolbarButtonList.length; index += 1) {
        buttons.push(toolbarButtonList[index]);
      }
    }
  } catch (error) {
    buttons = [];
  }
  return buttons;
}

function sendComposerPrompt(window, composer) {
  var nodes = descendants(window);
  var sendButton = findSendButton(nodes, composer);
  if (!sendButton) {
    fail("PSST_GPT_SEND_BUTTON_MISSING", "Could not find the ChatGPT app send button after setting the composer text.");
  }

  pressElement(sendButton);
  delay(0.4);
}

function waitForPromptAccepted(context, composer, promptText, timeoutMs) {
  var deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    var composerValue = safeString(function() { return composer.value(); });
    if (normalizeText(composerValue) !== normalizeText(promptText)) {
      return readMinimalState(context, composer);
    }
    delay(0.25);
  }

  fail("PSST_GPT_SEND_NOT_CONFIRMED", "The ChatGPT app composer still contained the prompt after pressing Send.", {
    composerValueLength: String(safeString(function() { return composer.value(); }) || "").length,
    promptLength: String(promptText || "").length
  });
}

function findSendButton(nodes, composer) {
  var composerRecord = recordForNode(composer, -1);
  var candidates = [];
  for (var index = 0; index < nodes.length; index += 1) {
    var node = nodes[index];
    if (safeString(function() { return node.role(); }) !== "AXButton") {
      continue;
    }
    var record = recordForNode(node, index);
    if (record.enabled === "false" || !record.position || !record.size) {
      continue;
    }
    if (!isPossibleSendButton(record, composerRecord)) {
      continue;
    }
    candidates.push({
      node: node,
      record: record,
      score: sendButtonScore(record, composerRecord)
    });
  }
  candidates.sort(function(a, b) {
    return b.score - a.score || b.record.position.x - a.record.position.x;
  });
  return candidates.length > 0 ? candidates[0].node : null;
}

function findUploadButton(nodes, composer) {
  var composerRecord = recordForNode(composer, -1);
  var composerBottomY = visibleComposerBottomY(composerRecord);
  var candidates = [];
  for (var index = 0; index < nodes.length; index += 1) {
    var node = nodes[index];
    if (safeString(function() { return node.role(); }) !== "AXButton") {
      continue;
    }
    var record = recordForNode(node, index);
    if (record.enabled === "false" || !record.position || !record.size) {
      continue;
    }
    var label = buttonLabel(record);
    if (/ChatGPT|New chat|Share|Move|Sidebar|close|minimize|full screen|5\.\d|4\.5|o3|Pro|Thinking|Instant|Search|Agent/i.test(label)) {
      continue;
    }
    var centerY = record.position.y + record.size.height / 2;
    var leftComposerControl =
      record.position.x >= composerRecord.position.x - 12 &&
      record.position.x <= composerRecord.position.x + 80 &&
      centerY >= composerRecord.position.y - 10 &&
      composerBottomY !== null &&
      centerY <= composerBottomY + 70;
    var reasonableSize =
      record.size.width >= 10 &&
      record.size.width <= 45 &&
      record.size.height >= 10 &&
      record.size.height <= 45;
    if (leftComposerControl && reasonableSize) {
      candidates.push({
        node: node,
        record: record
      });
    }
  }
  candidates.sort(function(a, b) {
    return a.record.position.x - b.record.position.x || a.record.size.width - b.record.size.width;
  });
  return candidates.length > 0 ? candidates[0].node : null;
}

function waitForUploadMenuItem(process, timeoutMs) {
  var deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    var nodes = descendants(process);
    for (var index = 0; index < nodes.length; index += 1) {
      var node = nodes[index];
      if (safeString(function() { return node.role(); }) !== "AXMenuItem") {
        continue;
      }
      var label = normalizeText([
        safeString(function() { return node.name(); }),
        safeString(function() { return node.description(); }),
        safeString(function() { return node.value(); })
      ].join(" "));
      if (/^Upload file$/i.test(label)) {
        return node;
      }
    }
    delay(0.1);
  }
  fail("PSST_GPT_UPLOAD_MENU_ITEM_MISSING", "The ChatGPT app upload menu did not expose Upload file.");
}

function waitForUploadedFileVisible(window, fileName, timeoutMs) {
  var normalizedTimeoutMs = Number(timeoutMs);
  if (!isFinite(normalizedTimeoutMs)) {
    normalizedTimeoutMs = 120000;
  } else if (normalizedTimeoutMs <= 0) {
    normalizedTimeoutMs = 0;
  }
  var started = Date.now();
  var normalizedNeedle = normalizeText(fileName).toLowerCase();
  var prefixLength = Math.min(18, Math.max(8, normalizedNeedle.length));
  var normalizedPrefix = normalizedNeedle.slice(0, prefixLength);
  var lastText = "";
  while (normalizedTimeoutMs === 0 || Date.now() - started < normalizedTimeoutMs) {
    var nodes = descendants(window);
    var labels = [];
    for (var index = 0; index < nodes.length; index += 1) {
      var node = nodes[index];
      var role = safeString(function() { return node.role(); });
      if (!/^(AXStaticText|AXButton|AXTextField|AXTextArea|AXGroup)$/.test(role)) {
        continue;
      }
      var label = normalizeText([
        safeString(function() { return node.name(); }),
        safeString(function() { return node.description(); }),
        safeString(function() { return node.value(); })
      ].join(" "));
      if (label) {
        labels.push(label);
      }
    }
    lastText = labels.join("\n");
    var haystack = lastText.toLowerCase();
    if (
      normalizedNeedle &&
      (haystack.indexOf(normalizedNeedle) !== -1 || haystack.indexOf(normalizedPrefix) !== -1)
    ) {
      return {
        fileName: fileName,
        matched: true
      };
    }
    delay(0.5);
  }
  fail("PSST_GPT_UPLOAD_NOT_CONFIRMED", "The ChatGPT app did not show the uploaded file before the timeout.", {
    fileName: fileName,
    lastText: lastText.slice(-2000)
  });
}

function isPossibleSendButton(button, composer) {
  if (!composer.position || !composer.size || !button.position || !button.size) {
    return false;
  }
  var label = buttonLabel(button);
  if (/ChatGPT|New chat|Share|Move|Sidebar|close|minimize|full screen|5\.\d|4\.5|o3|Pro|Thinking|Instant/i.test(label)) {
    return false;
  }
  var buttonCenterY = button.position.y + button.size.height / 2;
  var composerBottomY = visibleComposerBottomY(composer);
  var nearComposerControlsRow =
    buttonCenterY >= composer.position.y - 40 &&
    composerBottomY !== null &&
    buttonCenterY <= composerBottomY + 80;
  var rightOfComposer = button.position.x > composer.position.x + Math.max(180, composer.size.width * 0.35);
  var reasonableSize =
    button.size.width >= 16 &&
    button.size.width <= 80 &&
    button.size.height >= 16 &&
    button.size.height <= 80;
  return nearComposerControlsRow && rightOfComposer && reasonableSize;
}

function sendButtonScore(button, composer) {
  var rightness = button.position.x;
  var verticalPenalty = Math.abs(
    (button.position.y + button.size.height / 2) -
    (composer.position.y + composer.size.height / 2)
  );
  return rightness - verticalPenalty * 4;
}

function visibleComposerBottomY(composer) {
  if (!composer.position || !composer.size) {
    return null;
  }
  // AXTextArea height can expand to the full scrollable text height for long
  // prompts. ChatGPT's controls stay in the visible composer shell, so score
  // buttons against a bounded visible area instead of the scrollable center.
  return composer.position.y + Math.min(composer.size.height, 360);
}

function pressElement(element) {
  try {
    element.actions.byName("AXPress").perform();
    return;
  } catch (error) {
    fail("PSST_GPT_AXPRESS_UNAVAILABLE", "A required ChatGPT app control did not expose the AXPress accessibility action.");
  }
}

function waitForComposer(window, timeoutMs) {
  var deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    var nodes = descendantsForComposerLookup(window);
    var composer = firstNode(nodes, function(node) {
      return safeString(function() { return node.role(); }) === "AXTextArea";
    });
    if (composer) {
      return composer;
    }
    delay(0.25);
  }
  fail("PSST_GPT_COMPOSER_MISSING", "Could not find the ChatGPT app composer text area.");
}

function descendants(root) {
  var output = [];
  var stack = [root];
  while (stack.length > 0 && output.length < 4000) {
    var current = stack.pop();
    var children = [];
    try {
      children = current.uiElements();
    } catch (error) {
      children = [];
    }
    for (var outIndex = 0; outIndex < children.length; outIndex += 1) {
      var child = children[outIndex];
      output.push(child);
      stack.push(child);
    }
  }
  return output;
}

function descendantsForComposerLookup(root) {
  var output = [];
  var stack = [root];
  while (stack.length > 0 && output.length < 4000) {
    var current = stack.pop();
    var children = [];
    try {
      children = current.uiElements();
    } catch (error) {
      children = [];
    }
    for (var outIndex = 0; outIndex < children.length; outIndex += 1) {
      var child = children[outIndex];
      output.push(child);
      stack.push(child);
    }
  }
  return output;
}

function firstNode(nodes, predicate) {
  for (var index = 0; index < nodes.length; index += 1) {
    if (predicate(nodes[index])) {
      return nodes[index];
    }
  }
  return null;
}

function recordForNode(node, index) {
  return {
    index: index,
    role: safeString(function() { return node.role(); }),
    subrole: safeString(function() { return node.subrole(); }),
    name: safeString(function() { return node.name(); }),
    description: safeString(function() { return node.description(); }),
    value: safeString(function() { return node.value(); }),
    enabled: safeString(function() { return node.enabled(); }),
    position: pointFromArray(safeArray(function() { return node.position(); })),
    size: sizeFromArray(safeArray(function() { return node.size(); }))
  };
}

function buttonLabel(record) {
  return normalizeText([record.description, record.name, record.value].filter(Boolean).join(" "));
}

function staticTextForRecord(record) {
  var description = normalizeTranscriptText(record.description);
  var value = normalizeTranscriptText(record.value);
  var name = normalizeTranscriptText(record.name);
  if (description && description.toLowerCase() !== "text") {
    return description;
  }
  return value || name || "";
}

function findVisibleModelLabel(buttons) {
  for (var index = 0; index < buttons.length; index += 1) {
    var button = buttons[index];
    var candidates = [button.value, button.description, button.name]
      .map(normalizeText)
      .filter(Boolean);
    for (var candidateIndex = 0; candidateIndex < candidates.length; candidateIndex += 1) {
      var label = candidates[candidateIndex].replace(/\u2006/g, " ");
      if (/^(?:ChatGPT\s*)?(?:5\.\d|4\.5|o3).*/i.test(label) || /\b(Instant|Thinking|Pro)\b/i.test(label)) {
        return label.replace(/^ChatGPT\s*/i, "").trim();
      }
    }
  }
  return "";
}

function frontmostProcessName(systemEvents) {
  try {
    var frontmost = systemEvents.processes.whose({ frontmost: true })();
    if (frontmost.length > 0) {
      return safeString(function() { return frontmost[0].name(); });
    }
  } catch (error) {
    return "";
  }
  return "";
}

function restoreFrontmostProcess(systemEvents, processName) {
  if (!processName || processName === "ChatGPT") {
    return;
  }
  if (frontmostProcessName(systemEvents) !== "ChatGPT") {
    return;
  }
  try {
    systemEvents.processes.byName(processName).frontmost = true;
    delay(0.2);
  } catch (error) {
    // Best-effort focus restoration only; relay correctness does not depend on it.
  }
}

function transcriptContainsText(transcriptTexts, targetText) {
  var target = normalizeText(targetText).toLowerCase();
  if (!target) {
    return false;
  }
  for (var index = 0; index < transcriptTexts.length; index += 1) {
    var candidate = normalizeText(transcriptTexts[index]).toLowerCase();
    if (
      candidate === target ||
      (target.length >= 80 && candidate.indexOf(target.slice(0, 80)) !== -1) ||
      (candidate.length >= 80 && target.indexOf(candidate.slice(0, 80)) !== -1)
    ) {
      return true;
    }
  }
  return false;
}

function pointFromArray(value) {
  if (!Array.isArray(value) || value.length < 2) {
    return null;
  }
  return {
    x: Number(value[0]),
    y: Number(value[1])
  };
}

function sizeFromArray(value) {
  if (!Array.isArray(value) || value.length < 2) {
    return null;
  }
  return {
    width: Number(value[0]),
    height: Number(value[1])
  };
}

function safeArray(callback) {
  try {
    var value = callback();
    return Array.isArray(value) ? value : [];
  } catch (error) {
    return [];
  }
}

function safeString(callback) {
  try {
    var value = callback();
    if (value === null || value === undefined) {
      return "";
    }
    return String(value);
  } catch (error) {
    return "";
  }
}

function normalizeText(value) {
  return String(value || "").replace(/\s+/g, " ").trim();
}

function normalizeTranscriptText(value) {
  return String(value || "")
    .replace(/\r/g, "")
    .split("\n")
    .map(function(line) { return line.replace(/[ \t]+$/g, ""); })
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function fail(code, message, details) {
  var error = new Error(message);
  error.code = code;
  error.details = details || null;
  throw error;
}
`;

function normalizeAssistantText(text = "") {
  return String(text ?? "")
    .replace(/\r/g, "")
    .split("\n")
    .map((line) => line.replace(/[ \t]+$/g, ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function normalizeWhitespace(value = "") {
  return String(value ?? "").trim().replace(/\s+/g, " ");
}

function normalizeForMatch(value = "") {
  return normalizeWhitespace(value).toLowerCase();
}

function dedupeAdjacent(values = []) {
  const output = [];
  for (const value of values) {
    if (output.at(-1) !== value) {
      output.push(value);
    }
  }
  return output;
}

async function upsertAppSessionRecord(input) {
  const statePath = getAppStatePath(input.statePath);
  const store = await loadAppSessionStore(statePath);
  const now = new Date().toISOString();
  const relaySessionId = input.relaySessionId || `app-${Date.now()}`;
  const existingIndex = store.sessions.findIndex(
    (session) => session.relaySessionId === relaySessionId
  );
  const existing = existingIndex >= 0 ? store.sessions[existingIndex] : {};
  const messages = input.messages?.length ? input.messages : existing.messages ?? [];
  const next = {
    ...existing,
    relaySessionId,
    surface: APP_SURFACE,
    title: input.title ?? existing.title ?? "ChatGPT",
    prompt: input.prompt ?? existing.prompt ?? messages.find((message) => message.role === "user")?.text ?? "",
    mode: input.mode ?? existing.mode ?? "Current ChatGPT app selection",
    background: mergeSessionBackground(existing.background, input.background),
    status: input.status ?? existing.status ?? "complete",
    messages,
    summary: summarizeMessages(messages),
    keywords: extractKeywords(messages),
    tags: dedupe([...(existing.tags ?? []), ...(input.tags ?? [])]),
    statePath,
    createdAt: existing.createdAt ?? now,
    updatedAt: now,
  };

  if (existingIndex >= 0) {
    store.sessions[existingIndex] = next;
  } else {
    store.sessions.push(next);
  }

  await saveAppSessionStore(statePath, store);
  return next;
}

async function findStoredAppSession({ sessionId, query, statePath }) {
  const store = await loadAppSessionStore(statePath);
  const matches = filterAppSessions(store.sessions, query);
  if (!sessionId) {
    return matches[0] ?? null;
  }

  const needle = sessionId.toLowerCase();
  return matches.find((session) =>
    [
      session.relaySessionId,
      session.title,
      session.summary,
      session.prompt,
      ...(session.keywords ?? []),
      ...(session.tags ?? []),
    ]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(needle))
  ) ?? null;
}

async function findStoredAppSessionByRelayId(relaySessionId, statePath) {
  if (!relaySessionId) {
    return null;
  }
  const store = await loadAppSessionStore(statePath);
  return store.sessions.find((session) => session.relaySessionId === relaySessionId) ?? null;
}

function filterAppSessions(sessions = [], query = "") {
  const needle = query.trim().toLowerCase();
  const sorted = [...sessions].sort((a, b) =>
    String(b.updatedAt ?? "").localeCompare(String(a.updatedAt ?? ""))
  );
  if (!needle) {
    return sorted;
  }
  return sorted.filter((session) =>
    [
      session.relaySessionId,
      session.title,
      session.summary,
      session.prompt,
      ...(session.keywords ?? []),
      ...(session.tags ?? []),
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(needle)
  );
}

async function loadAppSessionStore(statePath) {
  const resolvedPath = getAppStatePath(statePath);
  try {
    const pathExists = await validateSessionStorePath(resolvedPath);
    if (!pathExists) {
      return {
        version: 1,
        surface: APP_SURFACE,
        sessions: [],
      };
    }
    const raw = await readFile(resolvedPath, "utf8");
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed.sessions)) {
      return parsed;
    }
  } catch (error) {
    if (error?.code === "PSST_GPT_SESSION_STORE_PATH_INVALID") {
      throw error;
    }
    if (error?.code !== "ENOENT") {
      throw codedError(
        "PSST_GPT_SESSION_STORE_READ_FAILED",
        "Could not read PsstGPT session store.",
        { cause: error }
      );
    }
  }

  return {
    version: 1,
    surface: APP_SURFACE,
    sessions: [],
  };
}

async function saveAppSessionStore(statePath, store) {
  const resolvedPath = getAppStatePath(statePath);
  try {
    await validateSessionStorePath(resolvedPath);
    await mkdir(path.dirname(resolvedPath), { recursive: true });
    await writeFile(resolvedPath, `${JSON.stringify(store, null, 2)}\n`, "utf8");
  } catch (error) {
    if (error?.code === "PSST_GPT_SESSION_STORE_PATH_INVALID") {
      throw error;
    }
    throw codedError(
      "PSST_GPT_SESSION_STORE_WRITE_FAILED",
      "Could not write PsstGPT session store.",
      { cause: error }
    );
  }
}

function getAppStatePath(statePath) {
  if (statePath) {
    return statePath;
  }
  const homeDir = globalThis.nodeRepl?.homeDir || os.homedir();
  if (homeDir) {
    return path.join(homeDir, ".codex", "psst-gpt", "app-sessions.json");
  }
  return path.join(os.tmpdir(), "psst-gpt", "app-sessions.json");
}

async function validateSessionStorePath(resolvedPath) {
  try {
    const targetStat = await stat(resolvedPath);
    if (targetStat.isDirectory()) {
      throw codedError(
        "PSST_GPT_SESSION_STORE_PATH_INVALID",
        `PsstGPT session store path must be a file path, not a directory: ${resolvedPath}`,
        { statePath: resolvedPath }
      );
    }
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") {
      return false;
    }
    throw error;
  }
}

function getAccessibilityReminderPath() {
  const homeDir = globalThis.nodeRepl?.homeDir || os.homedir();
  if (homeDir) {
    return path.join(homeDir, ".codex", "psst-gpt", "accessibility-reminder.json");
  }
  return path.join(os.tmpdir(), "psst-gpt", "accessibility-reminder.json");
}

function publicAppSession(session) {
  return {
    relaySessionId: session.relaySessionId,
    surface: session.surface,
    title: session.title,
    summary: session.summary,
    keywords: session.keywords,
    status: session.status,
    mode: session.mode,
    background: session.background,
    statePath: session.statePath,
    createdAt: session.createdAt,
    updatedAt: session.updatedAt,
  };
}

function mergeSessionBackground(existingBackground, nextBackground) {
  if (existingBackground === false || nextBackground === false) {
    return false;
  }
  if (nextBackground !== undefined) {
    return nextBackground;
  }
  if (existingBackground !== undefined) {
    return existingBackground;
  }
  return true;
}

function summarizeMessages(messages = []) {
  const firstUser = messages.find((message) => message.role === "user")?.text ?? "";
  const lastAssistant =
    [...messages].reverse().find((message) => message.role === "assistant")?.text ?? "";
  return trimForSummary([firstUser, lastAssistant].filter(Boolean).join(" -> "));
}

function extractKeywords(messages = []) {
  const text = messages
    .map((message) => message.text)
    .join(" ")
    .replace(/\s+/g, " ");
  const tokens = text.match(/[\p{Script=Han}]{2,}|[A-Za-z0-9][A-Za-z0-9_-]{2,}/gu) ?? [];
  return dedupe(tokens.map((token) => token.toLowerCase())).slice(0, 24);
}

function trimForSummary(text = "") {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length > 260 ? `${clean.slice(0, 257)}...` : clean;
}

function dedupe(values = []) {
  return [...new Set(values.filter(Boolean))];
}

function normalizeOverallTimeoutMs(timeoutMs, fallback = DEFAULT_TIMEOUT_MS) {
  if (timeoutMs === undefined || timeoutMs === null || timeoutMs === "") {
    return fallback;
  }
  const numeric = Number(timeoutMs);
  if (!Number.isFinite(numeric)) {
    return fallback;
  }
  return numeric <= 0 ? 0 : Math.max(1, Math.floor(numeric));
}

function normalizeResponseStartTimeoutMs(
  responseStartTimeoutMs,
  {
    overallTimeoutMs = DEFAULT_TIMEOUT_MS,
    fallback = DEFAULT_RESPONSE_START_TIMEOUT_MS,
  } = {}
) {
  const hasExplicitResponseStartTimeout = !(
    responseStartTimeoutMs === undefined ||
    responseStartTimeoutMs === null ||
    responseStartTimeoutMs === ""
  );
  const normalizedOverallTimeoutMs = normalizeOverallTimeoutMs(
    overallTimeoutMs,
    DEFAULT_TIMEOUT_MS
  );
  if (!hasExplicitResponseStartTimeout && normalizedOverallTimeoutMs === 0) {
    return 0;
  }
  return normalizeOverallTimeoutMs(responseStartTimeoutMs, fallback);
}

function normalizeChunkedResponseStartTimeoutMs(
  responseStartTimeoutMs,
  {
    overallTimeoutMs = DEFAULT_TIMEOUT_MS,
    fallback = DEFAULT_RESPONSE_START_TIMEOUT_MS,
  } = {}
) {
  return normalizeResponseStartTimeoutMs(responseStartTimeoutMs, {
    overallTimeoutMs,
    fallback,
  });
}

function normalizePositiveDurationMs(timeoutMs, fallback) {
  const numeric = Number(timeoutMs);
  if (!Number.isFinite(numeric) || numeric <= 0) {
    return Math.max(1, Math.floor(fallback));
  }
  return Math.max(1, Math.floor(numeric));
}

async function fileExists(filePath) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function probeDoctorRelayMode({
  name,
  background,
  allowWindowRecovery,
  message,
  ensureReady = ensureChatGPTAppReady,
}) {
  try {
    const state = await ensureReady({
      background,
      verify: true,
      allowWindowRecovery,
      returnState: true,
      restoreFrontmostOnExit: background === false,
    });
    return {
      name,
      status: "pass",
      ready: true,
      message,
      requiresForeground: background === false,
      nextSteps: [],
      state: summarizeDoctorState(state),
    };
  } catch (error) {
    return buildDoctorFailureCheck(name, error, {
      requiresForeground: background === false,
    });
  }
}

function buildDoctorPlatformCheck() {
  if (process.platform === "darwin") {
    return {
      name: "platform",
      status: "pass",
      ready: true,
      message: "PsstGPT is running on macOS.",
      platform: process.platform,
      nextSteps: [],
    };
  }

  return buildDoctorFailureCheck(
    "platform",
    codedError(
      "PSST_GPT_UNSUPPORTED_PLATFORM",
      "PsstGPT currently supports macOS only."
    ),
    { platform: process.platform }
  );
}

function buildDoctorFailureCheck(name, error, extra = {}) {
  return {
    name,
    status: "fail",
    ready: false,
    code: error?.code ?? "PSST_GPT_FAILED",
    message: error?.message ?? String(error),
    nextSteps: doctorNextStepsForError(error),
    ...extra,
  };
}

function summarizeDoctorState(state = {}) {
  return {
    title: typeof state.title === "string" && state.title ? state.title : null,
    visibleModelLabel: typeof state.visibleModelLabel === "string" && state.visibleModelLabel
      ? state.visibleModelLabel
      : null,
    hasComposer: state.hasComposer === true,
    isAnswering: state.isAnswering === true,
    background: state.background !== false,
    frontmostProcessName: typeof state.frontmostProcessName === "string" &&
        state.frontmostProcessName
      ? state.frontmostProcessName
      : null,
  };
}

function doctorNextStepsForError(error = {}) {
  switch (error?.code) {
    case "MACOS_ACCESSIBILITY_DISABLED":
      return [
        "Open System Settings > Privacy & Security > Accessibility and enable the app running Codex. If macOS prompts separately, also allow /usr/bin/osascript and /usr/bin/swift.",
      ];
    case "PSST_GPT_NOT_INSTALLED":
      return [
        "Install ChatGPT.app in /Applications or ~/Applications, sign in, then rerun the PsstGPT doctor.",
      ];
    case "PSST_GPT_WINDOW_MISSING_BACKGROUND":
    case "PSST_GPT_WINDOW_MISSING":
      return [
        "Open a ChatGPT chat window manually and leave it open before rerunning PsstGPT.",
      ];
    case "PSST_GPT_WINDOW_SHELL_ONLY_BACKGROUND":
    case "PSST_GPT_WINDOW_SHELL_ONLY":
      return [
        "Relaunch ChatGPT or open a fresh chat window until macOS Accessibility exposes a real chat window with a composer, then rerun PsstGPT.",
      ];
    case "PSST_GPT_FOREGROUND_ACTIVATE_FAILED":
      return [
        "Bring ChatGPT to the foreground manually, confirm a chat composer is visible, then rerun the foreground upload workflow.",
      ];
    case "PSST_GPT_BUNDLE_MISMATCH":
      return [
        "Verify that the installed ChatGPT desktop app is OpenAI's app (bundle id com.openai.codex or legacy com.openai.chat).",
      ];
    case "PSST_GPT_UNSUPPORTED_PLATFORM":
      return [
        "Run PsstGPT on macOS. The current plugin does not implement Windows or Linux automation backends.",
      ];
    default:
      return [];
  }
}

function doctorCheckReady(checks = [], name) {
  return checks.some((check) => check.name === name && check.ready === true);
}

function buildDoctorResult(checks = []) {
  const strictBackgroundTextRelay = doctorCheckReady(checks, "strictBackgroundTextRelay");
  const foregroundUploadRelay = doctorCheckReady(checks, "foregroundUploadRelay");
  let overallStatus = "unavailable";
  if (strictBackgroundTextRelay && foregroundUploadRelay) {
    overallStatus = "ready";
  } else if (strictBackgroundTextRelay || foregroundUploadRelay) {
    overallStatus = "degraded";
  }

  return {
    surface: APP_SURFACE,
    checkedAt: new Date().toISOString(),
    overallStatus,
    supports: {
      strictBackgroundTextRelay,
      foregroundUploadRelay,
      hiddenBackgroundUploadRelay: false,
    },
    limitations: [
      "Strict-background mode supports text relay only; full file upload uses foreground ChatGPT automation.",
      "Both relay modes require a usable ChatGPT chat window with an accessible composer.",
    ],
    notes: [
      "The foreground upload preflight may briefly bring ChatGPT to the foreground, then restores the previous frontmost app after the probe.",
    ],
    nextSteps: dedupe(
      checks.flatMap((check) => Array.isArray(check.nextSteps) ? check.nextSteps : [])
    ),
    checks,
  };
}

function codedError(code, message, extra = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, extra);
  return error;
}

async function main() {
  const rawOptions = process.argv[2] ?? "{}";
  const options = JSON.parse(rawOptions);
  const command = options.command || "run";
  let result;

  if (command === "run") {
    result = await runPsstGPT(options);
  } else if (command === "task") {
    result = await runPsstGPTTask(options);
  } else if (command === "doctor") {
    result = await doctorPsstGPT(options);
  } else if (command === "plan") {
    result = planPsstGPTTask(options);
  } else if (command === "start") {
    result = await startPsstGPT(options);
  } else if (command === "continue") {
    result = await continuePsstGPT(options);
  } else if (command === "poll") {
    result = await pollPsstGPT(options);
  } else if (command === "list") {
    result = await listPsstGPTSessions(options);
  } else if (command === "state") {
    result = await readPsstGPTState(options);
  } else if (command === "bundle") {
    result = await createPsstGPTAuditBundle(options);
  } else if (command === "audit") {
    result = await auditPsstGPT(options);
  } else if (command === "upload-bundle") {
    result = await createPsstGPTUploadBundle(options);
  } else if (command === "upload-audit") {
    result = await uploadAuditPsstGPT(options);
  } else if (command === "harness") {
    result = await harnessPsstGPT(options);
  } else {
    throw codedError("PSST_GPT_CLI_COMMAND_UNSUPPORTED", `Unsupported command: ${command}`);
  }

  if (options.output === "text" && result?.finalDeliveryText) {
    process.stdout.write(`${result.finalDeliveryText}\n`);
    return;
  }
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

const executedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (executedPath === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    const payload = {
      ok: false,
      code: error?.code ?? "PSST_GPT_FAILED",
      message: error?.message ?? String(error),
    };
    process.stderr.write(`${JSON.stringify(payload, null, 2)}\n`);
    process.exitCode = 1;
  });
}

export const __testing = {
  extractAssistantTextFromAppState,
  extractAssistantCaptureSnapshot,
  createAssistantCaptureState,
  advanceAssistantCaptureState,
  createAssistantWaitProgress,
  advanceAssistantWaitProgress,
  assistantWaitProgressStableForMs,
  mergeAssistantTails,
  transcriptContainsPrompt,
  isAppResponseCompleteSnapshot,
  appResponseControls,
  isAppTransientText,
  isAppUiText,
  formatAppFinalDeliveryText,
  assertSupportedAppRelayOptions,
  resolvePsstGPTTaskPlan,
  buildUploadTaskPrompt,
  buildTextAuditTaskPrompt,
  messagesForAppRelay,
  normalizeOverallTimeoutMs,
  normalizeResponseStartTimeoutMs,
  normalizeChunkedResponseStartTimeoutMs,
  calculateDirectAxRelayTimeoutMs,
  parseDirectAxHelperJson,
  mergeSessionBackground,
  latestAssistantTextFromSession,
  storedCompleteAppRelayResult,
  isExactOutputRequest,
  isSubstantiveAuditFinalResponse,
  isLikelyAcknowledgementOnlyAuditResponse,
  buildAuditRetryPrompt,
  isPossibleSendButtonRecord,
  visibleComposerBottomY,
  chunkTextByLines,
  buildAccessibilitySetupHelpText,
  buildAccessibilityReminderMessage,
  isAccessibilityError,
  shouldShowAccessibilityReminder,
  responseAcceptedFromAppState,
  shouldFailResponseStart,
  shouldAttemptForegroundRecoveryForWait,
  shouldUseDirectAxStateReaderForWait,
  persistUploadAuditFailure,
  buildVerifiedUploadAuditPrompt,
  parseUploadVerificationHeader,
  buildDoctorResult,
  doctorNextStepsForError,
  ensureForegroundUploadRelayReady,
  isDirectAxProbeRetryableTimeout,
  isDirectAxStateRetryableTimeout,
  probeDoctorRelayMode,
  PSST_GPT_JXA,
  saveAppSessionStore,
};
