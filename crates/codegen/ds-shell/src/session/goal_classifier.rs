//! Goal-verification stage (harness-owned).
//!
//! The adversarial skeptic panel is the whole verification: it
//! spawns N independent skeptic subagents in parallel,
//! parses each one's JSON verdict (with terminal-token fallback), and
//! aggregates unanimous structured approval to drive `update_goal(completed:
//! true)`. Each spawn sends a `SubagentEvent::Spawn` directly over
//! `tool_context.subagent_event_tx` — no `task` tool call, so the
//! parent model's transcript stays clean. The spawn is hidden behind
//! the [`GoalClassifierSpawner`] trait so tests can inject deterministic
//! responses; production uses [`ChannelSpawner`]. The struct / trait /
//! constant names retain the `classifier` prefix to keep the env /
//! remote / config wire contract stable across the rewire.

pub(crate) mod evidence;

use crate::session::events::{Event, GoalClassifierFailOpenReason};
use crate::session::goal_planner::{RoleRenderedPrompt, RoleSpawnOverride};
use crate::session::goal_role_tools::RoleToolNames;
use crate::session::goal_tracker::GoalClassifierVerdict;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// Constants

/// Default per-goal classifier run cap. A sane local default; the stall
/// early-exit ([`crate::session::goal_tracker::GOAL_CLASSIFIER_STALL_THRESHOLD`])
/// is the primary, cheaper stop for stuck loops, so this cap is a
/// runaway-cost backstop. There is no upper ceiling — override via
/// `DS_GOAL_CLASSIFIER_MAX` or remote `goal_classifier_max_runs` to
/// raise it arbitrarily (only the `GOAL_CLASSIFIER_MAX_RUNS_MIN` floor
/// is enforced).
pub(crate) const GOAL_CLASSIFIER_MAX_RUNS_DEFAULT: u32 = 10;

/// Floor for `DS_GOAL_CLASSIFIER_MAX` / remote `goal_classifier_max_runs`.
/// Floor 1 keeps the gate live (0 would disable rejection entirely).
/// There is deliberately no upper ceiling so the cap can be raised
/// arbitrarily via remote/env.
pub(crate) const GOAL_CLASSIFIER_MAX_RUNS_MIN: u32 = 1;

/// Maximum size of the embedded diff in bytes. Past this the diff is
/// truncated with an explicit marker — the verifier prompt's
/// diff-based rules can still operate on the head of the diff plus the
/// truncation marker (and rule 5 if even the head is unavailable).
pub(crate) const GOAL_CLASSIFIER_DIFF_MAX_BYTES: usize = 256 * 1024;

/// Overall byte cap for the aggregated panel details file. A 3-skeptic
/// panel of rich reports runs ~30-40 KB; this ceiling leaves wide
/// headroom (≈5 large reports) while bounding a pathological skeptic.
/// Overall cap only — never per-line.
pub(crate) const GOAL_VERIFIER_PANEL_MAX_BYTES: usize = 512 * 1024;

/// Template for the per-attempt details FILE NAME, rooted under the
/// owner-only (0700) per-goal scratch root by `format_details_path`.
/// Classifier artifacts never live in bare `/tmp`: their names are
/// predictable from the prompt/log-visible `verifier_id`, so a
/// world-writable directory would let a local attacker pre-plant a
/// symlink and redirect the harness's writes (see
/// [`super::goal_tracker::ensure_goal_scratch_root`]).
pub(crate) const GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}.md";

/// Template for the per-attempt patch FILE NAME (rooted like
/// [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]). The captured diff is
/// written here and each skeptic reads it via its `read_file` tool
/// instead of receiving the body inline in its prompt.
#[cfg(test)]
pub(crate) const GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}.patch";

/// Wall-clock budget for the best-effort `git rev-parse HEAD` capture
/// during goal creation. The call must NEVER block goal creation; if
/// the workspace isn't a git repo or HEAD takes longer than this
/// (network filesystem, etc.) we drop the baseline and surface
/// `(unavailable)` to each skeptic — matching the verifier prompt's
/// rule 5.
const GIT_BASELINE_CAPTURE_TIMEOUT: Duration = Duration::from_secs(1);

/// Harness-owned role with read/search plus a kernel-sandboxed computation
/// shell. Its toolset has no mutation or subagent capabilities.
const GOAL_CLASSIFIER_SUBAGENT_TYPE: &str =
    ds_tools::implementations::ds_build::task::types::FINAL_VERIFIER_AGENT_TYPE;

/// Description shown in the pager subagent strip. Kept short — the
/// stage may spawn up to `GOAL_VERIFIER_SKEPTIC_MAX` skeptics per
/// attempt, but a stable label reads more cleanly in the strip than
/// a per-spawn suffix.
const GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION: &str = "goal achievement skeptic";

const GOAL_VERIFIER_PROMPT_TEMPLATE: &str = include_str!("templates/goal_verifier_prompt.md");

/// Default number of adversarial skeptics spawned per verification
/// attempt. Override via `DS_GOAL_VERIFIER_N` (clamped 1..=5) or the
/// remote `goal_verifier_count` setting. Every critic must return a valid
/// structured approval, so the default of three supplies independent coverage
/// without allowing one vote to be ignored.
pub(crate) const GOAL_VERIFIER_SKEPTIC_COUNT: u32 = 3;

/// Lower/upper bounds for `DS_GOAL_VERIFIER_N` / remote
/// `goal_verifier_count`. Five is the practical ceiling — any more is
/// pointless cost and saturates the subagent coordinator.
pub(crate) const GOAL_VERIFIER_SKEPTIC_MIN: u32 = 1;
pub(crate) const GOAL_VERIFIER_SKEPTIC_MAX: u32 = 5;

/// Expand a skeptic `pool` to a per-index assignment of length `n` via
/// round-robin (index `i` → `pool[i % pool.len()]`), reusing the frozen
/// `existing` prefix verbatim.
///
/// Resume stability + monotonic growth: committed indices are never
/// rewritten, so skeptic-0 always keeps `pool[0]` across resume AND
/// cold-fallback, and a later `n` bump only appends new indices (continuing
/// the round-robin, clamped by the caller). An empty `pool` keeps `existing`
/// unchanged (a frozen assignment survives a remote-cleared pool); empty
/// `existing` + empty `pool` ⇒ empty (all skeptics inherit the current
/// model). `n` is the CLAMPED skeptic count — identical to the value used at
/// the fan-out site — so the assignment never desyncs from the spawned
/// indices.
pub(crate) fn expand_skeptic_assignment(
    existing: &[crate::util::config::GoalRoleModel],
    pool: &[crate::util::config::GoalRoleModel],
    n: usize,
) -> Vec<crate::util::config::GoalRoleModel> {
    let mut out = existing.to_vec();
    if pool.is_empty() || out.len() >= n {
        return out;
    }
    for i in out.len()..n {
        out.push(pool[i % pool.len()].clone());
    }
    out
}

/// Per-skeptic JSON verdict FILE NAME template (rooted under the
/// per-goal scratch root like [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]).
/// The harness reads each skeptic's JSON to drive the aggregation; the
/// terminal token is the fast-path signal but the JSON is authoritative.
#[cfg(test)]
pub(crate) const GOAL_VERIFIER_VERDICT_PATH_TEMPLATE: &str =
    "goal-verdict-{verifier_id}-{attempt}-{skeptic_idx}.json";

/// Per-skeptic Markdown details FILE NAME template (rooted like
/// [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]). Each skeptic writes its
/// own analysis here; the harness concatenates them into the canonical
/// `GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE` path the existing ack
/// contract surfaces.
#[cfg(test)]
pub(crate) const GOAL_VERIFIER_DETAILS_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}-skeptic-{skeptic_idx}.md";

// ── Soft spawn backpressure ────────────────────────────────────────────

/// Fraction of the live context window at which new skeptic spawns are
/// deferred (soft backpressure). Above `live_subagent_tokens >= FRACTION *
/// live_context_window` the panel waits (polling) before spawning the next
/// skeptic; it never rejects a spawn (no hard caps) and never waits longer
/// than [`GOAL_SPAWN_BACKPRESSURE_MAX_WAIT`].
pub(crate) const GOAL_SPAWN_BACKPRESSURE_FRACTION: f64 = 0.8;

/// Poll interval while waiting for subagent token pressure to drop.
pub(crate) const GOAL_SPAWN_BACKPRESSURE_POLL: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Upper bound on a single soft-backpressure wait. After this the spawn
/// proceeds anyway — backpressure defers, it never blocks the goal.
pub(crate) const GOAL_SPAWN_BACKPRESSURE_MAX_WAIT: std::time::Duration =
    std::time::Duration::from_secs(30);

/// Soft spawn backpressure for the skeptic panel.
///
/// `is_over` reports whether live subagent token burn exceeds the
/// threshold; [`Self::wait_soft`] polls it before a spawn and returns once
/// pressure clears or the bounded wait expires (soft: the spawn always
/// proceeds eventually — no hard caps).
pub(crate) struct SpawnBackpressure {
    pub(crate) is_over: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    /// Poll interval (overridable in tests; production uses
    /// [`GOAL_SPAWN_BACKPRESSURE_POLL`]).
    pub(crate) poll: std::time::Duration,
}

impl SpawnBackpressure {
    pub(crate) fn new(is_over: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            is_over,
            poll: GOAL_SPAWN_BACKPRESSURE_POLL,
        }
    }

    pub(crate) async fn wait_soft(&self) {
        let deadline = std::time::Instant::now() + GOAL_SPAWN_BACKPRESSURE_MAX_WAIT;
        while (self.is_over)() && std::time::Instant::now() < deadline {
            tokio::time::sleep(self.poll).await;
        }
    }
}

// Outcome + spawner abstraction

/// Result of one classifier attempt. `Achieved` / `NotAchieved` are
/// verdict-class outcomes: every critic produced a usable structured record.
/// `FailOpenAchieved` is the legacy name for an infrastructure-class outcome;
/// callers always pause without approval. Missing or malformed structured
/// records reach that infrastructure outcome after the panel emits its
/// diagnostics.
#[derive(Debug, Clone)]
pub(crate) enum GoalClassifierOutcome {
    Achieved {
        details_path: String,
    },
    NotAchieved {
        details_path: String,
        /// One-line-per-refuter gist inlined into the rejection nudge so
        /// a weak model sees the actionable gaps without a file read (see
        /// [`build_gaps_summary`]). Never empty for a real rejection
        /// (≥1 refuter).
        gaps_summary: String,
        /// Blocker bullets grouped by [`SkepticBlocking`] class for the
        /// user-facing auto-pause message (see [`build_pause_summary`]).
        pause_summary: String,
        /// Stall fingerprint computed at the SOURCE from the raw
        /// (undecorated, log-path-free) gap evidence via
        /// [`gap_fingerprint`]; the drain compares it across attempts.
        gap_fingerprint: String,
    },
    /// Every refuter classified its gap as a contradiction or
    /// environment-unverifiable blocker — no model-fixable gap remains,
    /// so iterating cannot help. The goal pauses for a user decision
    /// rather than receiving another retry nudge. No stall fingerprint
    /// is carried — the drain resets the streak when routing here.
    Blocked {
        details_path: String,
        /// Grouped blocker bullets (all non-model-fixable) used as the
        /// user-facing pause message.
        pause_summary: String,
    },
    FailOpenAchieved {
        reason: GoalClassifierFailOpenReason,
        /// Empty when the failure happened before path resolution
        /// (e.g. an unsafe path was rejected by the validator).
        details_path: String,
    },
}

/// Subagent spawn abstraction. Production uses [`ChannelSpawner`];
/// tests use [`MockSpawner`].
#[async_trait::async_trait]
pub(crate) trait GoalClassifierSpawner: Send + Sync {
    /// Spawn under `id` and return the terminal response when the subagent
    /// finishes. `resume_from`, when `Some`, names a previously-completed
    /// subagent session whose transcript / tool-state / model the new
    /// child inherits (used to resume skeptic 0 across attempts).
    async fn spawn_classifier(
        &self,
        id: &str,
        skeptic_idx: u32,
        prompt: RoleRenderedPrompt,
        details_path: &Path,
        reviewed_root: &Path,
        resume_from: Option<&str>,
    ) -> Result<String, SpawnError>;
}

/// Spawn-time error. Distinguishes between transport errors (channel
/// closed, coordinator unreachable) and runtime errors (subagent
/// reported failure, was cancelled, etc.) so the runner can map them
/// to the correct fail-open reason.
#[derive(Debug)]
pub(crate) enum SpawnError {
    /// Subagent coordinator was unreachable (channel closed, no
    /// `subagent_event_tx` plumbed). Maps to `SamplerError`.
    Transport(String),
    /// Subagent ran but reported failure. `cancelled: true` maps to
    /// [`GoalClassifierFailOpenReason::Aborted`]; `cancelled: false`
    /// maps to [`GoalClassifierFailOpenReason::SamplerError`].
    Runtime { message: String, cancelled: bool },
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(d) => write!(f, "subagent transport error: {d}"),
            Self::Runtime { message, cancelled } => {
                write!(
                    f,
                    "subagent runtime error (cancelled={cancelled}): {message}"
                )
            }
        }
    }
}

impl crate::session::goal_planner::RetryableSpawnError for SpawnError {
    fn is_cancelled(&self) -> bool {
        matches!(
            self,
            SpawnError::Runtime {
                cancelled: true,
                ..
            }
        )
    }
}

// Path resolution + validation

/// Root a substituted classifier file name under the goal's private
/// scratch root. Single seam for every classifier artifact path so the
/// owner-only-directory invariant cannot drift per call site.
fn scratch_rooted(verifier_id: &str, file_name: String) -> String {
    super::goal_tracker::goal_scratch_root(verifier_id)
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

/// Substitute the `{verifier_id}` / `{attempt}` placeholders in
/// `GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE` and root the result under
/// the goal's scratch root. Pure string ops; no I/O.
pub(crate) fn format_details_path(verifier_id: &str, attempt: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string()),
    )
}

/// Return the aggregate details path for one immutable verification round.
/// Attempt counters reset on resume, so they are deliberately not used as the
/// freshness identity for real panel output.
fn format_round_panel_details_path(verifier_id: &str, round_id: &str) -> String {
    scratch_rooted(
        verifier_id,
        format!("goal-classifier-{verifier_id}-{round_id}.md"),
    )
}

/// Substitute placeholders in `GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE`
/// and root the result under the goal's scratch root.
#[cfg(test)]
pub(crate) fn format_changes_path(verifier_id: &str, attempt: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string()),
    )
}

/// Errors classifying a candidate details-file path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PathValidationError {
    /// Path contains `..`, a NUL byte, or starts in a forbidden
    /// system prefix (`/etc`, `/proc`, `/sys`, `/dev`, `~`).
    UnsafeComponent,
    /// Path contains an unresolved `${...}` / `{...}` substitution
    /// marker other than the known classifier placeholders.
    UnresolvedSubstitution,
    /// Resolved path is outside the platform temp dir the classifier
    /// roots its artifacts under.
    OutsideAllowedPrefix,
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeComponent => f.write_str("path contains an unsafe component"),
            Self::UnresolvedSubstitution => f.write_str("path contains unresolved substitution"),
            Self::OutsideAllowedPrefix => f.write_str("path is outside the allowed temp root"),
        }
    }
}

/// Validate the resolved classifier details-file path against the
/// platform temp dir (the goal scratch root's parent, where
/// `format_*_path` roots every artifact). No bare-`/tmp` allowance:
/// every production caller validates a freshly `format_*_path`-built
/// path. See [`validate_details_path_in_root`] for the rules.
pub(crate) fn validate_details_path(path: &Path) -> Result<(), PathValidationError> {
    validate_details_path_in_root(path, &std::env::temp_dir())
}

/// Root-injectable core of [`validate_details_path`], so the
/// allowed-prefix rule is unit-testable on every platform (on Linux
/// `temp_dir()` IS `/tmp`). String-structural only; symlink resistance
/// comes from the owner-only (0700) scratch root.
pub(crate) fn validate_details_path_in_root(
    path: &Path,
    temp_root: &Path,
) -> Result<(), PathValidationError> {
    let s = path.to_string_lossy();
    // Cheap structural checks first — these don't require any I/O.
    if s.contains("..") || s.contains('\0') {
        return Err(PathValidationError::UnsafeComponent);
    }
    for prefix in &["/etc", "/proc", "/sys", "/dev"] {
        if s.starts_with(prefix) {
            return Err(PathValidationError::UnsafeComponent);
        }
    }
    if s.starts_with('~') {
        return Err(PathValidationError::UnsafeComponent);
    }
    // Substitution markers other than the known classifier placeholders.
    // The runner substitutes `{verifier_id}` / `{attempt}` BEFORE
    // validation, so any remaining `{...}` is an error.
    if s.contains("${") || s.contains('{') || s.contains('}') {
        return Err(PathValidationError::UnresolvedSubstitution);
    }
    // Allowed prefix — the platform temp dir (on macOS this is
    // /var/folders/..., not /tmp). Extend this check for future
    // session-dir overrides without changing the failure-class taxonomy.
    if !path.starts_with(temp_root) {
        return Err(PathValidationError::OutsideAllowedPrefix);
    }
    Ok(())
}

// Terminal-token parse

/// Parse an adversarial skeptic's terminal response. `Refuted`
/// ⇒ `Some(true)`, `Not Refuted` ⇒ `Some(false)`. The JSON verdict
/// file is authoritative when present; the terminal token is the
/// fast-path signal for the skeptic's vote when JSON parsing fails.
///
/// Tolerates code fences/backticks and a trailing `.`/`!` around the
/// token, but the response must contain ONLY the token — any other
/// prose stays `None`.
pub(crate) fn parse_skeptic_terminal_response(text: &str) -> Option<bool> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        // Drop fence lines entirely, including language-tagged ones
        // ("```text") that backtick-trimming alone would leave behind.
        .filter(|l| !l.starts_with("```"))
        .map(|l| l.trim_matches('`').trim_end_matches(['.', '!']).trim())
        .filter(|l| !l.is_empty())
        .collect();
    match lines.as_slice() {
        ["Refuted"] => Some(true),
        ["Not Refuted"] => Some(false),
        _ => None,
    }
}

// Git baseline capture (called from `setup_goal`)

/// Best-effort `git rev-parse HEAD` capture for goal creation.
///
/// Returns the commit SHA on success; `None` for any failure
/// (workspace is not a git repo, `git` is not installed, HEAD has
/// no commits, the call timed out). NEVER blocks goal creation —
/// the wall-clock budget is bounded by `GIT_BASELINE_CAPTURE_TIMEOUT`
/// and the caller treats `None` as the documented "no baseline"
/// signal (each skeptic renders `CHANGES_FILE: (unavailable)` and the
/// verifier prompt's rule 5 takes over).
pub(crate) async fn capture_git_baseline(workspace_root: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(evidence::git_bin());
    cmd.arg("rev-parse").arg("HEAD").current_dir(workspace_root);

    let output = match tokio::time::timeout(GIT_BASELINE_CAPTURE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::debug!(
                error = %err,
                "goal baseline capture: failed to spawn git rev-parse",
            );
            return None;
        }
        Err(_) => {
            tracing::debug!("goal baseline capture: git rev-parse exceeded budget");
            return None;
        }
    };
    if !output.status.success() {
        tracing::debug!(
            exit = ?output.status.code(),
            "goal baseline capture: git rev-parse non-zero exit",
        );
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return None;
    }
    Some(sha)
}

// Trace-only recording for harness-spawned subagents

/// Build the synthetic `task` tool_call + tool_result pair for a
/// harness-spawned subagent, shaped like a model-issued `task` spawn.
///
/// The tool_result MUST carry the real task tool's `<subagent_result>` footer
/// (via [`ds_tool_types::format_resume_footer`]): trace tooling
/// discovers subagents by scanning tool_result bodies for that
/// `subagent_id:` block, so without it the harness subagent never shows in the
/// session tree. The footer id equals the child session id, so the viewer can
/// fetch its uploaded trace.
pub(crate) fn build_subagent_trace_items(
    task_tool_name: &str,
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    prompt: &str,
    output: &str,
) -> Vec<ds_sampling_types::conversation::ConversationItem> {
    use ds_sampling_types::conversation::{ConversationItem, ToolCall};
    let arguments = serde_json::json!({
        "description": description,
        "subagent_type": subagent_type,
        "prompt": prompt,
    })
    .to_string();
    let call = ConversationItem::assistant_tool_calls(vec![ToolCall {
        id: std::sync::Arc::from(subagent_id),
        name: task_tool_name.to_string(),
        arguments: std::sync::Arc::from(arguments),
    }]);
    let footer = ds_tool_types::format_resume_footer(subagent_id, subagent_type, None);
    let result = ConversationItem::tool_result(subagent_id, format!("{output}\n\n{footer}"));
    vec![call, result]
}

/// Record a harness-spawned subagent into the in-progress harness trace phase
/// as a synthetic `task` call (see [`build_subagent_trace_items`]). The items
/// accumulate in a side buffer (never the live model context); the caller seals
/// the phase via [`ds_chat_state::ChatStateHandle::flush_harness_trace_turn`]
/// so it uploads as its own sibling `turn_{N}` artifact. No-op when tracing is
/// off (`sink` absent) or no prompt was captured. `sink` carries the chat-state
/// handle and the resolved `task` tool name.
pub(crate) fn record_subagent_trace(
    sink: Option<&(ds_chat_state::ChatStateHandle, String)>,
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    prompt: Option<&str>,
    output: &str,
) {
    if let (Some((handle, task_tool)), Some(prompt)) = (sink, prompt) {
        handle.append_harness_trace_items(build_subagent_trace_items(
            task_tool,
            subagent_id,
            subagent_type,
            description,
            prompt,
            output,
        ));
    }
}

// Production spawner — wraps the subagent coordinator channel

/// Production spawner. Sends a `SubagentEvent::Spawn` to the session's
/// coordinator and awaits the result on a fresh oneshot. The parent model
/// never sees the spawn live — it is direct (no `task` tool call). When a
/// `trace_sink` is wired, each skeptic is recorded as a synthetic `task` call
/// (see [`record_subagent_trace`]) into the harness trace phase; the caller
/// seals the panel into its own sibling trace turn so the subagents are
/// discoverable in data collection.
pub(crate) struct ChannelSpawner {
    pub(crate) event_tx: tokio::sync::mpsc::UnboundedSender<
        ds_tools::implementations::ds_build::task::types::SubagentEvent,
    >,
    pub(crate) parent_session_id: String,
    pub(crate) parent_prompt_id: Option<String>,
    pub(crate) cwd: Option<String>,
    /// Trace-artifact sink + the resolved `task` tool name. `None` disables
    /// trace recording (tests, or sessions without trace capture).
    pub(crate) trace_sink: Option<(ds_chat_state::ChatStateHandle, String)>,
    /// Per-skeptic-index resolved model+toolset override, indexed by
    /// `skeptic_idx`. An out-of-range index (or `Default`) inherits the
    /// current model — round-robin expansion + auth/capability fail-open is
    /// resolved parent-side before the spawner is built.
    pub(crate) skeptic_overrides: Vec<RoleSpawnOverride>,
    /// `/goal` orchestration metadata surfaced on the wire: phase + 1-based
    /// attempt/round. Set by the caller that knows the current round.
    pub(crate) goal_phase: Option<&'static str>,
    pub(crate) goal_attempt: Option<u32>,
}

#[async_trait::async_trait]
impl GoalClassifierSpawner for ChannelSpawner {
    async fn spawn_classifier(
        &self,
        id: &str,
        skeptic_idx: u32,
        prompt: RoleRenderedPrompt,
        details_path: &Path,
        reviewed_root: &Path,
        resume_from: Option<&str>,
    ) -> Result<String, SpawnError> {
        // Clone the primary render for the trace pair only when tracing; the
        // wrapper moves each render into its attempt (no other clone).
        let trace_prompt = self.trace_sink.as_ref().map(|_| prompt.primary.clone());
        // Per-index override; out-of-range ⇒ inherit (defensive).
        let inherit = RoleSpawnOverride::default();
        let override_ = self
            .skeptic_overrides
            .get(skeptic_idx as usize)
            .unwrap_or(&inherit);
        // Do not use the general role retry wrapper here. A second spawn under
        // the same verdict path could consume files or tool events left by the
        // first spawn, defeating the round-freshness guarantee. Any verifier
        // runtime failure therefore fails this round closed.
        let (model, harness) = if override_.is_explicit() {
            (override_.model.clone(), override_.agent_type.clone())
        } else {
            (None, None)
        };
        let outcome = self
            .send_one(
                id,
                prompt.primary,
                model,
                harness,
                resume_from,
                details_path,
                reviewed_root,
            )
            .await;

        match &outcome {
            Ok(text) => record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_CLASSIFIER_SUBAGENT_TYPE,
                GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                text,
            ),
            Err(SpawnError::Runtime { message, .. }) => record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_CLASSIFIER_SUBAGENT_TYPE,
                GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                message,
            ),
            Err(SpawnError::Transport(_)) => {}
        }
        outcome
    }
}

impl ChannelSpawner {
    /// Send one skeptic spawn (model + harness override resolved by the caller)
    /// and await its terminal result. Final verification is single-spawn: a
    /// failed configured verifier cannot be replayed under the same identity.
    /// The
    /// subagent_type is always [`GOAL_CLASSIFIER_SUBAGENT_TYPE`];
    /// `harness_agent_type` selects the harness flavor (`None` ⇒ session
    /// harness).
    async fn send_one(
        &self,
        id: &str,
        prompt: String,
        model: Option<String>,
        harness_agent_type: Option<String>,
        resume_from: Option<&str>,
        details_path: &Path,
        reviewed_root: &Path,
    ) -> Result<String, SpawnError> {
        use ds_tools::implementations::ds_build::task::types::{
            SubagentEvent, SubagentRequest, SubagentRuntimeOverrides,
        };
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let reviewed_root = reviewed_root.to_path_buf();
        let scratch_root = details_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                SpawnError::Transport("final verifier scratch root is missing".into())
            })?;
        let request = SubagentRequest {
            id: id.to_string(),
            prompt,
            description: GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION.to_string(),
            subagent_type: GOAL_CLASSIFIER_SUBAGENT_TYPE.to_string(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            resume_from: resume_from.map(str::to_string),
            cwd: self.cwd.clone(),
            runtime_overrides: SubagentRuntimeOverrides {
                model,
                harness_agent_type,
                verifier_sandbox: Some(
                    ds_tools::implementations::ds_build::task::types::VerifierSandboxSpec {
                        reviewed_root,
                        scratch_root,
                    },
                ),
                ..Default::default()
            },
            run_in_background: Some(false),
            // Harness-internal: never surface to the model's idle reminder.
            surface_completion: false,
            fork_context: false,
            goal_phase: self.goal_phase.map(str::to_string),
            goal_attempt: self.goal_attempt,
            result_tx,
        };
        if self
            .event_tx
            .send(SubagentEvent::Spawn(Box::new(request)))
            .is_err()
        {
            return Err(SpawnError::Transport(
                "subagent coordinator channel closed".to_string(),
            ));
        }
        let result = result_rx
            .await
            .map_err(|_| SpawnError::Transport("subagent result channel dropped".to_string()))?;
        if !result.success {
            let message = result.error.unwrap_or_else(|| "unknown error".to_string());
            return Err(SpawnError::Runtime {
                message,
                cancelled: result.cancelled,
            });
        }
        Ok(result.output.to_string())
    }
}

// Fail-open helper (shared by verification stage)

/// Record an infrastructure-failure outcome: emit telemetry, write a
/// placeholder details file (when the path is resolved), and return the legacy
/// wire variant consumed by the always-fail-closed apply path. Empty
/// `details_raw` skips the write.
async fn record_fail_open(
    reason: GoalClassifierFailOpenReason,
    attempt: u32,
    started: std::time::Instant,
    emit_event: &dyn Fn(Event),
    details_path: Option<&Path>,
    details_raw: String,
) -> GoalClassifierOutcome {
    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalClassifierFailOpen {
        reason: reason.as_const_str(),
        attempt,
        latency_ms,
    });
    let resolved_path = match details_path {
        // Surface the path only when the placeholder is on disk — a failed
        // write would point the user at a missing file (empty = no details).
        Some(p) if maybe_write_fail_open_placeholder(p, reason).await => details_raw,
        _ => String::new(),
    };
    GoalClassifierOutcome::FailOpenAchieved {
        reason,
        details_path: resolved_path,
    }
}

/// Write `body` to `path` atomically via tempfile + rename. The
/// tempfile sits next to the target so `rename` stays on one FS.
pub(crate) async fn write_patch_file_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    // Scratch-rooted paths always have a parent; a rootless path is a bug.
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("patch path has no parent directory"));
    };
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("goal-classifier.patch");
    let tmp = dir.join(format!(".{file_name}.{}.tmp", uuid::Uuid::now_v7()));
    tokio::fs::write(&tmp, body).await?;
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    Ok(())
}

/// Write a placeholder file at `path` unless a non-empty file is
/// already there. `headline` becomes the Markdown `# <headline>`
/// header; `body` is appended verbatim. Best-effort.
///
/// Returns `true` when a non-empty details file exists at `path`
/// afterward (it already did, or the write succeeded) and `false` when
/// the write was attempted and failed — so the caller never surfaces a
/// path to a file that isn't there.
async fn maybe_write_classifier_placeholder(path: &Path, headline: &str, body: &str) -> bool {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_file() && meta.len() > 0 => return true,
        // Never bless or write through a pre-existing symlink, directory, or
        // other unexpected entry in the scratch namespace.
        Ok(_) => return false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    let content = format!("# {headline}\n\n{body}\n");
    match write_patch_file_atomic(path, &content).await {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "goal classifier: failed to write placeholder",
            );
            false
        }
    }
}

/// Returns `true` iff the placeholder is on disk afterward (see
/// [`maybe_write_classifier_placeholder`]).
async fn maybe_write_fail_open_placeholder(
    path: &Path,
    reason: GoalClassifierFailOpenReason,
) -> bool {
    let reason_str = reason.as_const_str();
    let body = format!(
        "The verification stage did not produce a verdict (infra-class \
         failure). The goal was not approved; the harness pauses until a fresh \
         verification round can run. No skeptic analysis was captured.\n\n\
         ## Reason\n\n{reason_str}"
    );
    maybe_write_classifier_placeholder(
        path,
        &format!("Verification infrastructure failure: {reason_str}"),
        &body,
    )
    .await
}

// Verifier — the adversarial skeptic panel

/// Confidence label on a skeptic verdict. The JSON wire vocabulary is
/// `high|medium|low`; any other (or missing) value normalises to
/// `Unknown` so a verifier with a botched JSON field still produces an
/// aggregable vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SkepticConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

impl SkepticConfidence {
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Unknown,
        }
    }
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Unknown => "unknown",
        }
    }

    /// Sort key for the inlined gaps summary: high-confidence refuters
    /// surface first (`High` → 0 … `Unknown` → 3).
    fn rank(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
            Self::Unknown => 3,
        }
    }
}

/// Classification of a refutation's blocker. `None` is an ordinary
/// model-fixable gap (the default — absent or unrecognised wire values
/// normalise here, keeping the JSON contract back-compatible).
/// `Contradiction` flags an objective/plan internal conflict;
/// `Unverifiable` flags evidence that is infeasible to capture in the
/// current environment. A rejection whose refuters are *all* non-`None`
/// cannot progress by iterating and routes to the blocked outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SkepticBlocking {
    #[default]
    None,
    Contradiction,
    Unverifiable,
}

impl SkepticBlocking {
    /// Test helper: maps a label to a blocking class (unknown → `None`).
    /// Production parsing rejects unknown labels at the verdict boundary.
    #[cfg(test)]
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "contradiction" => Self::Contradiction,
            "unverifiable" => Self::Unverifiable,
            _ => Self::None,
        }
    }
    fn is_blocking(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Parsed skeptic verdict — JSON shape mirrors the verifier prompt's
/// contract. `evidence` and `details_md` are kept for the aggregated
/// details file; the harness operates on `refuted` + `confidence` +
/// `blocking`.
/// One concise verifier finding (the implementer-facing gap list). Fields
/// default to empty for weak-model robustness; an all-empty finding is
/// dropped at parse time.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Finding {
    /// `bug` | `gap` | `todo` (rendered verbatim after trim).
    #[serde(default)]
    pub kind: String,
    /// `path:line` when code-related, else a short place; may be empty.
    #[serde(default)]
    pub location: String,
    /// One-line description.
    #[serde(default)]
    pub detail: String,
}

impl Finding {
    fn is_empty(&self) -> bool {
        self.kind.trim().is_empty()
            && self.location.trim().is_empty()
            && self.detail.trim().is_empty()
    }
}

/// Correctness requirements compose: a hybrid task can activate several
/// independent verifier lenses and every active facet must pass.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VerificationFacet {
    Code,
    Analysis,
    Research,
    Math,
    Empirical,
    Sources,
    Citations,
    DocumentRender,
    StateRegression,
}

impl VerificationFacet {
    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Analysis => "analysis",
            Self::Research => "research",
            Self::Math => "math",
            Self::Empirical => "empirical",
            Self::Sources => "sources",
            Self::Citations => "citations",
            Self::DocumentRender => "document-render",
            Self::StateRegression => "state-regression",
        }
    }
}

/// One artifact-bound validation receipt. The compact schema deliberately
/// records only evidence the harness can validate mechanically.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidationCheck {
    pub gate: String,
    pub facet: String,
    pub status: String,
    pub target: String,
    pub evidence: String,
    pub artifact_path: String,
    pub artifact_sha256: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_input_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_output_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicability_basis: Option<String>,
}

fn canonical_math_gate(raw: &str) -> Option<&'static str> {
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_', '/'], "-");
    match normalized.as_str() {
        "contract-closure" | "contract-closure-validation" => Some("contract-closure"),
        "derivation-integrity" | "derivation-integrity-validation" => Some("derivation-integrity"),
        "evidence-provenance" | "evidence-provenance-validation" => Some("evidence-provenance"),
        "invariant-ledger" | "invariant-ledger-validation" => Some("invariant-ledger"),
        "state-isolation"
        | "state-isolation-validation"
        | "state-isolation-artifact-freezing"
        | "state-isolation-and-artifact-freezing" => Some("state-isolation"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SkepticVerdict {
    pub verdict_schema_version: u32,
    pub goal_id: String,
    pub verification_round_id: String,
    pub contract_digest: String,
    pub reviewed_artifact_manifest_digest: String,
    pub critic_id: String,
    pub critic_assignment_id: String,
    pub refuted: bool,
    pub evidence: String,
    pub confidence: SkepticConfidence,
    pub blocking: SkepticBlocking,
    pub details_md: String,
    /// Structured findings (the implementer-facing gap list); empty when
    /// the verifier emitted none (then the `evidence` fallback is used).
    pub findings: Vec<Finding>,
    /// Complete structured coverage for every active facet.
    pub checks: Vec<ValidationCheck>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkepticVerdictRaw {
    verdict_schema_version: u32,
    goal_id: String,
    verification_round_id: String,
    contract_digest: String,
    reviewed_artifact_manifest_digest: String,
    critic_id: String,
    critic_assignment_id: String,
    refuted: bool,
    evidence: String,
    confidence: String,
    #[serde(default)]
    blocking: Option<String>,
    #[serde(default)]
    details_md: Option<String>,
    #[serde(default)]
    findings: Option<Vec<Finding>>,
    checks: Vec<ValidationCheck>,
}

/// Parse the JSON body the skeptic wrote to its `{VERDICT_FILE}`.
///
/// Every identity, coverage, and verdict field is mandatory.
/// A missing or empty `evidence` field rejects (`None`) — without
/// evidence the rubber-stamp failure mode this contract explicitly
/// closes is back open. `details_md` is optional (it's a harness-side
/// extension to the schema; the aggregator prefers the on-disk
/// per-skeptic report and uses this JSON field only as a fallback when
/// that file is missing/empty). Unknown fields and enum values are rejected.
/// The skeptic-level fallback (`run_one_skeptic`) maps any
/// `None` here to a synthetic `refuted: true` vote.
pub(crate) fn parse_verdict_json(body: &str) -> Option<SkepticVerdict> {
    let raw: SkepticVerdictRaw = serde_json::from_str(body.trim()).ok()?;
    if raw.verdict_schema_version != 1
        || raw.goal_id.trim().is_empty()
        || raw.verification_round_id.trim().is_empty()
        || raw.contract_digest.trim().is_empty()
        || raw.reviewed_artifact_manifest_digest.trim().is_empty()
        || raw.critic_id.trim().is_empty()
        || raw.critic_assignment_id.trim().is_empty()
        || raw.evidence.trim().is_empty()
    {
        return None;
    }
    let confidence = SkepticConfidence::parse(&raw.confidence);
    if confidence == SkepticConfidence::Unknown {
        return None;
    }
    let blocking = match raw.blocking.as_deref() {
        None | Some("none") => SkepticBlocking::None,
        Some("contradiction") => SkepticBlocking::Contradiction,
        Some("unverifiable") => SkepticBlocking::Unverifiable,
        Some(_) => return None,
    };
    let findings = raw
        .findings
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.is_empty())
        .collect();
    Some(SkepticVerdict {
        verdict_schema_version: raw.verdict_schema_version,
        goal_id: raw.goal_id,
        verification_round_id: raw.verification_round_id,
        contract_digest: raw.contract_digest,
        reviewed_artifact_manifest_digest: raw.reviewed_artifact_manifest_digest,
        critic_id: raw.critic_id,
        critic_assignment_id: raw.critic_assignment_id,
        refuted: raw.refuted,
        evidence: raw.evidence,
        confidence,
        blocking,
        details_md: raw.details_md.unwrap_or_default(),
        findings,
        checks: raw.checks,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
struct VerdictIdentity {
    goal_id: String,
    verification_round_id: String,
    contract_digest: String,
    reviewed_artifact_manifest_digest: String,
    critic_id: String,
    critic_assignment_id: String,
}

fn gates_for_facet(facet: VerificationFacet) -> &'static [&'static str] {
    match facet {
        VerificationFacet::Code => &["code-correctness"],
        VerificationFacet::Analysis => &["analysis-correctness"],
        VerificationFacet::Research => &["research-validity"],
        VerificationFacet::Math => &MATH_VALIDATION_GATES,
        VerificationFacet::Empirical => &["empirical-validity"],
        VerificationFacet::Sources => &["source-support"],
        VerificationFacet::Citations => &["citation-integrity"],
        VerificationFacet::DocumentRender => &["document-render"],
        VerificationFacet::StateRegression => &["state-isolation"],
    }
}

fn canonical_facet(raw: &str) -> Option<VerificationFacet> {
    match raw
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_'], "-")
        .as_str()
    {
        "code" | "code-change" => Some(VerificationFacet::Code),
        "analysis" => Some(VerificationFacet::Analysis),
        "research" => Some(VerificationFacet::Research),
        "math" | "mathematics" | "physics" => Some(VerificationFacet::Math),
        "empirical" | "statistics" => Some(VerificationFacet::Empirical),
        "sources" | "source" => Some(VerificationFacet::Sources),
        "citations" | "citation" => Some(VerificationFacet::Citations),
        "document-render" | "render" => Some(VerificationFacet::DocumentRender),
        "state-regression" | "regression" => Some(VerificationFacet::StateRegression),
        _ => None,
    }
}

fn canonical_gate(facet: VerificationFacet, raw: &str) -> Option<&'static str> {
    if facet == VerificationFacet::Math {
        return canonical_math_gate(raw);
    }
    let normalized = raw.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    gates_for_facet(facet)
        .iter()
        .copied()
        .find(|gate| *gate == normalized)
}

fn validate_structured_verdict(
    verdict: &SkepticVerdict,
    expected: &VerdictIdentity,
    facets: &std::collections::BTreeSet<VerificationFacet>,
    reviewed_root: &Path,
    manifest: &super::verification_snapshot::ArtifactManifest,
    trace: &[super::verifier_runtime::VerificationToolEvent],
) -> Result<(), String> {
    for (label, actual, wanted) in [
        (
            "goal_id",
            verdict.goal_id.as_str(),
            expected.goal_id.as_str(),
        ),
        (
            "verification_round_id",
            verdict.verification_round_id.as_str(),
            expected.verification_round_id.as_str(),
        ),
        (
            "contract_digest",
            verdict.contract_digest.as_str(),
            expected.contract_digest.as_str(),
        ),
        (
            "reviewed_artifact_manifest_digest",
            verdict.reviewed_artifact_manifest_digest.as_str(),
            expected.reviewed_artifact_manifest_digest.as_str(),
        ),
        (
            "critic_id",
            verdict.critic_id.as_str(),
            expected.critic_id.as_str(),
        ),
        (
            "critic_assignment_id",
            verdict.critic_assignment_id.as_str(),
            expected.critic_assignment_id.as_str(),
        ),
    ] {
        if actual != wanted {
            return Err(format!("structured verdict has wrong {label}"));
        }
    }
    if verdict.verdict_schema_version != 1 {
        return Err("structured verdict has an unsupported schema version".to_string());
    }

    let required: std::collections::BTreeSet<_> = facets
        .iter()
        .flat_map(|facet| {
            gates_for_facet(*facet)
                .iter()
                .map(move |gate| (*facet, *gate))
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut facet_has_decision = std::collections::BTreeSet::new();
    let mut failures = 0_usize;
    for check in &verdict.checks {
        let facet = canonical_facet(&check.facet)
            .ok_or_else(|| format!("unknown correctness facet `{}`", check.facet.trim()))?;
        let gate = canonical_gate(facet, &check.gate)
            .ok_or_else(|| format!("unknown gate `{}` for facet {}", check.gate, check.facet))?;
        if !required.contains(&(facet, gate)) {
            return Err(format!("unassigned receipt {}/{gate}", facet.as_str()));
        }
        if !seen.insert((facet, gate)) {
            return Err(format!("duplicate receipt {}/{gate}", facet.as_str()));
        }
        if check.target.trim().is_empty()
            || check.evidence.trim().is_empty()
            || check.method.trim().is_empty()
        {
            return Err(format!("receipt {}/{gate} is incomplete", facet.as_str()));
        }
        let status = check
            .status
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_");
        match status.as_str() {
            "not_applicable" | "n/a" => {
                if !check.artifact_path.trim().is_empty()
                    || !check.artifact_sha256.trim().is_empty()
                    || check.tool_event_id.is_some()
                    || check.exact_input_digest.is_some()
                    || check.observed_output_digest.is_some()
                    || check
                        .applicability_basis
                        .as_deref()
                        .is_none_or(|reason| reason.trim().len() < 12)
                {
                    return Err(format!(
                        "receipt {}/{gate} has an invalid not-applicable basis",
                        facet.as_str()
                    ));
                }
            }
            "pass" | "fail" => {
                facet_has_decision.insert(facet);
                if status == "fail" {
                    failures += 1;
                }
                if !super::verification_snapshot::entry_matches(
                    manifest,
                    &check.artifact_path,
                    &check.artifact_sha256,
                ) {
                    return Err(format!(
                        "receipt {}/{gate} cites an unknown artifact revision",
                        facet.as_str()
                    ));
                }
                if !artifact_contains_target(reviewed_root, &check.artifact_path, &check.target)? {
                    return Err(format!(
                        "receipt {}/{gate} target is not present in its cited artifact",
                        facet.as_str()
                    ));
                }
                match (
                    check.tool_event_id.as_deref(),
                    check.exact_input_digest.as_deref(),
                    check.observed_output_digest.as_deref(),
                ) {
                    (None, None, None) => {
                        // Only an APPROVAL of math evidence-provenance must
                        // bind a live, successful tool event. A REFUTATION
                        // (`fail`) is itself the finding that the evidence is
                        // missing / unreproducible, so it cannot be required to
                        // produce the very tool event the implementer failed to
                        // capture — requiring it turned every legitimate
                        // evidence-provenance refutation into a spurious
                        // "infrastructure failure" (fail-closed pause).
                        if status == "pass"
                            && facet == VerificationFacet::Math
                            && gate == "evidence-provenance"
                        {
                            return Err(
                                "math evidence-provenance requires a successful current-round tool event"
                                    .to_string(),
                            );
                        }
                    }
                    (Some(event), Some(input), Some(output)) => {
                        if !super::verifier_runtime::trace_contains(
                            trace,
                            event,
                            input,
                            output,
                            &check.artifact_path,
                            &check.target,
                        ) {
                            return Err(format!(
                                "receipt {}/{gate} cites a missing, failed, or stale tool event",
                                facet.as_str()
                            ));
                        }
                    }
                    _ => {
                        return Err(format!(
                            "receipt {}/{gate} has a partial tool-event binding",
                            facet.as_str()
                        ));
                    }
                }
                let method = check.method.to_ascii_lowercase();
                if (method.contains("numerical") || method.contains("approx"))
                    && check
                        .tolerance
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(format!(
                        "receipt {}/{gate} uses a numerical method without tolerance",
                        facet.as_str()
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "receipt {}/{gate} has invalid status",
                    facet.as_str()
                ));
            }
        }
    }
    if seen != required {
        return Err(format!(
            "structured verdict coverage is incomplete: covered={seen:?}, required={required:?}"
        ));
    }
    if facets
        .iter()
        .any(|facet| !facet_has_decision.contains(facet))
    {
        return Err("a correctness facet cannot be entirely not-applicable".to_string());
    }
    if verdict.refuted {
        if failures == 0 || verdict.findings.is_empty() {
            return Err(
                "refutation requires a failed receipt and a structured finding".to_string(),
            );
        }
    } else {
        if failures != 0 {
            return Err("approval contains a failed receipt".to_string());
        }
        if verdict.blocking.is_blocking() {
            return Err("approval declares a blocking condition".to_string());
        }
        if !verdict.findings.is_empty() {
            return Err("approval contains unresolved findings".to_string());
        }
    }
    Ok(())
}

fn artifact_contains_target(root: &Path, relative: &str, target: &str) -> Result<bool, String> {
    let path = root.join(relative);
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("cannot read cited artifact {}: {error}", path.display()))?;
    Ok(bytes
        .windows(target.as_bytes().len())
        .any(|window| window == target.as_bytes()))
}

/// Result of one skeptic in the panel. The `refuted` flag is the
/// aggregator's input; the rest is for the details-file render. A malformed
/// or missing JSON record is marked as an infrastructure fallback, which the
/// aggregate cannot turn into approval.
#[derive(Debug, Clone)]
pub(crate) struct SkepticResult {
    pub skeptic_idx: u32,
    pub refuted: bool,
    pub confidence: SkepticConfidence,
    /// Blocker classification carried over from the verdict JSON;
    /// `None` (default) for a model-fixable gap, a synthetic refute, or
    /// a terminal-token-only fallback.
    pub blocking: SkepticBlocking,
    /// Single-line `path:line` citation from the verdict JSON. Drives
    /// the stall fingerprint and the gaps-summary fallback when no
    /// structured `findings` were emitted.
    pub evidence: String,
    /// Structured findings for the implementer (preferred over `evidence`
    /// when non-empty). Empty on fallback / failure paths.
    pub findings: Vec<Finding>,
    /// `None` on a clean parse; populated when the JSON file was
    /// missing/malformed or the spawn failed.
    pub fallback_note: Option<String>,
    /// Round-unique report path produced by this critic.
    pub details_path: String,
    /// Per-skeptic spawn-to-verdict wall clock in ms. Plumbed up so
    /// the panel-level event can surface slow outliers even though
    /// emissions are batched after `join_all`.
    pub latency_ms: u64,
}

/// Substitute the per-skeptic JSON-verdict path placeholders and root
/// the result under the goal's scratch root.
#[cfg(test)]
pub(crate) fn format_verdict_path(verifier_id: &str, attempt: u32, skeptic_idx: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_VERIFIER_VERDICT_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string())
            .replace("{skeptic_idx}", &skeptic_idx.to_string()),
    )
}

/// Substitute the per-skeptic Markdown-details path placeholders and
/// root the result under the goal's scratch root.
#[cfg(test)]
pub(crate) fn format_verifier_details_path(
    verifier_id: &str,
    attempt: u32,
    skeptic_idx: u32,
) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_VERIFIER_DETAILS_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string())
            .replace("{skeptic_idx}", &skeptic_idx.to_string()),
    )
}

fn round_critic_dir(verifier_id: &str, round_id: &str, skeptic_idx: u32) -> PathBuf {
    super::goal_tracker::goal_scratch_root(verifier_id)
        .join("verification-rounds")
        .join(round_id)
        .join(format!("critic-{skeptic_idx}"))
}

fn format_round_verdict_path(verifier_id: &str, round_id: &str, skeptic_idx: u32) -> String {
    round_critic_dir(verifier_id, round_id, skeptic_idx)
        .join("verdict.json")
        .to_string_lossy()
        .into_owned()
}

fn format_round_details_path(verifier_id: &str, round_id: &str, skeptic_idx: u32) -> String {
    round_critic_dir(verifier_id, round_id, skeptic_idx)
        .join("details.md")
        .to_string_lossy()
        .into_owned()
}

/// Aggregate the panel under unanimous approval semantics.
///
/// Every accepted structured verdict represents assigned correctness
/// coverage. A single refutation, missing verdict, or infrastructure fallback
/// therefore prevents approval. The empty panel also fails closed.
///
/// Returns `(refuted_count, total, achieved)`.
pub(crate) fn aggregate_skeptic_verdicts(results: &[SkepticResult]) -> (u32, u32, bool) {
    let total = results.len() as u32;
    let refuted_count = results.iter().filter(|r| r.refuted).count() as u32;
    (refuted_count, total, total > 0 && refuted_count == 0)
}

/// Per-evidence-line char cap for the inlined gaps summary — bounds a
/// runaway verdict yet holds a full multi-point gap without cutting the
/// primary finding mid-sentence. The model's reminder inlines only this
/// bounded summary; the untruncated per-skeptic writeup is persisted to
/// `last_classifier_details_path` for the user. Counted in `char`s, never
/// bytes, so truncation can't split a codepoint.
const GAPS_EVIDENCE_MAX_CHARS: usize = 800;

/// Neutralize and cap a model-written evidence string before it is
/// inlined into the `<system-reminder>` rejection nudge. The skeptic's
/// `evidence` is the only model-controlled text on the gaps path, so a
/// verifier emitting `</system-reminder>` or the `<goal-state>` tags
/// could otherwise close/reopen the reminder frame; a zero-width space
/// after the leading `<` breaks each literal tag while staying visually
/// identical. Capped on a `char` boundary (placeholder inertness is the
/// renderer's last-substitution concern, not this function's).
fn sanitize_evidence(evidence: &str) -> String {
    neutralize_reminder_tags(cap_chars(evidence.trim(), GAPS_EVIDENCE_MAX_CHARS))
}

/// Char cap for the whole multi-skeptic `{PRIOR_GAPS}` block, sized for
/// 2-3 skeptics × [`GAPS_MAX_FINDINGS`] findings — the per-line
/// [`GAPS_EVIDENCE_MAX_CHARS`] cap would chop later skeptics' gaps.
const PRIOR_GAPS_MAX_CHARS: usize = 4_000;

/// [`sanitize_evidence`]'s neutralization with the block-sized
/// [`PRIOR_GAPS_MAX_CHARS`] cap, for the `{PRIOR_GAPS}` prompt slot.
fn sanitize_prior_gaps(gaps: &str) -> String {
    neutralize_reminder_tags(cap_chars(gaps.trim(), PRIOR_GAPS_MAX_CHARS))
}

/// Truncate to `max_chars` `char`s (never bytes, so a codepoint can't
/// split) with an `…` suffix when capped; single pass via `char_indices`.
pub(crate) fn cap_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => {
            let mut s = String::with_capacity(cut + '…'.len_utf8());
            s.push_str(&text[..cut]);
            s.push('…');
            s
        }
        None => text.to_string(),
    }
}

/// Break the literal reminder-frame tags with a zero-width space so
/// model-written text cannot close/reopen the `<system-reminder>` /
/// `<goal-state>` frames it is embedded in.
pub(crate) fn neutralize_reminder_tags(text: String) -> String {
    text.replace("</system-reminder>", "<\u{200b}/system-reminder>")
        .replace("<system-reminder>", "<\u{200b}system-reminder>")
        .replace("</goal-state>", "<\u{200b}/goal-state>")
        .replace("<goal-state>", "<\u{200b}goal-state>")
}

/// Cap on findings rendered per refuter — bounds a runaway verdict while
/// holding a full multi-point gap list.
const GAPS_MAX_FINDINGS: usize = 12;

/// Render one structured finding as `kind · location — detail`, dropping
/// empty segments. Sanitized like evidence (tag-inert, char-capped).
fn render_finding(f: &Finding) -> String {
    let kind = f.kind.trim();
    let loc = f.location.trim();
    let detail = f.detail.trim();
    let head = if kind.is_empty() { "finding" } else { kind };
    let body = match (loc.is_empty(), detail.is_empty()) {
        (false, false) => format!("{head} · {loc} — {detail}"),
        (false, true) => format!("{head} · {loc}"),
        (true, false) => format!("{head} — {detail}"),
        (true, true) => head.to_string(),
    };
    sanitize_evidence(&body)
}

/// Render one refuter as a sanitized bullet. Prefers structured `findings`
/// (one sub-bullet each), else `evidence`, else the synthetic `fallback_note`,
/// else a bare no-evidence note. All model text is sanitized.
fn render_refuter_bullet(r: &SkepticResult) -> String {
    let header = format!(
        "- [skeptic {}, {}]",
        r.skeptic_idx,
        r.confidence.as_const_str()
    );
    if !r.findings.is_empty() {
        let lines: Vec<String> = r
            .findings
            .iter()
            .take(GAPS_MAX_FINDINGS)
            .map(|f| format!("  - {}", render_finding(f)))
            .collect();
        return format!("{header}\n{}", lines.join("\n"));
    }
    let evidence = r.evidence.trim();
    if !evidence.is_empty() {
        format!("{header} {}", sanitize_evidence(evidence))
    } else if let Some(note) = &r.fallback_note {
        format!(
            "- [skeptic {}] no verdict produced: {}",
            r.skeptic_idx,
            sanitize_evidence(note),
        )
    } else {
        format!("- [skeptic {}] refuted (no evidence)", r.skeptic_idx)
    }
}

/// Refuters ordered high→low confidence (stable within a tier, so
/// skeptic index breaks ties).
fn refuters_by_confidence(results: &[SkepticResult]) -> Vec<&SkepticResult> {
    let mut refuters: Vec<&SkepticResult> = results.iter().filter(|r| r.refuted).collect();
    refuters.sort_by_key(|r| r.confidence.rank());
    refuters
}

/// Build the inlined gaps summary for the rejection nudge: one bullet
/// per refuting skeptic, ordered high→low confidence. Bounded by the
/// panel size. Empty only for a no-refuter panel — unreachable on the
/// panel-reject path (`achieved == false` implies a refute majority).
fn build_gaps_summary(results: &[SkepticResult]) -> String {
    refuters_by_confidence(results)
        .into_iter()
        .map(render_refuter_bullet)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Section headers for the auto-pause blocker summary, one per
/// [`SkepticBlocking`] class. `PAUSE_GROUP_FIXABLE` is also reused by
/// the synthetic-sampler cap path in `acp_session`.
pub(crate) const PAUSE_GROUP_FIXABLE: &str = "Model-fixable gaps";
const PAUSE_GROUP_CONTRADICTION: &str = "Contradictions (objective/plan conflict)";
const PAUSE_GROUP_UNVERIFIABLE: &str = "Unverifiable in this environment";

/// Build the user-facing auto-pause summary: refuter bullets grouped by
/// [`SkepticBlocking`] class so a paused goal tells the user which
/// blockers are model-fixable versus contradictions versus
/// environment-unverifiable. Empty groups are omitted; reuses
/// [`render_refuter_bullet`] so sanitization stays single-sourced.
fn build_pause_summary(results: &[SkepticResult]) -> String {
    let refuters = refuters_by_confidence(results);
    [
        (SkepticBlocking::None, PAUSE_GROUP_FIXABLE),
        (SkepticBlocking::Contradiction, PAUSE_GROUP_CONTRADICTION),
        (SkepticBlocking::Unverifiable, PAUSE_GROUP_UNVERIFIABLE),
    ]
    .into_iter()
    .filter_map(|(class, header)| {
        let bullets: Vec<String> = refuters
            .iter()
            .copied()
            .filter(|r| r.blocking == class)
            .map(render_refuter_bullet)
            .collect();
        (!bullets.is_empty()).then(|| format!("{header}:\n{}", bullets.join("\n")))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Normalized fingerprint of a rejection's *raw* gaps, used to detect a
/// stuck loop (identical fingerprint across attempts). Operates on the
/// undecorated evidence — never the rendered `- [skeptic N, conf]`
/// bullets — so identical gaps map to one fingerprint regardless of
/// skeptic ordering/confidence. Uses the deduplicated, sorted,
/// lowercased `path:line` citations; with none present, falls back to
/// the sorted trimmed non-empty lines. Empty input → `""`, which the
/// stall guard treats as "no stable fingerprint".
pub(crate) fn gap_fingerprint(raw_evidence: &[&str]) -> String {
    let normalized: Vec<Cow<'_, str>> = raw_evidence
        .iter()
        .map(|e| normalize_scratch_paths(e))
        .collect();
    let mut tokens: Vec<String> = normalized
        .iter()
        .flat_map(|e| extract_path_line_tokens(e))
        .collect();
    if tokens.is_empty() {
        tokens = normalized
            .iter()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
    }
    tokens.sort();
    tokens.dedup();
    tokens.join("\n")
}

/// Replace scratch/temp-path tokens with `<scratch>`: they embed
/// per-attempt ids, so leaving them in makes an identical gap
/// fingerprint differently every attempt and the stall guard never
/// fires. Borrowed when no scratch token is present (the common case);
/// spacing collapses on the owned path — fine for a comparison-only
/// fingerprint.
fn normalize_scratch_paths(text: &str) -> Cow<'_, str> {
    const SCRATCH_MARKERS: &[&str] = &["/tmp/", "/var/folders/", "/private/tmp/"];
    if !SCRATCH_MARKERS.iter().any(|m| text.contains(m)) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.split_whitespace()
            .map(|tok| {
                if SCRATCH_MARKERS.iter().any(|m| tok.contains(m)) {
                    "<scratch>"
                } else {
                    tok
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Per-refuter fingerprint source: the raw model `evidence`, or the
/// `fallback_note` when a synthetic refute carries no evidence. Keeps
/// repeated infra-failure rejections stable without the bullet decoration.
fn refuter_fingerprint_source(r: &SkepticResult) -> &str {
    if r.evidence.trim().is_empty() {
        r.fallback_note.as_deref().unwrap_or("")
    } else {
        r.evidence.as_str()
    }
}

/// Pull `path:line` citations out of free text, lowercasing the path. A
/// token qualifies when the prefix (before the FIRST colon) looks path-ish
/// (contains `/` or `.`) and the first colon-segment after it is all
/// digits — tolerating the `path:line:col` / trailing-colon forms common
/// in compiler / test-runner output (e.g. `src/foo.rs:12:5: error`).
fn extract_path_line_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace())
        .filter_map(|raw| {
            let word = raw.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && !matches!(c, '.' | '/' | '_' | '-' | ':')
            });
            let (path, rest) = word.split_once(':')?;
            let line = rest.split(':').next().unwrap_or_default();
            let path_ok = !path.is_empty() && (path.contains('/') || path.contains('.'));
            let line_ok = !line.is_empty() && line.chars().all(|c| c.is_ascii_digit());
            (path_ok && line_ok).then(|| format!("{}:{line}", path.to_ascii_lowercase()))
        })
        .collect()
}

/// The planner's `## Goal kind` tag (see `goal_planner_prompt.md`). Selects
/// the kind-specific verifier review lens; an unrecognised / absent kind maps
/// to `None` (no lens — the generic adversarial verifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalKind {
    CodeChange,
    Analysis,
    Research,
    /// Mathematical derivation / quantitative computation / proof deliverable.
    Math,
}

/// Canonical wire/plan names for the five mandatory math-correctness gates.
/// These strings are shared by plan validation and structured verdict
/// validation so the planner, implementer, and skeptic cannot silently use
/// different checklists.
const MATH_VALIDATION_GATES: [&str; 5] = [
    "contract-closure",
    "derivation-integrity",
    "evidence-provenance",
    "invariant-ledger",
    "state-isolation",
];

/// Parse the `## Goal kind` value from a plan-file body. Reads the first
/// non-empty line after the header; trims backticks/whitespace/emphasis
/// and normalizes space/underscore separators so a near-miss tag
/// (`**code-change**`, `code change`) does not silently drop the lens.
pub(crate) fn parse_goal_kind(plan: &str) -> Option<GoalKind> {
    let mut lines = plan.lines();
    while let Some(line) = lines.next() {
        if !line.trim().eq_ignore_ascii_case("## Goal kind") {
            continue;
        }
        for next in lines.by_ref() {
            let value = next.trim().trim_matches(['`', '*', '_']).trim();
            if value.is_empty() {
                continue;
            }
            let normalized: String = value
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c == ' ' || c == '_' { '-' } else { c })
                .collect();
            return match normalized.as_str() {
                "code-change" => Some(GoalKind::CodeChange),
                "analysis" => Some(GoalKind::Analysis),
                "research" => Some(GoalKind::Research),
                "math" | "math-derivation" | "derivation" | "quantitative" => Some(GoalKind::Math),
                _ => None,
            };
        }
    }
    None
}

/// Parse the planner's composable `## Goal facets` section. Legacy plans with
/// only `## Goal kind` remain valid and contribute one facet.
pub(crate) fn parse_goal_facets(plan: &str) -> std::collections::BTreeSet<VerificationFacet> {
    let mut facets = std::collections::BTreeSet::new();
    let mut in_section = false;
    for raw in plan.lines() {
        let line = raw.trim();
        if line.eq_ignore_ascii_case("## Goal facets") {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("## ") {
            break;
        }
        if !in_section || line.is_empty() {
            continue;
        }
        for token in line
            .trim_start_matches(['-', '*'])
            .split([',', '+', ';'])
            .map(str::trim)
        {
            if let Some(facet) = canonical_facet(token.trim_matches(['`', '*', '_'])) {
                facets.insert(facet);
            }
        }
    }
    if facets.is_empty()
        && let Some(kind) = parse_goal_kind(plan)
    {
        facets.insert(match kind {
            GoalKind::CodeChange => VerificationFacet::Code,
            GoalKind::Analysis => VerificationFacet::Analysis,
            GoalKind::Research => VerificationFacet::Research,
            GoalKind::Math => VerificationFacet::Math,
        });
    }
    facets
}

/// `code-change` review lens — adversarial code review layered on the
/// acceptance criteria, hunting real defects, test-theater, and cheating.
const KIND_LENS_CODE_CHANGE: &str = "\n## Code-change review lens\n\n\
This goal changes code. Satisfying the criteria nominally is NOT enough — do a senior-engineer adversarial review of every file in CHANGED_FILES and the paths they touch. Read the CURRENT contents, run the code, and cite a `path:line` or a command/test transcript for every finding. Bias to `refuted: true`.\n\n\
Your PRIMARY mandate is to actively HUNT for real bugs, issues, and gaps in the shipped behavior — defects you can demonstrate — not to nitpick coverage. Missing coverage alone, when the code is correct and the criteria hold, is NOT a refute.\n\n\
- Correctness — reason over the whole input space (valid, invalid, empty, boundary, large, concurrent, adversarial) for any input that makes the code produce a wrong result; one such input is a decisive refute — state the input and expected-vs-actual. Illustrative, not exhaustive: off-by-one, wrong operator, inverted condition, wrong variable/index, null/empty dereference, unhandled error path, overflow/precision/sign, bad early-return, race.\n\
- Completeness — fully implement the requirement, not just the happy path. Refute when edge/error cases are silently dropped, a value is hardcoded that must be dynamic, a branch returns a placeholder, or only the demo case works.\n\
- Real tests, not theater — judge each test by whether it would catch a deliberately-broken implementation; one that still passes against a wrong implementation (asserts only on mocks/constants, sets internal state instead of using the real entry point, or has no meaningful assertion) is theater — discount it (refute if it is the only evidence for a required behavior). Injecting a fake at an environment boundary (clock, RNG, network/file/output sink) so the unit's REAL logic runs deterministically is honest dependency injection, NOT theater. A green project suite is WEAK evidence, never proof. Refute hard on tests weakened, `#[ignore]`/skipped, commented out, or whose expected values were edited to match buggy output.\n\
- End-to-end reality — build it and exercise each behavioral criterion through the REAL entry point and observed output, judging as the USER would; driving an internal flag or helper proves the mechanism exists, NOT that the wired-up feature works. A criterion whose code is present but whose integrated behavior is wrong, unreachable, or unusable is `refuted: true`, as is anything that fails to compile, fails its tests, or errors at runtime. EXCEPTION — behavior the harness cannot drive headlessly (a UI, a browser, a game loop, a long-running interactive session): the static/structural fallback is the accepted bar (the artifact is present AND the shipped unit-level functions — e.g. physics, collision, input mapping, state transitions — are exercised against the real path); this applies EVEN IF the plan did not spell the fallback out. The fallback still includes the cheap load check: a browser-loaded script must evaluate without error in a browser-like environment (`window` defined, NO Node globals) — an unguarded `module.exports`/`require` in a `<script src>` file crashes at load (blank page) and is a decisive, headlessly-provable defect. Likewise an ES-module/import-map page with no `file:` fallback message: double-clicked from disk it is a silent black screen (CORS blocks module imports), so it must either use plain scripts or visibly tell the user to serve it. Entry-point launch: whatever the deliverable (CLI, server, library, page), it must have been LAUNCHED once on its real entry path with the cheapest runtime the environment offers — run the command, boot the server and hit an endpoint, import the library fresh, or headless-load the page (zero page errors, plus the strong primary-observable bar below; module-resolution failures only surface on a real load). Audit the implementer's captured launch evidence (transcript/screenshot) and refute when it is absent even though the environment could launch it. Present is not correct: the launch gate must assert the deliverable's PRIMARY OBSERVABLE is CORRECT, not merely present or non-empty — a CLI's actual output content (not just that it ran), a server's response body (not just HTTP 200), a library call's real return value, or for a rendered page that the render surface's drawing dimensions equal the intended/target size (a renderer that cached a stale/default size paints a near-blank surface), that the surface is SUBSTANTIALLY filled (a high painted fraction or a painted bbox ≈ the whole surface, NOT a `> 0 pixels` check), and that a driven input produces the expected visible/state change. Launch evidence proving only \"exists / non-empty / exited 0\" is INSUFFICIENT — refute and request the stronger gate (the next-round gap). If the captured evidence instead shows the LAUNCHER failing for environmental reasons (browser cannot start in the sandbox, missing system dep), or the environment can launch but cannot reliably read back the primary observable (headless pixel/WebGL readback or input injection unavailable), that honest failure capture plus the static fallback IS the accepted bar — do not keep demanding a launch or readback the environment cannot perform; refute fabricated/synthetic launch evidence, not the honest fallback. \"Cannot read back\" means the readback mechanism is unavailable or errors, NOT a readback that succeeded and returned a blank or partial buffer — that buffer IS the deliverable's output and a defect to refute. A captured launch/run FAILURE (a page error, an empty or too-short render buffer, a \"canvas buffer empty\", a wrong/empty CLI output, an error response body, a nonzero exit) is a defect, NOT flakiness — do not wave it off or let one cherry-picked success supersede it. Re-run captures that DISAGREE across attempts to consensus on the CAUSE (not a pass/fail vote), and attribute EVERY failure by the cause test below. Route by CAUSE, not frequency: an ENVIRONMENT/launcher failure (the sandbox cannot run or observe it, whether every time or only intermittently) never forces a refute — take a good capture if one run produced it, else the honest fallback above; an APP failure (the launcher ran but the deliverable was wrong, blank, or errored) refutes even when only some runs show it — the non-determinism is itself the defect, never an unverifiable environment. Do NOT refute merely because an end-to-end outcome lacks test-only scaffolding, only when a gating criterion is missed or a real defect is present.\n\
- Code-correctness floor (applies EVEN under the End-to-end EXCEPTION above) — the static/structural fallback excuses the *runtime* proof, never a defect you can read in the source. Before accepting the fallback for ANY deliverable (domain-agnostic: CLI, service, library, data job, UI, game), READ the shipped code for the core behaviors the OBJECTIVE names or plainly implies — not only the ones the plan enumerated — and refute (cite `path:line`) when such a behavior is, in the code, absent, a no-op, dead, or wired to nothing: e.g. a handler/branch that never changes the state it exists to change, an input/event/endpoint/flag bound to no effect, a feature present only as a placeholder/stub return, or a primary flow with no reachable completion/terminal state the objective implies. This is a FLOOR for the objective's CORE purpose ONLY — do NOT extend it to polish, fidelity, extra scope, edge/error handling, or robustness the plan did not require (those remain false-refutes — never invent scope beyond the contract); the anti-ratchet rule still binds: the floor is fixed by the objective and does not rise between rounds.\n\
- No regressions — run the pre-existing suite and inspect adjacent call sites and any changed signature / public API.\n\
- No cheating — refute if the agent hardcoded the expected output, special-cased the test input, swallowed errors to suppress failures, deleted/disabled failing assertions, stubbed the hard part behind a TODO, or narrowed scope to dodge the requirement.\n\
- Security — no secret committed, and no injection, path traversal, unsafe deserialization, or unsanitised-input path introduced.\n";

/// `research` fact-check lens — verify every claim against its cited source.
const KIND_LENS_RESEARCH: &str = "\n## Research fact-check lens\n\n\
This goal gathers external information; the deliverable's whole value is its factual accuracy, so do not accept claims on trust — verify them. Bias to `refuted: true` on any claim you cannot confirm.\n\n\
- Source-back every claim — for each material factual assertion, OPEN the cited source with the available read tools or a sandboxed command and confirm that source actually states it. A claim with no citation, a dead or invented citation, a citation that does not support (or outright contradicts) it, or one resting only on FINAL_RESPONSE prose is `refuted: true`.\n\
- No fabrication or staleness — flag invented APIs/figures/quotes/version numbers, statistics with no provenance, and information that is out of date for a time-sensitive objective.\n\
- Balance & completeness — if the objective implies a comparison or survey, material alternatives and counter-evidence must be covered; a one-sided or cherry-picked answer is incomplete.\n\
- Conflicts — where sources disagree, the deliverable must surface the disagreement rather than silently pick one side.\n";

/// `analysis` soundness lens — conclusions must be evidence-grounded and follow.
const KIND_LENS_ANALYSIS: &str = "\n## Analysis soundness lens\n\n\
This goal explains or diagnoses something; the failure mode to hunt is a fluent, confident analysis that is actually wrong. Verify the reasoning, do not grade the prose.\n\n\
- Evidence-grounded — every claim about the code/system must cite concrete, checkable evidence (a `path:line`, a command/test transcript, a log line). Open the cited evidence and confirm it says what the analysis claims; an assertion with no verifiable backing is `refuted: true`.\n\
- Causally sound — the diagnosis must actually follow from the evidence: a correct root cause, not a correlation or a plausible-sounding guess. If you can find evidence that contradicts the stated conclusion, refute and cite it.\n\
- Verifiable — when the analysis claims \"X causes Y\" or \"the bug is Z\", confirm it with a cheap repro/test where feasible; a falsifiable causal claim you can disprove is a decisive refute.\n\
- Answers the question — the analysis must address what was actually asked, with no critical sub-question hand-waved, hedged into vagueness, or skipped.\n";

/// `math` correctness lens — independent, risk-proportional verification.
const KIND_LENS_MATH: &str = "\n## Math / quantitative correctness lens\n\n\
This goal produces mathematical derivations, physical formulations, proofs, simulations, or quantitative results. Structural completeness is supporting evidence, never a substitute for correctness.\n\n\
Read the authoritative task sources and the actual final artifact. Independently challenge every requested result and the consequential reasoning whose failure could change a conclusion. For a short derivation, checking each step may be proportionate; for a long research artifact, prioritize governing equations, sensitive assumptions, primary conclusions, and high-risk numerical or physical links instead of mechanically testing every line.\n\n\
Apply all five harness gates and record one claim-bound `checks` receipt for each before approving:\n\
- `contract-closure`: enumerate every requested result and test its domain, branches, BC/IC, boundary values, and critical equality cases against the original relation.\n\
- `derivation-integrity`: check every consequential implication; reject a correct final formula reached through a false equality, illegal division, dropped branch, sign/factor error, or unmet hypothesis.\n\
- `evidence-provenance`: bind each CAS/numerical/tool observation to the exact claim, current final-artifact location/version, input/command, output, and tolerance/error. An unbound successful run is not evidence.\n\
- `invariant-ledger`: propagate symbols, units, dimensions, normalization, signs, coordinate/gauge/Fourier conventions, admissibility, and conservation from assumptions through the final answer.\n\
- `state-isolation`: compare authoritative inputs and the frozen goal-start state to the current artifact, then use prior-round gaps to detect repair regressions; detect whole-artifact rewrite loss and recheck every changed dependency. Use `not_applicable` only with a concrete reason.\n\n\
Use re-derivation, residual/substitution, special and limiting regimes, or numerical convergence/error and sensitivity as appropriate. Numerical claims need reproducible inputs and enough tolerance/convergence evidence to support the stated precision. Tool-backed checks are preferred when they materially reduce uncertainty, but a fixed log or manifest is not evidence by itself.\n\n\
Refute confirmed mathematical errors, missing requested results, unsupported material claims, inconsistent conventions, and contradictions between prose, equations, sources, or computed evidence. Accept valid alternative derivations and clearly defined equivalent notation. Do not add benchmark-specific forms, require ceremony the task did not request, or reject harmless presentation differences.\n";

/// Compose every applicable review lens. A hybrid code+math task receives both
/// instead of allowing one classifier label to suppress the other.
fn facet_lenses(facets: &std::collections::BTreeSet<VerificationFacet>) -> String {
    let mut out = String::new();
    if facets.contains(&VerificationFacet::Code) {
        out.push_str(KIND_LENS_CODE_CHANGE);
    }
    if facets.contains(&VerificationFacet::Analysis)
        || facets.contains(&VerificationFacet::Empirical)
    {
        out.push_str(KIND_LENS_ANALYSIS);
    }
    if facets.contains(&VerificationFacet::Research)
        || facets.contains(&VerificationFacet::Sources)
        || facets.contains(&VerificationFacet::Citations)
    {
        out.push_str(KIND_LENS_RESEARCH);
    }
    if facets.contains(&VerificationFacet::Math) {
        out.push_str(KIND_LENS_MATH);
    }
    out
}

#[cfg(test)]
fn kind_lens(kind: Option<GoalKind>) -> &'static str {
    match kind {
        Some(GoalKind::CodeChange) => KIND_LENS_CODE_CHANGE,
        Some(GoalKind::Research) => KIND_LENS_RESEARCH,
        Some(GoalKind::Analysis) => KIND_LENS_ANALYSIS,
        Some(GoalKind::Math) => KIND_LENS_MATH,
        None => "",
    }
}

fn classify_facets(
    objective: &str,
    plan: &str,
    workspace_root: &Path,
) -> std::collections::BTreeSet<VerificationFacet> {
    let mut facets = parse_goal_facets(plan);
    let lower = objective.to_ascii_lowercase();
    let named_sources = named_local_source_candidates(objective);
    let named_extensions: Vec<String> = named_sources
        .iter()
        .filter_map(|candidate| {
            Path::new(candidate)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
        })
        .collect();
    if objective_suggests_code_change(objective) {
        facets.insert(VerificationFacet::Code);
    }
    if objective_or_named_sources_suggests_math(objective, workspace_root)
        || objective_suggests_math(plan)
    {
        facets.insert(VerificationFacet::Math);
    }
    if ["research", "literature", "paper", "manuscript"]
        .iter()
        .any(|signal| lower.contains(signal))
    {
        facets.insert(VerificationFacet::Research);
    }
    let empirical = [
        "experimental data",
        "statistical",
        "regression analysis",
        "confidence interval",
    ]
    .iter()
    .any(|signal| lower.contains(signal));
    if empirical {
        facets.insert(VerificationFacet::Empirical);
        facets.insert(VerificationFacet::Math);
    }
    let named_authoritative_source = named_extensions.iter().any(|extension| {
        matches!(
            extension.as_str(),
            "pdf" | "docx" | "tex" | "bib" | "ipynb" | "csv" | "tsv" | "xlsx" | "parquet"
        )
    });
    if named_authoritative_source
        || ["authoritative source", "cited source", "source material"]
            .iter()
            .any(|signal| lower.contains(signal))
    {
        facets.insert(VerificationFacet::Sources);
    }
    let named_document_source = named_extensions
        .iter()
        .any(|extension| matches!(extension.as_str(), "pdf" | "docx" | "tex"));
    if named_document_source
        && [
            "paper",
            "manuscript",
            "scientific",
            "mathematical",
            "physics",
            "derive",
            "formulate",
        ]
        .iter()
        .any(|signal| lower.contains(signal))
    {
        facets.insert(VerificationFacet::Math);
    }
    if ["citation", "bibliography", "cited"]
        .iter()
        .any(|signal| lower.contains(signal))
    {
        facets.insert(VerificationFacet::Citations);
    }
    if [
        "render",
        "compile the paper",
        "compiled pdf",
        "paper",
        "manuscript",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
    {
        facets.insert(VerificationFacet::DocumentRender);
    }
    if facets.is_empty() {
        facets.insert(VerificationFacet::Analysis);
    }
    facets.insert(VerificationFacet::StateRegression);
    facets
}

/// Strong objective signals that the deliverable is mathematical /
/// quantitative (derivation, proof, closed form) rather than code or
/// generic research prose. Conservative — avoids "calculus history" style
/// false positives by requiring derivation/solve/compute phrasing.
pub(crate) fn objective_suggests_math(objective: &str) -> bool {
    let o = objective.to_ascii_lowercase();
    const STRONG: &[&str] = &[
        "derive ",
        "derivation",
        "prove that",
        "closed-form",
        "closed form",
        "sympy",
        "\\boxed",
        "boxed answer",
        "boxed final",
        "solve the equation",
        "differential equation",
        "partial differential",
        "eigenvalue",
        "math completion",
        "mathematical derivation",
        "mathematical formulation",
        "quantitative analysis",
        "quantitative research",
        "numerical validation",
        "numerically validate",
        "scientific computation",
        "formulate a model",
        "governing equation",
        "boundary condition",
        "initial value problem",
        "dimensional analysis",
        "adversarial math",
        "compute the integral",
        "evaluate the integral",
        "find the closed form",
        "independent recomputation",
        "mathematical paper",
        "physics paper",
    ];
    STRONG.iter().any(|s| o.contains(s))
}

fn objective_suggests_code_change(objective: &str) -> bool {
    let o = objective.to_ascii_lowercase();
    const STRONG: &[&str] = &[
        "implement ",
        "fix the bug",
        "fix a bug",
        "refactor ",
        "modify the code",
        "change the code",
        "update the code",
        "source code",
        "add a feature",
        "add an endpoint",
        "rust crate",
        "cli command",
    ];
    STRONG.iter().any(|signal| o.contains(signal))
}

fn named_local_source_candidates(text: &str) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[
        "txt", "md", "rst", "tex", "bib", "json", "yaml", "yml", "toml", "csv", "tsv", "pdf",
        "docx", "ipynb", "xlsx", "parquet", "rs", "py", "jl", "m", "c", "cpp", "h", "hpp", "f90",
    ];

    let mut candidates = Vec::new();
    for delimiter in ['"', '\'', '`'] {
        let mut parts = text.split(delimiter);
        while let Some(_outside) = parts.next() {
            let Some(inside) = parts.next() else {
                break;
            };
            let candidate = inside.trim();
            let extension = Path::new(candidate)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            if extension
                .as_deref()
                .is_some_and(|ext| EXTENSIONS.contains(&ext))
            {
                candidates.push(candidate.to_string());
            }
        }
    }
    let mut token = String::new();
    let mut finish_token = |token: &mut String| {
        let candidate = token
            .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | '!' | '?'))
            .to_string();
        token.clear();
        let extension = Path::new(&candidate)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        if extension
            .as_deref()
            .is_some_and(|ext| EXTENSIONS.contains(&ext))
        {
            candidates.push(candidate);
        }
    };

    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/') {
            token.push(ch);
        } else if !token.is_empty() {
            finish_token(&mut token);
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn resolve_named_source(workspace_root: &Path, base: &Path, raw: &str) -> Option<PathBuf> {
    let raw_path = Path::new(raw);
    let mut attempts = Vec::with_capacity(2);
    if raw_path.is_absolute() {
        attempts.push(raw_path.to_path_buf());
    } else {
        attempts.push(base.join(raw_path));
        if base != workspace_root {
            attempts.push(workspace_root.join(raw_path));
        }
    }
    for attempt in attempts {
        let Ok(path) = std::fs::canonicalize(attempt) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if path.starts_with(workspace_root) && metadata.is_file() {
            return Some(path);
        }
    }
    None
}

/// Detect a quantitative source contract even when the literal objective is
/// only a wrapper such as "start from SPEC.txt". Traversal is workspace-local,
/// text-only, bounded, and follows named files rather than scanning the tree.
pub(crate) fn objective_or_named_sources_suggests_math(
    objective: &str,
    workspace_root: &Path,
) -> bool {
    const MAX_DEPTH: usize = 3;
    const MAX_FILES: usize = 16;
    const MAX_FILE_BYTES: u64 = 512 * 1024;
    const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;

    if objective_suggests_math(objective) {
        return true;
    }
    let Ok(workspace_root) = std::fs::canonicalize(workspace_root) else {
        return false;
    };
    let mut queue = std::collections::VecDeque::new();
    for raw in named_local_source_candidates(objective) {
        queue.push_back((workspace_root.clone(), raw, 0_usize));
    }
    let mut visited = std::collections::BTreeSet::new();
    let mut inspected = 0_usize;
    let mut total_bytes = 0_u64;
    while let Some((base, raw, depth)) = queue.pop_front() {
        if inspected >= MAX_FILES {
            break;
        }
        let Some(path) = resolve_named_source(&workspace_root, &base, &raw) else {
            continue;
        };
        if !visited.insert(path.clone()) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES
            || total_bytes.saturating_add(metadata.len()) > MAX_TOTAL_BYTES
        {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        inspected += 1;
        total_bytes += metadata.len();
        if objective_suggests_math(&body) {
            return true;
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let base = path.parent().unwrap_or(&workspace_root).to_path_buf();
        for child in named_local_source_candidates(&body) {
            queue.push_back((base.clone(), child, depth + 1));
        }
    }
    false
}

/// Whether this plan must include an independent math-verification step.
/// True when kind is `math`, or
/// when the objective is strongly quantitative and the kind is not
/// `code-change` (coding tasks with math-flavored words stay on the
/// code-change path).
#[cfg(test)]
pub(crate) fn plan_requires_math_adversarial(objective: &str, plan: &str) -> bool {
    parse_goal_facets(plan).contains(&VerificationFacet::Math)
        || objective_suggests_math(objective)
        || objective_suggests_math(plan)
}

/// Static check: a math-required plan must (1) tag kind `math`, (2) include a
/// `gating` adversarial / independent recomputation, and (3) cover all five
/// canonical math-correctness gates in gating (not evidence-only) steps.
#[cfg(test)]
pub(crate) fn validate_math_plan_contract(objective: &str, plan: &str) -> Result<(), &'static str> {
    if !plan_requires_math_adversarial(objective, plan) {
        return Ok(());
    }
    if !parse_goal_facets(plan).contains(&VerificationFacet::Math) {
        return Err(
            "math/quantitative objective requires the `math` goal facet so the math \
             verifier lens applies",
        );
    }
    if !plan_has_adversarial_math_gate(plan) {
        return Err(
            "math plan missing gating adversarial/independent recomputation step \
             in ## Verification plan",
        );
    }
    if !plan_has_complete_math_gate_coverage(plan) {
        return Err(
            "math plan's gating verification steps must cover contract-closure, \
             derivation-integrity, evidence-provenance, invariant-ledger, and \
             state-isolation",
        );
    }
    Ok(())
}

/// Source-aware sibling of [`validate_math_plan_contract`]: the wrapper
/// objective may only name local paths whose quantitative content lives in
/// those files. The planner gate must use the SAME traversal the final
/// verifier uses ([`objective_or_named_sources_suggests_math`]) so a
/// misclassified wrapper cannot slip past the static contract check.
pub(crate) fn validate_math_plan_contract_source_aware(
    objective: &str,
    workspace_root: &std::path::Path,
    plan: &str,
) -> Result<(), &'static str> {
    if !plan_requires_math_adversarial_source_aware(objective, workspace_root, plan) {
        return Ok(());
    }
    if !parse_goal_facets(plan).contains(&VerificationFacet::Math) {
        return Err(
            "math/quantitative objective (incl. named sources) requires the `math` goal facet \
             so the math verifier lens applies",
        );
    }
    if !plan_has_adversarial_math_gate(plan) {
        return Err(
            "math plan missing gating adversarial/independent recomputation step \
             in ## Verification plan",
        );
    }
    if !plan_has_complete_math_gate_coverage(plan) {
        return Err(
            "math plan's gating verification steps must cover contract-closure, \
             derivation-integrity, evidence-provenance, invariant-ledger, and \
             state-isolation",
        );
    }
    Ok(())
}

/// Source-aware variant of [`plan_requires_math_adversarial`] that also
/// follows named local sources named by the wrapper objective.
pub(crate) fn plan_requires_math_adversarial_source_aware(
    objective: &str,
    workspace_root: &std::path::Path,
    plan: &str,
) -> bool {
    parse_goal_facets(plan).contains(&VerificationFacet::Math)
        || objective_or_named_sources_suggests_math(objective, workspace_root)
        || objective_suggests_math(plan)
}

/// Minimal typed-plan barrier shared by every deliverable. It rejects empty or
/// prose-only plans before the model can enter the execution loop.
pub(crate) fn validate_plan_contract(plan: &str) -> Result<(), &'static str> {
    let first = plan.lines().find(|line| !line.trim().is_empty());
    if first.is_none_or(|line| {
        line.trim()
            .strip_prefix("# Plan:")
            .is_none_or(|headline| headline.trim().is_empty())
    }) {
        return Err("plan must begin with a non-empty `# Plan:` heading");
    }
    if parse_goal_facets(plan).is_empty() {
        return Err("plan must declare at least one recognized goal facet");
    }
    for section in ["## Acceptance criteria", "## Verification plan"] {
        let mut inside = false;
        let mut items = Vec::new();
        for raw in plan.lines() {
            let line = raw.trim();
            if line.eq_ignore_ascii_case(section) {
                inside = true;
                continue;
            }
            if inside && line.starts_with("## ") {
                break;
            }
            if inside
                && let Some(item) = numbered_item_body(line)
            {
                items.push(item.to_ascii_lowercase());
            }
        }
        if items.is_empty() {
            return Err(match section {
                "## Acceptance criteria" => "plan must contain a numbered acceptance criterion",
                _ => "plan must contain a numbered verification step",
            });
        }
        if section == "## Verification plan"
            && items.iter().any(|item| !has_verification_step_tag(item))
        {
            return Err("every verification step must be tagged `gating` or `evidence`");
        }
    }
    Ok(())
}

/// True when a `## Verification plan` item carries a typed `gating` or
/// `evidence` tag as its leading token. Tolerates Markdown decoration around
/// the tag word (backticks, bold/italic `*`/`_`), matching the plan-writer
/// prompt's `` `gating` `` / `` `evidence` `` prose, so a plan that quotes the
/// tag is not rejected on decoration alone. The tag must be the item's first
/// whitespace-delimited token, consistent with the "tag each step" contract.
fn has_verification_step_tag(item: &str) -> bool {
    item.split_whitespace().next().is_some_and(|token| {
        let tag = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-');
        tag == "gating" || tag == "evidence"
    })
}

/// Extract normalized numbered/bulleted items from `## Verification plan`.
/// Continuation lines stay attached to their item; later sections are outside
/// the contract and cannot satisfy a gate accidentally.
fn verification_plan_items(plan: &str) -> Vec<String> {
    let mut in_verification_plan = false;
    let mut items: Vec<String> = Vec::new();

    for raw_line in plan.lines() {
        let line = raw_line.trim();
        if !in_verification_plan {
            if line.eq_ignore_ascii_case("## Verification plan") {
                in_verification_plan = true;
            }
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        if line.is_empty() {
            continue;
        }
        if is_markdown_list_item(line) {
            items.push(line.to_ascii_lowercase());
        } else if let Some(item) = items.last_mut() {
            item.push(' ');
            item.push_str(&line.to_ascii_lowercase());
        }
    }
    items
}

fn is_gating_verification_item(item: &str) -> bool {
    item.split_whitespace().take(3).any(|token| {
        token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-') == "gating"
    })
}

/// True when one numbered/bulleted item in `## Verification plan` is both
/// gating and an independent mathematical check.
///
/// Keeping the signals in one list item prevents unrelated prose elsewhere in
/// the plan—or an `evidence`-only check—from satisfying the completion gate.
fn plan_has_adversarial_math_gate(plan: &str) -> bool {
    verification_plan_items(plan).iter().any(|item| {
        let has_gating_tag = is_gating_verification_item(item);

        let uses_math_attacker = item.contains("attacker-math");
        let explicitly_rechecks = [
            "recompute",
            "re-compute",
            "recomputation",
            "recalculate",
            "re-calculat",
            "recalculation",
            "re-derive",
            "rederive",
        ]
        .iter()
        .any(|signal| item.contains(signal));
        let establishes_independence = ["independent", "adversarial", "alternative", "separate"]
            .iter()
            .any(|signal| item.contains(signal));
        let performs_math = [
            "comput",
            "calculat",
            "deriv",
            "residual",
            "substitut",
            "numerical",
            "symbolic",
            "solve",
            "proof",
        ]
        .iter()
        .any(|signal| item.contains(signal));
        let has_independent_check = uses_math_attacker
            || explicitly_rechecks
            || (establishes_independence && performs_math);
        has_gating_tag && has_independent_check
    })
}

/// Every canonical gate must appear in a gating verification item. This is a
/// coverage check, not a prescribed proof method: the plan remains free to
/// choose task-appropriate residuals, derivations, numerical tests, or N/A
/// justifications.
fn plan_has_complete_math_gate_coverage(plan: &str) -> bool {
    let gating = verification_plan_items(plan)
        .into_iter()
        .filter(|item| is_gating_verification_item(item))
        .collect::<Vec<_>>();
    MATH_VALIDATION_GATES
        .iter()
        .all(|gate| gating.iter().any(|item| affirmative_gate_mention(item, gate)))
}

fn affirmative_gate_mention(item: &str, gate: &str) -> bool {
    item.match_indices(gate).any(|(index, _)| {
        let clause_start = item[..index]
            .rfind([';', '.', ','])
            .map_or(0, |position| position + 1);
        let prefix = item[clause_start..index].trim();
        ![
            "do not",
            "don't",
            "not check",
            "not cover",
            "without",
            "skip",
            "omit",
            "ignore",
            "n/a",
            "not applicable",
        ]
        .iter()
        .any(|negation| prefix.contains(negation))
    })
}

fn numbered_item_body(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let digit_count = line.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let rest = &line[digit_count..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let body = rest.trim();
    (!body.is_empty()).then_some(body)
}

fn is_markdown_list_item(line: &str) -> bool {
    let line = line.trim_start();
    if ["- ", "* ", "+ "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    {
        return true;
    }

    numbered_item_body(line).is_some()
}

/// Delta-focused resume prompt for skeptic 0 when it is RESUMED across
/// attempts (it already carries its prior transcript and the gaps it
/// flagged). It must re-read the changed files (its cached reads are
/// stale after the agent's further edits), confirm each prior gap is
/// genuinely fixed in the CURRENT files with no regression introduced,
/// and emit the same strict verdict-file + terminal-token contract.
#[cfg(test)]
const GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE: &str = "You are the SAME adversarial verifier from the previous attempt — you have your \
prior transcript, the gaps you flagged, and the evidence you cited. You are NOT \
the agent that produced the changes. Your job is still to **refute** that the \
objective has been met. The agent claims it addressed your gaps; do NOT trust \
that — RE-CHECK. **Default to `refuted: true` if uncertain** (passing broken \
work is far worse than one more iteration).\n\n\
You have your standard tool inventory ({READ_TOOL}, {SEARCH_TOOL}, {LIST_TOOL}, \
run a command).{TOOLSET_TOOLS}\n\n\
## Delta re-check\n\n\
- Your cached reads are STALE — RE-READ the CURRENT contents of every file in \
CHANGED_FILES (and CHANGES_FILE) before judging.\n\
- For EACH prior gap, confirm it is GENUINELY fixed — not merely claimed, \
papered over, hardcoded, or stubbed. AUDIT the implementer's updated tests + \
captured evidence (CHANGED_FILES and `{IMPLEMENTER_SCRATCH}`) first; reach for \
RUNNING the code yourself only as a cheap spot-check, and reuse the \
implementer's captured run instead of expensive re-runs. A gap you cannot \
confirm is fixed remains `refuted: true`. If the fix's evidence is missing, \
refute and ask the implementer to produce it — do not build it yourself.\n\
- Check for REGRESSIONS: the changes must not break a criterion that previously \
held, an adjacent call site, or a passing test.\n\
- PRIOR_GAPS — the gaps the previous round told the implementer to fix:\n\n\
{PRIOR_GAPS}\n\n\
- The whole contract still applies (all numbered criteria + the \
`## Verification plan`), not only the gaps you flagged; refute a newly-doubtful \
criterion too. Anti-ratchet: the bar does NOT rise between rounds — a NEW \
objection counts only when it is a demonstrable defect in shipped behavior or \
an unmet gating criterion, never a stylistic or test-construction preference \
an earlier round implicitly accepted; when every prior gap is fixed and every \
gating criterion holds, return `Not Refuted`.\n\
- PLAN_CHANGES shows how the agent edited PLAN_FILE this run — a weakened, \
deleted, or self-serving criterion is itself grounds for `refuted: true`.\n\
- Cite concrete evidence per assertion (`path:line`, a captured transcript, or \
a diff hunk). Classify any refute via `blocking` as before (`\"none\"`, \
`\"contradiction\"`, or `\"unverifiable\"`).\n\
{KIND_LENS}\n\
## Scratch dirs\n\n\
- `{IMPLEMENTER_SCRATCH}` — the implementer's outputs / captured evidence, your \
PRIMARY source: READ it instead of re-running; do NOT write into it.\n\
- `{SKEPTIC_SCRATCH}` — yours, for cheap spot-checks only; when one re-runs the \
`## Verification plan`, the literal `{SCRATCH}` placeholder resolves here.\n\n\
{SCRATCH_STATUS}\n\n\
## Output contract — STRICT\n\n\
Do BOTH, then emit the terminal token.\n\n\
### 1. JSON verdict → `{VERDICT_FILE}`\n\n\
Write this object (fixed schema) with your file-write tool:\n\n\
```json\n\
{\n\
  \"refuted\": true,\n\
  \"findings\": [{\"kind\": \"bug|gap|todo\", \"location\": \"path:line or where\", \"detail\": \"one line\"}],\n\
  \"evidence\": \"string — one-line summary citation\",\n\
  \"confidence\": \"high\",\n\
  \"blocking\": \"none\",\n\
  \"math_checks\": [\n\
    {\"gate\": \"contract-closure\", \"status\": \"pass|fail|not_applicable\", \"target\": \"result/equation/artifact\", \"evidence\": \"claim-bound observation or N/A reason\"},\n\
    {\"gate\": \"derivation-integrity\", \"status\": \"pass|fail|not_applicable\", \"target\": \"result/equation/artifact\", \"evidence\": \"claim-bound observation or N/A reason\"},\n\
    {\"gate\": \"evidence-provenance\", \"status\": \"pass|fail|not_applicable\", \"target\": \"result/equation/artifact\", \"evidence\": \"claim-bound observation or N/A reason\"},\n\
    {\"gate\": \"invariant-ledger\", \"status\": \"pass|fail|not_applicable\", \"target\": \"result/equation/artifact\", \"evidence\": \"claim-bound observation or N/A reason\"},\n\
    {\"gate\": \"state-isolation\", \"status\": \"pass|fail|not_applicable\", \"target\": \"result/equation/artifact\", \"evidence\": \"claim-bound observation or N/A reason\"}\n\
  ],\n\
  \"details_md\": \"Markdown summary of your findings\"\n\
}\n\
```\n\n\
- `findings` (array — the PRIMARY output the implementer acts on): one terse item \
per gap. `kind` = `bug` (defect in shipped behavior) | `gap` (unmet criterion / \
missing test or evidence) | `todo` (TODO/`#[ignore]`/stub left in). `location` = \
`path:line` when code-related, else where. `detail` = one concrete line, no prose.\n\
- `refuted` (bool): `false` only if every prior gap is confirmed fixed and no \
regression or other criterion fails.\n\
- `evidence` (string): a one-line summary citation; for `code-change`, FINAL_RESPONSE \
prose is NOT evidence.\n\
- `confidence` (string): `\"high\"` | `\"medium\"` | `\"low\"`.\n\
- `blocking` (string, default `\"none\"`): `\"none\"` | `\"contradiction\"` | \
`\"unverifiable\"`.\n\
- `math_checks` (array): mandatory when the math lens applies and `refuted: false`; \
exactly one non-empty, claim-bound row per canonical gate. `not_applicable` needs \
a concrete reason; an approval cannot contain `fail`. Non-math verdicts may omit it.\n\
- `details_md` (string, optional): Markdown writeup; if omitted, the aggregator \
falls back to the `{DETAILS_FILE}` contents.\n\n\
### 2. Details → `{DETAILS_FILE}`\n\n\
The same findings as `details_md`, rendered as real Markdown.\n\n\
### 3. Terminal token\n\n\
Your terminal response must be **exactly** one of these and nothing else — no \
prose, fences, or punctuation; capitalization is significant:\n\n\
```\nRefuted\n```\n\nor\n\n```\nNot Refuted\n```\n\n\
`Refuted` ⇒ `refuted: true`; `Not Refuted` ⇒ `refuted: false`. The JSON is \
authoritative; the token is the fast-path signal.";

/// Wrap the evidence packet (OBJECTIVE / CHANGES_FILE / PLAN_FILE /
/// FINAL_RESPONSE) in `template`, substituting the kind-specific review
/// lens into `{KIND_LENS}`, the runner-allocated output paths into
/// `{DETAILS_FILE}` / `{VERDICT_FILE}`, and the per-runner scratch dirs
/// into `{SKEPTIC_SCRATCH}` (this skeptic's own) / `{IMPLEMENTER_SCRATCH}`
/// (the goal-wide implementer dir). Shared by the cold and resume skeptic
/// prompts; only the template differs.
#[allow(clippy::too_many_arguments)]
fn render_verifier_prompt(
    template: &str,
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    let user_prompt = evidence::build_classifier_evidence_packet(
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
        // The implementer's captured run output lives in the goal-wide
        // scratch dir; point the verifier at it explicitly so the pack
        // carries plan + changed paths + test-evidence location in one
        // payload (fewer read round-trips).
        (!implementer_scratch.is_empty()).then_some(Path::new(implementer_scratch)),
    );
    let prior_gaps_rendered = match prior_gaps {
        Some(g) if !g.trim().is_empty() => sanitize_prior_gaps(g),
        _ => "(none — first verification round)".to_string(),
    };
    let rendered = template
        .replace("{KIND_LENS}", kind_lens)
        .replace("{DETAILS_FILE}", details_path)
        .replace("{VERDICT_FILE}", verdict_path)
        .replace("{SKEPTIC_SCRATCH}", skeptic_scratch)
        .replace("{IMPLEMENTER_SCRATCH}", implementer_scratch)
        // Only claim the dirs exist when both were actually created.
        .replace(
            "{SCRATCH_STATUS}",
            if scratch_ready {
                "Both dirs have been created for you."
            } else {
                "Create your own scratch dir with `mkdir -p` if it is missing."
            },
        )
        .replace("{PRIOR_GAPS}", &prior_gaps_rendered);
    let rendered = tool_names.apply(&rendered);
    let mut out = String::with_capacity(rendered.len() + user_prompt.len() + 8);
    out.push_str(&rendered);
    out.push_str("\n\n");
    out.push_str(&user_prompt);
    out
}

/// Build the per-skeptic cold user prompt — the full adversarial
/// verifier template plus the evidence packet.
#[allow(clippy::too_many_arguments)]
fn render_skeptic_prompt(
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    render_verifier_prompt(
        GOAL_VERIFIER_PROMPT_TEMPLATE,
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
        details_path,
        verdict_path,
        kind_lens,
        skeptic_scratch,
        implementer_scratch,
        prior_gaps,
        tool_names,
        scratch_ready,
    )
}

/// Build the resumed-skeptic-0 delta prompt (see
/// [`GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE`]).
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn render_skeptic_resume_prompt(
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    render_verifier_prompt(
        GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE,
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
        details_path,
        verdict_path,
        kind_lens,
        skeptic_scratch,
        implementer_scratch,
        prior_gaps,
        tool_names,
        scratch_ready,
    )
}

/// Wrap a raw spawn failure / parse failure into a `SkepticResult` with
/// `refuted: true` (fail-closed at the skeptic level). The `note` is
/// surfaced in the aggregated details file so the user can see why this
/// skeptic produced a synthetic refute.
fn skeptic_failure(
    skeptic_idx: u32,
    details_path: String,
    note: String,
    latency_ms: u64,
) -> SkepticResult {
    SkepticResult {
        skeptic_idx,
        refuted: true,
        confidence: SkepticConfidence::Unknown,
        blocking: SkepticBlocking::None,
        evidence: String::new(),
        findings: Vec::new(),
        fallback_note: Some(note),
        details_path,
        latency_ms,
    }
}

/// Read and validate one current-round structured verdict. Terminal text can
/// tighten an approval to refuted but can never synthesize an approval.
async fn read_skeptic_verdict(
    skeptic_idx: u32,
    verdict_raw: &str,
    canonical_verdict_path: &Path,
    canonical_details_path: &Path,
    terminal: &str,
    started: std::time::Instant,
    identity: &VerdictIdentity,
    facets: &std::collections::BTreeSet<VerificationFacet>,
    reviewed_root: &Path,
    manifest: &super::verification_snapshot::ArtifactManifest,
    trace: &[super::verifier_runtime::VerificationToolEvent],
) -> SkepticResult {
    let fail = |reason: String| {
        skeptic_failure(
            skeptic_idx,
            canonical_details_path.to_string_lossy().into_owned(),
            format!("structured verdict rejected: {reason}"),
            started.elapsed().as_millis() as u64,
        )
    };
    let body = match read_bounded_verdict(Path::new(verdict_raw)) {
        Ok(body) => body,
        Err(error) => return fail(error),
    };
    let Some(verdict) = parse_verdict_json(&body) else {
        return fail("missing, malformed, or unknown-schema JSON".to_string());
    };
    if parse_skeptic_terminal_response(terminal) == Some(true) && !verdict.refuted {
        return fail("terminal refutation conflicts with structured approval".to_string());
    }
    if let Err(error) =
        validate_structured_verdict(&verdict, identity, facets, reviewed_root, manifest, trace)
    {
        return fail(error);
    }
    if let Err(error) = persist_validated_verdict(canonical_verdict_path, &verdict) {
        return fail(error);
    }

    let report = if verdict.details_md.trim().is_empty() {
        format!("# Verification receipt\n\n{}\n", verdict.evidence)
    } else {
        verdict.details_md.clone()
    };
    if std::fs::symlink_metadata(canonical_details_path).is_ok()
        || crate::util::config::atomic_write_string(canonical_details_path, &report).is_err()
    {
        return fail("critic details could not be persisted safely".to_string());
    }
    SkepticResult {
        skeptic_idx,
        refuted: verdict.refuted,
        confidence: verdict.confidence,
        blocking: verdict.blocking,
        evidence: verdict.evidence,
        findings: verdict.findings,
        fallback_note: None,
        details_path: canonical_details_path.to_string_lossy().into_owned(),
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

fn read_bounded_verdict(path: &Path) -> Result<String, String> {
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("verdict file is missing: {error}"))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_BYTES {
        return Err("verdict is not a bounded regular file".to_string());
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("verdict cannot be opened safely: {error}"))?
    };
    #[cfg(not(unix))]
    let file =
        std::fs::File::open(path).map_err(|error| format!("verdict cannot be opened: {error}"))?;
    use std::io::Read as _;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("verdict cannot be read: {error}"))?;
    if bytes.len() as u64 != metadata.len() {
        return Err("verdict changed while being read".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "verdict is not UTF-8".to_string())
}

fn persist_validated_verdict(path: &Path, verdict: &SkepticVerdict) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err("validated verdict path already exists".to_string());
    }
    let body = serde_json::to_string_pretty(verdict)
        .map_err(|error| format!("cannot serialize validated verdict: {error}"))?;
    crate::util::config::atomic_write_string(path, &body)
        .map_err(|error| format!("cannot persist validated verdict: {error}"))?;
    let reread = read_bounded_verdict(path)?;
    let reparsed = parse_verdict_json(&reread)
        .ok_or_else(|| "persisted verdict cannot be parsed".to_string())?;
    if &reparsed != verdict {
        return Err("persisted verdict changed during write".to_string());
    }
    Ok(())
}

/// Spawn one skeptic under `spawn_id`, wait for its terminal response,
/// and read the JSON verdict file. Pure per-skeptic; no telemetry
/// side-effects so the orchestrator owns event emission for both the
/// happy and failure paths uniformly.
///
/// Every invocation is cold and bound to one immutable round. Reusing a prior
/// child would also reuse its old cwd and tool state, defeating freshness.
async fn run_one_skeptic(
    spawner: &Arc<dyn GoalClassifierSpawner>,
    skeptic_idx: u32,
    inputs: &SkepticInputs<'_>,
    spawn_id: &str,
    tool_names: &RoleToolNames,
    inherit_tool_names: &RoleToolNames,
    backpressure: Option<&SpawnBackpressure>,
) -> SkepticResult {
    let started = std::time::Instant::now();
    let details_raw = format_round_details_path(inputs.verifier_id, inputs.round_id, skeptic_idx);
    let verdict_raw = format_round_verdict_path(inputs.verifier_id, inputs.round_id, skeptic_idx);
    // Soft backpressure: while live subagent token burn is over the
    // threshold, defer the spawn (bounded — never blocks the goal).
    if let Some(bp) = backpressure {
        bp.wait_soft().await;
    }
    // An unsecurable (squatted) root makes every artifact path
    // untrustworthy — fail closed, like the unsafe-path arm below.
    if let Err(err) = super::goal_tracker::ensure_goal_scratch_root(inputs.verifier_id) {
        return skeptic_failure(
            skeptic_idx,
            details_raw,
            format!("internal: could not secure the goal scratch root: {err}"),
            started.elapsed().as_millis() as u64,
        );
    }
    let skeptic_scratch = round_critic_dir(inputs.verifier_id, inputs.round_id, skeptic_idx);
    let skeptic_scratch_ready = tokio::fs::create_dir(&skeptic_scratch).await.is_ok();
    // Readiness for the verifier prompt = the implementer dir (from the
    // orchestration) AND this skeptic's own subdir both exist on disk.
    let scratch_ready = inputs.scratch_dir_ready && skeptic_scratch_ready;
    let skeptic_scratch = skeptic_scratch.to_string_lossy();
    if validate_details_path(Path::new(&details_raw)).is_err()
        || validate_details_path(Path::new(&verdict_raw)).is_err()
        || std::fs::symlink_metadata(&verdict_raw).is_ok()
        || std::fs::symlink_metadata(&details_raw).is_ok()
    {
        return skeptic_failure(
            skeptic_idx,
            details_raw,
            "internal: unsafe per-skeptic file path".to_string(),
            started.elapsed().as_millis() as u64,
        );
    }

    let identity = VerdictIdentity {
        goal_id: inputs.goal_id.to_string(),
        verification_round_id: inputs.round_id.to_string(),
        contract_digest: inputs.contract_digest.to_string(),
        reviewed_artifact_manifest_digest: inputs.artifact_manifest.manifest_digest.clone(),
        critic_id: spawn_id.to_string(),
        critic_assignment_id: format!("{}:critic-{skeptic_idx}", inputs.round_id),
    };
    let coverage: Vec<_> = inputs
        .facets
        .iter()
        .flat_map(|facet| {
            gates_for_facet(*facet)
                .iter()
                .map(move |gate| serde_json::json!({"facet": facet.as_str(), "gate": gate}))
        })
        .collect();
    let mechanical = format!(
        "\n\n## Harness-bound verification identity\n\nIDENTITY:\n{}\n\n\
         REQUIRED_COVERAGE:\n{}\n\nARTIFACT_MANIFEST: {}\n\
         STATE_CHANGED_PATHS_FROM_GOAL_START: {}\n",
        serde_json::to_string_pretty(&identity).expect("identity serializes"),
        serde_json::to_string_pretty(&coverage).expect("coverage serializes"),
        inputs.artifact_manifest_path.display(),
        serde_json::to_string(inputs.state_changed_paths).expect("paths serialize"),
    );
    let render = |tn: &RoleToolNames| {
        let mut prompt = render_skeptic_prompt(
            inputs.objective,
            inputs.changes_ref,
            inputs.changed_files,
            inputs.plan_file,
            inputs.plan_changes,
            inputs.final_response,
            &details_raw,
            &verdict_raw,
            inputs.kind_lens,
            &skeptic_scratch,
            inputs.implementer_scratch,
            inputs.prior_gaps,
            tn,
            scratch_ready,
        );
        prompt.push_str(&mechanical);
        prompt
    };
    let prompt = RoleRenderedPrompt {
        primary: render(tool_names),
        fallback: render(inherit_tool_names),
    };
    let trace_guard = match super::verifier_runtime::begin_trace(spawn_id) {
        Ok(guard) => guard,
        Err(error) => {
            return skeptic_failure(
                skeptic_idx,
                details_raw,
                error,
                started.elapsed().as_millis() as u64,
            );
        }
    };
    let outcome = spawner
        .spawn_classifier(
            spawn_id,
            skeptic_idx,
            prompt,
            Path::new(&details_raw),
            inputs.reviewed_root,
            None,
        )
        .await;
    let trace = match trace_guard.finish() {
        Ok(trace) => trace,
        Err(error) => {
            return skeptic_failure(
                skeptic_idx,
                details_raw,
                error,
                started.elapsed().as_millis() as u64,
            );
        }
    };
    match outcome {
        Ok(terminal) => {
            let canonical = inputs
                .validated_verdict_root
                .join(format!("critic-{skeptic_idx}.json"));
            let canonical_details = inputs
                .validated_verdict_root
                .join(format!("critic-{skeptic_idx}.md"));
            read_skeptic_verdict(
                skeptic_idx,
                &verdict_raw,
                &canonical,
                &canonical_details,
                &terminal,
                started,
                &identity,
                inputs.facets,
                inputs.reviewed_root,
                inputs.artifact_manifest,
                &trace,
            )
            .await
        }
        Err(SpawnError::Transport(d)) => skeptic_failure(
            skeptic_idx,
            details_raw,
            format!("transport error: {d}"),
            started.elapsed().as_millis() as u64,
        ),
        Err(SpawnError::Runtime { message, cancelled }) => skeptic_failure(
            skeptic_idx,
            details_raw,
            format!("runtime error (cancelled={cancelled}): {message}"),
            started.elapsed().as_millis() as u64,
        ),
    }
}

/// Shared per-skeptic inputs. Borrowed from the verification-stage
/// driver so each spawned skeptic shares the same evidence references.
struct SkepticInputs<'a> {
    objective: &'a str,
    final_response: &'a str,
    plan_file: Option<&'a Path>,
    /// Borrowed baseline→current plan diff, computed ONCE in
    /// [`run_verification_stage`] and shared by every skeptic (no per-skeptic
    /// clone). `None` renders the `PLAN_CHANGES: (none)` sentinel.
    plan_changes: Option<&'a str>,
    changes_ref: evidence::ChangesRef<'a>,
    changed_files: &'a [String],
    verifier_id: &'a str,
    goal_id: &'a str,
    round_id: &'a str,
    /// Kind-specific review lens (`kind_lens`), shared by every skeptic so the
    /// panel applies one consistent lens. Empty when the goal kind is absent.
    kind_lens: &'a str,
    facets: &'a std::collections::BTreeSet<VerificationFacet>,
    contract_digest: &'a str,
    reviewed_root: &'a Path,
    artifact_manifest: &'a super::verification_snapshot::ArtifactManifest,
    artifact_manifest_path: &'a Path,
    validated_verdict_root: &'a Path,
    state_changed_paths: &'a [String],
    /// The goal-wide implementer scratch dir as a string. Computed ONCE in
    /// [`run_verification_stage`] and shared by every skeptic (no per-skeptic
    /// clone); each skeptic derives its OWN dir from `verifier_id` instead.
    implementer_scratch: &'a str,
    /// Whether the implementer scratch dir was actually created (from the
    /// orchestration); combined with the skeptic's own subdir in `run_one_skeptic`.
    scratch_dir_ready: bool,
    /// Previous round's gaps summary for the `{PRIOR_GAPS}` placeholder
    /// (see [`VerificationStageInputs::prior_gaps`]).
    prior_gaps: Option<&'a str>,
}

/// Stage-level inputs threaded into [`run_verification_stage`]. Borrowed
/// throughout so the orchestrator stays pure and the test driver can
/// stamp fresh inputs per attempt without cloning.
pub(crate) struct VerificationStageInputs<'a> {
    pub goal_id: &'a str,
    pub objective: &'a str,
    pub final_response: &'a str,
    pub baseline_commit: Option<&'a str>,
    pub workspace_root: &'a Path,
    pub verifier_id: &'a str,
    pub attempt: u32,
    pub model_id: &'a str,
    pub goal_created_at: i64,
    pub plan_file: Option<&'a Path>,
    /// Path to the immutable baseline snapshot of the planner's original
    /// plan (`GoalOrchestration::plan_baseline_file`). The stage diffs the
    /// CURRENT `plan_file` against it to surface mid-run plan edits to the
    /// skeptics; `None` when no baseline was captured (planner-off goals or a
    /// snapshot failure).
    pub plan_baseline_file: Option<&'a Path>,
    /// Content-addressed state captured before the first worker round.
    pub initial_workspace_manifest_file: Option<&'a Path>,
    /// The goal-wide implementer scratch dir
    /// ([`super::goal_tracker::implementer_scratch_dir`]). Threaded into
    /// every skeptic prompt so the panel knows where the implementer wrote
    /// its build outputs / screenshots and can READ them to verify.
    pub implementer_scratch_dir: &'a Path,
    /// Whether that implementer dir was actually created (from the goal
    /// orchestration), so the verifier prompt only claims it exists when true.
    pub scratch_dir_ready: bool,
    pub skeptic_count: u32,
    /// Effective per-goal classifier cap (resolved env > remote > default), so
    /// `GoalClassifierFired` reports the real cap, not the default constant.
    pub max_runs: u32,
    /// Previous round's gaps summary (`last_classifier_gaps`), threaded into
    /// every skeptic prompt as `{PRIOR_GAPS}` so cold skeptics keep
    /// cross-round memory instead of ratcheting the bar with fresh
    /// objections each attempt. `None` on the first round.
    pub prior_gaps: Option<&'a str>,
    /// Per-skeptic-index resolved tool names for the verifier prompt
    /// placeholders, indexed by skeptic index. Built parent-side from
    /// each index's resolved toolset (explicit pair ⇒ its `describe` summary;
    /// inherit ⇒ the parent bridge). An index past the slice end (e.g. an
    /// empty slice in tests) falls back to [`RoleToolNames::inherit_defaults`].
    pub tool_names: &'a [RoleToolNames],
    /// Default/parent-toolset tool names used to render each skeptic's
    /// fail-open RETRY prompt (the retry falls back to the default toolset, so
    /// it must name THAT toolset's tools). Shared across the panel.
    pub inherit_tool_names: &'a RoleToolNames,
}

/// Outcome of [`run_verification_stage`] plus skeptic 0's child session
/// id when an N > 1 panel ran, so the next attempt can resume it. `None`
/// for the N == 1 sole-judge panel and the fail-open early-exits —
/// neither resumes.
pub(crate) struct VerificationStageResult {
    pub outcome: GoalClassifierOutcome,
    pub skeptic0_session_id: Option<String>,
    /// `true` only when the skeptic panel actually ran: the apply path
    /// keys the stored `skeptic0_session_id` overwrite on this so a
    /// fail-open early-exit cannot sever the gatekeeper resume chain
    /// (an N == 1 run still clears the id deliberately).
    pub panel_ran: bool,
}

impl From<GoalClassifierOutcome> for VerificationStageResult {
    /// Fail-open / early-exit conversion: no panel ran.
    fn from(outcome: GoalClassifierOutcome) -> Self {
        Self {
            outcome,
            skeptic0_session_id: None,
            panel_ran: false,
        }
    }
}

/// Run the verification stage: a fresh adversarial panel of `skeptic_count`
/// spawns. Approval requires every structured verdict to approve (see
/// [`aggregate_skeptic_verdicts`]); critics are never resumed across rounds.
///
/// Always emits a `GoalClassifierFired` for dashboard symmetry with the
/// legacy single classifier, then `GoalVerifierSkepticVerdict` per skeptic
/// plus an aggregate `GoalVerifierAggregateVerdict` and a final
/// `GoalClassifierVerdict`. The terminal outcome is one of `Achieved`,
/// `NotAchieved`, `Blocked`, or the legacy-named infrastructure outcome
/// `FailOpenAchieved` — same enum the drain path already consumes.
///
/// ## Cancellation
///
/// Verification runs inside the turn's `handle_prompt` (the abortable
/// running task), so a turn-cancel (`Cmd+C`) drops this future. Merely
/// dropping it does NOT notify the coordinator (it does not poll
/// `result_tx.is_closed()`), so the spawned skeptics are reaped instead
/// via the parent-prompt-id match: the `ChannelSpawner` tags each skeptic
/// `SubagentRequest` with the live `current_prompt_id`, and
/// `cancel_running_turn_subagents` → `cancel_by_parent_prompt_id` fires
/// each child's cancel token on a turn-cancel. The cancel handler also
/// pauses the goal (`UserPaused`), so a cancelled verification leaves no
/// partial verdict and the user resumes with `/goal resume`.
#[allow(clippy::too_many_lines)]
#[cfg(test)]
pub(crate) async fn run_verification_stage(
    spawner: Arc<dyn GoalClassifierSpawner>,
    inputs: VerificationStageInputs<'_>,
    emit_event: &dyn Fn(Event),
) -> VerificationStageResult {
    run_verification_stage_with_backpressure(spawner, inputs, emit_event, None).await
}

/// Like [`run_verification_stage`] but applies soft spawn backpressure
/// before each skeptic spawn. Production (goal mode) passes a gate built
/// from the session's live subagent-token pressure; tests pass `None`.
pub(crate) async fn run_verification_stage_with_backpressure(
    spawner: Arc<dyn GoalClassifierSpawner>,
    inputs: VerificationStageInputs<'_>,
    emit_event: &dyn Fn(Event),
    backpressure: Option<&SpawnBackpressure>,
) -> VerificationStageResult {
    let started = std::time::Instant::now();
    emit_event(Event::GoalClassifierFired {
        attempt: inputs.attempt,
        max_runs: inputs.max_runs,
        model_id: inputs.model_id.to_string(),
    });

    let round_id = uuid::Uuid::now_v7().to_string();
    let details_raw = format_round_panel_details_path(inputs.verifier_id, &round_id);
    let details_path = PathBuf::from(&details_raw);

    if let Err(err) = validate_details_path(&details_path) {
        tracing::warn!(
            details_path = %details_raw,
            error = %err,
            "verification stage: rejecting unsafe details path",
        );
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            None,
            String::new(),
        )
        .await
        .into();
    }
    // Re-ensure the scratch root (it can be missing after a restart),
    // BEFORE the changes-path validation: that arm's fail-open writes a
    // placeholder, which must never happen under an unverified root.
    if let Err(err) = super::goal_tracker::ensure_goal_scratch_root(inputs.verifier_id) {
        tracing::warn!(
            error = %err,
            "verification stage: failed to ensure scratch root; failing open",
        );
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            None,
            String::new(),
        )
        .await
        .into();
    }
    // Capture all verification inputs before spawning any critic. Missing
    // evidence is an infrastructure failure and cannot approve the goal.
    let captured = match evidence::capture_changes_diff(
        inputs.baseline_commit,
        inputs.workspace_root,
        inputs.goal_created_at,
    )
    .await
    {
        Ok(captured) => captured,
        // An analysis-only goal or an empty non-git workspace can
        // legitimately have no changed files. The immutable objective and
        // final-response artifacts still give the verifier a complete review
        // target; only an actual capture failure is infrastructural.
        Err(evidence::ChangesCaptureError::WalkdirEmpty) => evidence::CapturedChanges {
            diff: String::new(),
            changed_files: Vec::new(),
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "verification stage: changes capture failed",
            );
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    let sanitized = evidence::sanitize_final_response(inputs.final_response);

    // Compute the plan baseline→current diff ONCE; every skeptic shares the
    // same borrowed `&str` (no per-skeptic clone). The plan is agent-authored
    // text, so sanitize it for control tokens exactly like FINAL_RESPONSE.
    let plan_changes_raw = match (inputs.plan_baseline_file, inputs.plan_file) {
        (Some(baseline), Some(current)) => evidence::capture_plan_changes(baseline, current).await,
        _ => None,
    };
    let plan_changes_sanitized = plan_changes_raw
        .as_deref()
        .map(evidence::sanitize_final_response);

    // Select the shared review lens from the plan and retain the body for
    // source-aware defence in depth. A wrapper objective can name a local
    // requirements file which names the actual quantitative source.
    let plan_body = match inputs.plan_file {
        Some(path) => match tokio::fs::read_to_string(path).await {
            Ok(body) => Some(body),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "verification plan is unreadable");
                return record_fail_open(
                    GoalClassifierFailOpenReason::FileWriteFailed,
                    inputs.attempt,
                    started,
                    emit_event,
                    Some(&details_path),
                    details_raw,
                )
                .await
                .into();
            }
        },
        None => None,
    };
    let facets = classify_facets(
        inputs.objective,
        plan_body.as_deref().unwrap_or_default(),
        inputs.workspace_root,
    );
    let kind_lens = facet_lenses(&facets);

    let Some(initial_manifest_path) = inputs.initial_workspace_manifest_file else {
        tracing::warn!("verification stage: goal-start workspace manifest is missing");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    };
    let initial_manifest = match super::verification_snapshot::read_manifest(initial_manifest_path)
    {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(%error, "verification stage: goal-start manifest is invalid");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };

    let facet_names: Vec<_> = facets.iter().map(|facet| facet.as_str()).collect();
    let contract_bytes = match serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "goal_id": inputs.goal_id,
        "objective": inputs.objective,
        "plan": plan_body.as_deref().unwrap_or_default(),
        "applicable_facets": facet_names,
        "initial_workspace_manifest_digest": initial_manifest.manifest_digest,
    })) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "verification stage: contract serialization failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    let contract_digest = super::verification_snapshot::digest_bytes(&contract_bytes);
    let initial_manifest_bytes = match serde_json::to_vec_pretty(&initial_manifest) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "verification stage: initial manifest serialization failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    let virtual_artifacts = vec![
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/objective.txt".to_string(),
            bytes: inputs.objective.as_bytes().to_vec(),
        },
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/final-response.md".to_string(),
            bytes: sanitized.as_bytes().to_vec(),
        },
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/plan.md".to_string(),
            bytes: plan_body.as_deref().unwrap_or_default().as_bytes().to_vec(),
        },
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/changes.patch".to_string(),
            bytes: captured.diff.as_bytes().to_vec(),
        },
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/initial-workspace-manifest.json".to_string(),
            bytes: initial_manifest_bytes,
        },
        super::verification_snapshot::VirtualArtifact {
            path: ".ds-verification/contract.json".to_string(),
            bytes: contract_bytes.clone(),
        },
    ];
    let workspace_root = inputs.workspace_root.to_path_buf();
    let scratch_root = super::goal_tracker::goal_scratch_root(inputs.verifier_id);
    let snapshot_round_id = round_id.clone();
    let snapshot = match tokio::task::spawn_blocking(move || {
        super::verification_snapshot::create_reviewed_snapshot(
            &workspace_root,
            &scratch_root,
            &snapshot_round_id,
            &virtual_artifacts,
        )
    })
    .await
    {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => {
            tracing::warn!(%error, "verification stage: reviewed snapshot creation failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
        Err(error) => {
            tracing::warn!(%error, "verification stage: snapshot task failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    let state_changed_paths =
        match super::verification_snapshot::changed_paths(&initial_manifest, &snapshot.manifest) {
            Ok(paths) => paths,
            Err(error) => {
                tracing::warn!(%error, "verification stage: state comparison failed");
                return record_fail_open(
                    GoalClassifierFailOpenReason::FileWriteFailed,
                    inputs.attempt,
                    started,
                    emit_event,
                    Some(&details_path),
                    details_raw,
                )
                .await
                .into();
            }
        };
    // Validated receipts live in the durable session goal directory, not the
    // temporary critic scratch tree that terminal goal transitions remove.
    let goal_audit_root = match initial_manifest_path.parent() {
        Some(path) => path.join("verification-rounds"),
        None => {
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    if let Err(error) = tokio::fs::create_dir_all(&goal_audit_root).await {
        tracing::warn!(%error, "verification stage: durable audit root failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    let durable_round_root = goal_audit_root.join(&round_id);
    if let Err(error) = tokio::fs::create_dir(&durable_round_root).await {
        tracing::warn!(%error, "verification stage: durable round directory failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    let validated_verdict_root = durable_round_root.join("validated-verdicts");
    if let Err(error) = tokio::fs::create_dir(&validated_verdict_root).await {
        tracing::warn!(%error, "verification stage: validated verdict directory failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    if let Err(error) = super::verification_snapshot::persist_manifest(
        &durable_round_root.join("artifact-manifest.json"),
        &snapshot.manifest,
    ) {
        tracing::warn!(%error, "verification stage: durable artifact manifest failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    let contract_body = String::from_utf8(contract_bytes.clone())
        .expect("serialized verification contract is UTF-8 JSON");
    if let Err(error) = crate::util::config::atomic_write_string(
        &durable_round_root.join("contract.json"),
        &contract_body,
    ) {
        tracing::warn!(%error, "verification stage: durable contract persistence failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    let changes_path = snapshot.root.join(".ds-verification/changes.patch");
    let changes_raw = changes_path.to_string_lossy().into_owned();
    let snapshot_plan_path = plan_body
        .as_ref()
        .map(|_| snapshot.root.join(".ds-verification/plan.md"));
    let changes_ref = evidence::ChangesRef::File(&changes_raw);

    let implementer_scratch = inputs.implementer_scratch_dir.to_string_lossy();

    let n = inputs
        .skeptic_count
        .clamp(GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX);
    // Per-index tool names for the prompt placeholders; an index past the
    // provided slice (e.g. an empty slice in tests) falls back to the
    // parent-toolset defaults so the prompt still renders fully.
    let default_tool_names = RoleToolNames::inherit_defaults();
    let tool_names_for = |idx: u32| -> &RoleToolNames {
        inputs
            .tool_names
            .get(idx as usize)
            .unwrap_or(&default_tool_names)
    };
    let skeptic_inputs = SkepticInputs {
        objective: inputs.objective,
        final_response: sanitized.as_ref(),
        plan_file: snapshot_plan_path.as_deref(),
        plan_changes: plan_changes_sanitized.as_deref(),
        changes_ref,
        changed_files: &state_changed_paths,
        verifier_id: inputs.verifier_id,
        goal_id: inputs.goal_id,
        round_id: &round_id,
        kind_lens: &kind_lens,
        facets: &facets,
        contract_digest: &contract_digest,
        reviewed_root: &snapshot.root,
        artifact_manifest: &snapshot.manifest,
        artifact_manifest_path: &snapshot.manifest_path,
        validated_verdict_root: &validated_verdict_root,
        state_changed_paths: &state_changed_paths,
        implementer_scratch: implementer_scratch.as_ref(),
        scratch_dir_ready: inputs.scratch_dir_ready,
        prior_gaps: inputs.prior_gaps,
    };

    // Every critic is cold, sees the same immutable snapshot, and receives a
    // round-unique identity. Resume never reuses a verdict namespace.
    let critic_ids: Vec<String> = (0..n).map(|_| uuid::Uuid::now_v7().to_string()).collect();
    let spawns = (0..n).zip(&critic_ids).map(|(idx, id)| {
        run_one_skeptic(
            &spawner,
            idx,
            &skeptic_inputs,
            id.as_str(),
            tool_names_for(idx),
            inputs.inherit_tool_names,
            backpressure,
        )
    });
    let results = futures::future::join_all(spawns).await;

    for r in &results {
        emit_event(Event::GoalVerifierSkepticVerdict {
            attempt: inputs.attempt,
            skeptic_idx: r.skeptic_idx,
            refuted: r.refuted,
            confidence: r.confidence.as_const_str(),
            latency_ms: r.latency_ms,
        });
    }
    let (refuted_count, total, all_approved) = aggregate_skeptic_verdicts(&results);
    let achieved =
        total == n && all_approved && results.iter().all(|result| result.fallback_note.is_none());
    emit_event(Event::GoalVerifierAggregateVerdict {
        attempt: inputs.attempt,
        refuted_count,
        total,
        achieved,
    });

    let body = render_skeptic_panel_details(
        &results,
        refuted_count,
        total,
        achieved,
        inputs.verifier_id,
        inputs.attempt,
    );
    if let Err(error) = write_details_file(&details_path, &body).await {
        tracing::warn!(%error, "verification stage: aggregate details persistence failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }
    let review_root = snapshot.root.clone();
    let review_manifest = snapshot.manifest.clone();
    match tokio::task::spawn_blocking(move || {
        super::verification_snapshot::verify_reviewed_snapshot(&review_root, &review_manifest)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "verification stage: reviewed snapshot changed during audit");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
        Err(error) => {
            tracing::warn!(%error, "verification stage: snapshot recheck task failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    }
    let post_workspace_root = inputs.workspace_root.to_path_buf();
    let post_manifest = match tokio::task::spawn_blocking(move || {
        super::verification_snapshot::capture_workspace_manifest(&post_workspace_root)
    })
    .await
    {
        Ok(Ok(manifest)) => manifest,
        Ok(Err(error)) => {
            tracing::warn!(%error, "verification stage: post-audit workspace capture failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
        Err(error) => {
            tracing::warn!(%error, "verification stage: post-audit capture task failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    };
    match super::verification_snapshot::changed_paths(&post_manifest, &snapshot.manifest) {
        Ok(paths) if paths.is_empty() => {}
        Ok(paths) => {
            tracing::warn!(?paths, "verification stage: workspace changed during audit");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
        Err(error) => {
            tracing::warn!(%error, "verification stage: post-audit state comparison failed");
            return record_fail_open(
                GoalClassifierFailOpenReason::FileWriteFailed,
                inputs.attempt,
                started,
                emit_event,
                Some(&details_path),
                details_raw,
            )
            .await
            .into();
        }
    }
    let aggregate_path = durable_round_root.join("aggregate-verdict.json");
    if let Err(error) = persist_aggregate_receipt(
        &aggregate_path,
        inputs.goal_id,
        &round_id,
        &contract_digest,
        &snapshot.manifest.manifest_digest,
        &facets,
        &results,
        achieved,
    ) {
        tracing::warn!(%error, "verification stage: aggregate receipt persistence failed");
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }

    if results.iter().any(|result| result.fallback_note.is_some()) {
        let outcome = record_fail_open(
            GoalClassifierFailOpenReason::SamplerError,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await;
        return VerificationStageResult {
            outcome,
            skeptic0_session_id: None,
            panel_ran: true,
        };
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    let verdict = if achieved {
        GoalClassifierVerdict::Achieved
    } else {
        GoalClassifierVerdict::NotAchieved
    };
    emit_event(Event::GoalClassifierVerdict {
        verdict: verdict.into(),
        attempt: inputs.attempt,
        latency_ms,
    });

    if achieved {
        return VerificationStageResult {
            outcome: GoalClassifierOutcome::Achieved {
                details_path: details_raw,
            },
            skeptic0_session_id: None,
            panel_ran: true,
        };
    }

    // Route to Blocked only when EVERY refuter is a non-model-fixable
    // blocker (contradiction / unverifiable); a single fixable gap means
    // the loop can still make progress, so it stays NotAchieved.
    //
    // A lone blocking refuter (peers not refuting) is enough to route here
    // by design: Blocked is a fail-safe, resume-recoverable PAUSE, never an
    // approval — `decisive_refute` already forced not-achieved above, so
    // the only question is nudge-and-retry vs ask-the-user. With no fixable
    // gap to retry on, a high-confidence `unverifiable`/`contradiction`
    // legitimately needs a user decision; over-pausing is cheaply undone by
    // a resume, whereas nudging a model against an unfixable blocker is not.
    let all_blocking = results.iter().any(|r| r.refuted)
        && results
            .iter()
            .filter(|r| r.refuted)
            .all(|r| r.blocking.is_blocking());
    let outcome = if all_blocking {
        GoalClassifierOutcome::Blocked {
            details_path: details_raw,
            pause_summary: build_pause_summary(&results),
        }
    } else {
        let gap_fingerprint = gap_fingerprint(
            &results
                .iter()
                .filter(|r| r.refuted)
                .map(refuter_fingerprint_source)
                .collect::<Vec<_>>(),
        );
        GoalClassifierOutcome::NotAchieved {
            details_path: details_raw,
            gaps_summary: build_gaps_summary(&results),
            pause_summary: build_pause_summary(&results),
            gap_fingerprint,
        }
    };
    VerificationStageResult {
        outcome,
        skeptic0_session_id: None,
        panel_ran: true,
    }
}

/// Render the aggregated details file the rejection directive points the
/// model at: the headline, the concise `## Gaps to fix` checklist, and a
/// reference line listing the per-skeptic report paths (their full reasoning
/// stays in those files, not embedded here). Capped at
/// [`GOAL_VERIFIER_PANEL_MAX_BYTES`].
fn render_skeptic_panel_details(
    results: &[SkepticResult],
    refuted_count: u32,
    total: u32,
    achieved: bool,
    _verifier_id: &str,
    _attempt: u32,
) -> String {
    let headline = if achieved {
        format!(
            "# Goal verification — Achieved\n\n\
             {refuted_count} of {total} skeptics refuted; every structured verdict approved.\n\n"
        )
    } else {
        format!(
            "# Goal verification — Not Achieved\n\n\
             {refuted_count} of {total} skeptics refuted; panel rejected the claim.\n\n"
        )
    };

    // Per-skeptic report paths are round-unique and sorted for a stable listing.
    let mut by_idx: Vec<&SkepticResult> = results.iter().collect();
    by_idx.sort_by_key(|r| r.skeptic_idx);
    let paths: Vec<String> = by_idx
        .iter()
        .map(|result| result.details_path.clone())
        .collect();

    let mut out = String::with_capacity(headline.len() + 1024);
    out.push_str(&headline);
    if !achieved {
        let gaps = build_gaps_summary(results);
        if !gaps.is_empty() {
            out.push_str("## Gaps to fix\n\n");
            out.push_str(&gaps);
            out.push_str("\n\n");
        }
    }
    if !paths.is_empty() {
        if achieved {
            out.push_str("Per-skeptic reports: ");
        } else {
            out.push_str(
                "Fix the gaps above — they are what matters. For the full reasoning \
                 behind each, open the per-skeptic report files: ",
            );
        }
        out.push_str(&paths.join(", "));
        out.push('\n');
    }
    cap_panel_details(out)
}

/// Truncate the rendered panel to [`GOAL_VERIFIER_PANEL_MAX_BYTES`] at a
/// UTF-8 boundary, appending an explicit elision marker. Overall cap
/// only — never per-line — mirroring `evidence::truncate_diff`.
fn cap_panel_details(body: String) -> String {
    if body.len() <= GOAL_VERIFIER_PANEL_MAX_BYTES {
        return body;
    }
    let mut cut = GOAL_VERIFIER_PANEL_MAX_BYTES;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    // Count from the post-walk cut so the marker reports the exact
    // elided byte count, not the pre-boundary-walk approximation.
    let elided = body.len() - cut;
    let mut out = String::with_capacity(cut + 64);
    out.push_str(&body[..cut]);
    out.push_str(&format!(
        "\n... (panel details truncated, {elided} bytes elided) ...\n"
    ));
    out
}

async fn write_details_file(path: &Path, body: &str) -> std::io::Result<()> {
    write_patch_file_atomic(path, body).await
}

#[allow(clippy::too_many_arguments)]
fn persist_aggregate_receipt(
    path: &Path,
    goal_id: &str,
    round_id: &str,
    contract_digest: &str,
    manifest_digest: &str,
    facets: &std::collections::BTreeSet<VerificationFacet>,
    results: &[SkepticResult],
    achieved: bool,
) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err("aggregate receipt path already exists".to_string());
    }
    let receipt = serde_json::json!({
        "schema_version": 1,
        "goal_id": goal_id,
        "verification_round_id": round_id,
        "contract_digest": contract_digest,
        "reviewed_artifact_manifest_digest": manifest_digest,
        "applicable_facets": facets.iter().map(|facet| facet.as_str()).collect::<Vec<_>>(),
        "achieved": achieved,
        "critics": results.iter().map(|result| serde_json::json!({
            "critic_index": result.skeptic_idx,
            "refuted": result.refuted,
            "structured_verdict_accepted": result.fallback_note.is_none(),
            "details_path": result.details_path,
        })).collect::<Vec<_>>(),
    });
    let body = serde_json::to_string_pretty(&receipt)
        .map_err(|error| format!("cannot serialize aggregate receipt: {error}"))?;
    crate::util::config::atomic_write_string(path, &body)
        .map_err(|error| format!("cannot persist aggregate receipt: {error}"))?;
    let reread = read_bounded_verdict(path)?;
    let reparsed: serde_json::Value = serde_json::from_str(&reread)
        .map_err(|error| format!("persisted aggregate receipt is malformed: {error}"))?;
    if reparsed != receipt {
        return Err("persisted aggregate receipt changed during write".to_string());
    }
    Ok(())
}

// Test helpers (shared between this module's tests and acp_session's
// drain-path tests; gated by `#[cfg(test)]` so prod builds don't carry
// them).

/// Pull the runner-allocated `{VERDICT_FILE}` path out of a rendered
/// verifier prompt. `None` if the prompt doesn't contain one (e.g. a
/// non-verifier mock). Shared by `goal_classifier::tests::MockSpawner`
/// and `acp_session::goal_classifier_e2e_tests::MockCoordinator`.
#[cfg(test)]
pub(crate) fn parse_verdict_path_from_prompt(prompt: &str) -> Option<String> {
    parse_backticked_path(prompt, "### 1. JSON verdict → `")
        .or_else(|| parse_prompt_path(prompt, "goal-verdict-", ".json"))
}

/// Pull the per-skeptic `{DETAILS_FILE}` path out of a rendered verifier
/// prompt (anchor: the `-skeptic-` file-name marker). Shared by the
/// classifier and strategist e2e suites.
#[cfg(test)]
pub(crate) fn parse_skeptic_details_path_from_prompt(prompt: &str) -> Option<String> {
    parse_backticked_path(prompt, "### 2. Details → `")
        .or_else(|| parse_prompt_path(prompt, "-skeptic-", ".md"))
}

#[cfg(test)]
fn parse_backticked_path(prompt: &str, heading: &str) -> Option<String> {
    let (_, tail) = prompt.split_once(heading)?;
    let (path, _) = tail.split_once('`')?;
    (!path.trim().is_empty()).then(|| path.to_string())
}

/// Extract an absolute artifact path from a rendered prompt: the files
/// live under the per-goal scratch root (an arbitrary temp-dir path),
/// so anchor on a stable file-name `marker`, walk back to the start of
/// the whitespace/backtick-delimited token, and end at `suffix`.
#[cfg(test)]
fn parse_prompt_path(prompt: &str, marker: &str, suffix: &str) -> Option<String> {
    let marker = prompt.find(marker)?;
    let start = prompt[..marker]
        .rfind(|c: char| c.is_whitespace() || c == '`')
        .map_or(0, |i| i + 1);
    let tail = &prompt[start..];
    let end = tail.find(suffix)?;
    Some(tail[..end + suffix.len()].to_string())
}

/// Upgrade a concise legacy canned verdict into a fully round-bound receipt.
/// Test coordinators use this to exercise production validation without
/// hard-coding UUIDs or artifact digests that the stage owns.
#[cfg(test)]
pub(crate) fn bind_test_verdict(prompt: &str, reviewed_root: &Path, raw: &str) -> String {
    fn prompt_json_between(prompt: &str, start: &str, end: &str) -> serde_json::Value {
        let body = prompt
            .split_once(start)
            .and_then(|(_, tail)| tail.split_once(end).map(|(body, _)| body))
            .expect("mock prompt contains harness identity block");
        serde_json::from_str(body.trim()).expect("mock prompt identity JSON parses")
    }

    let Ok(legacy) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    if legacy.get("verdict_schema_version").is_some() {
        return raw.to_string();
    }
    let identity = prompt_json_between(prompt, "IDENTITY:\n", "\n\nREQUIRED_COVERAGE:");
    let coverage = prompt_json_between(prompt, "REQUIRED_COVERAGE:\n", "\n\nARTIFACT_MANIFEST:");
    let coverage = coverage.as_array().expect("coverage is an array");
    let math_required = coverage.iter().any(|row| row["facet"] == "math");
    if math_required && legacy.get("math_checks").is_none() {
        // Preserve the deliberately incomplete response used by the
        // missing-coverage regression test.
        return raw.to_string();
    }

    let manifest = super::verification_snapshot::read_manifest(
        &reviewed_root.join(super::verification_snapshot::SNAPSHOT_MANIFEST_PATH),
    )
    .expect("mock reviewed manifest parses");
    let artifact = [
        ".ds-verification/final-response.md",
        ".ds-verification/objective.txt",
    ]
    .into_iter()
    .find_map(|path| {
        let entry = manifest.entries.iter().find(|entry| entry.path == path)?;
        let text = std::fs::read_to_string(reviewed_root.join(path)).ok()?;
        let target = text
            .lines()
            .find(|line| !line.trim().is_empty())?
            .to_string();
        Some((path.to_string(), entry.sha256.clone()?, target))
    })
    .expect("mock snapshot has a non-empty virtual artifact");
    let refuted = legacy["refuted"].as_bool().unwrap_or(true);
    let mut failed_one = false;
    let checks: Vec<_> = coverage
        .iter()
        .map(|row| {
            let facet = row["facet"].as_str().unwrap();
            let gate = row["gate"].as_str().unwrap();
            if facet == "math" && gate == "evidence-provenance" {
                return serde_json::json!({
                    "gate": gate,
                    "facet": facet,
                    "status": "not_applicable",
                    "target": "No tool-backed claim was submitted in this mock",
                    "evidence": "The remaining mathematical gates inspect the artifact directly",
                    "artifact_path": "",
                    "artifact_sha256": "",
                    "method": "manual derivation review",
                    "applicability_basis": "No symbolic or numerical tool evidence is asserted"
                });
            }
            let status = if refuted && !failed_one {
                failed_one = true;
                "fail"
            } else {
                "pass"
            };
            serde_json::json!({
                "gate": gate,
                "facet": facet,
                "status": status,
                "target": artifact.2.as_str(),
                "evidence": legacy["evidence"].as_str().unwrap_or("mock receipt"),
                "artifact_path": artifact.0.as_str(),
                "artifact_sha256": artifact.1.as_str(),
                "method": "manual artifact review"
            })
        })
        .collect();
    serde_json::json!({
        "verdict_schema_version": 1,
        "goal_id": identity["goal_id"],
        "verification_round_id": identity["verification_round_id"],
        "contract_digest": identity["contract_digest"],
        "reviewed_artifact_manifest_digest": identity["reviewed_artifact_manifest_digest"],
        "critic_id": identity["critic_id"],
        "critic_assignment_id": identity["critic_assignment_id"],
        "refuted": refuted,
        "evidence": legacy["evidence"].as_str().unwrap_or("mock receipt"),
        "confidence": legacy["confidence"].as_str().unwrap_or("unknown"),
        "blocking": legacy.get("blocking").and_then(serde_json::Value::as_str).unwrap_or("none"),
        "details_md": legacy.get("details_md").and_then(serde_json::Value::as_str).unwrap_or(""),
        "findings": if refuted {
            serde_json::json!([{
                "kind": "bug",
                "location": artifact.0.as_str(),
                "detail": legacy["evidence"].as_str().unwrap_or("mock refutation")
            }])
        } else {
            serde_json::json!([])
        },
        "checks": checks
    })
    .to_string()
}

// Tests

#[cfg(test)]
#[path = "goal_classifier/unit_tests.rs"]
mod tests;
