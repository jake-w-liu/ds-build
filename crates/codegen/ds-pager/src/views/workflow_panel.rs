//! Claude Code–style orchestrated workflow progress view.
//!
//! Derives a multi-phase progress model from the active goal + live subagent
//! sessions and renders a two-column panel:
//!
//! ```text
//!  Phases          │  Execute · 2 agents
//!  ✓ Plan    1/1   │  ✓ planner          model · 12.1k tok   1m20s
//!  ▶ Execute 1/2   │  ⟳ worker (retry 1) model · 94.3k tok   5m10s
//!    Verify  0/1   │
//! ```
//!
//! Used by the goal-detail overlay (`g` toggle / auto-open) so multi-agent
//! goal work reads as an orchestrated workflow rather than a flat task list.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::agent::{GoalDisplayPhase, GoalDisplayState, GoalDisplayStatus};
use crate::app::subagent::{SubagentInfo, format_subagent_label};
use crate::render::SafeBuf;
use crate::theme::Theme;
use crate::util::format_duration;
use crate::views::agent_status::format_tokens_compact;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Lifecycle status of one named workflow phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPhaseStatus {
    Pending,
    Active,
    Complete,
    Failed,
}

/// One phase in the orchestrated workflow (Plan → Execute → Verify).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPhase {
    pub id: &'static str,
    pub name: &'static str,
    pub status: WorkflowPhaseStatus,
    pub agents_done: u32,
    pub agents_total: u32,
}

/// One agent row inside the active (or selected) phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowAgentRow {
    /// Canonical row identity: the child's session id.
    ///
    /// Must match `workflow_selected` and `Action::WorkflowDrillDown` so
    /// highlight, j/k selection, and Enter drill-down agree even when the
    /// shell's `subagent_id` differs from `child_session_id`.
    pub id: String,
    pub label: String,
    pub phase_id: &'static str,
    pub status: WorkflowAgentStatus,
    pub retry: u32,
    pub model: Option<String>,
    pub tokens: u64,
    pub duration: std::time::Duration,
    /// Live activity ("Thinking", "Running: cargo build") from
    /// `SubagentProgress` ticks; `None` when idle/finished.
    pub activity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowAgentStatus {
    Running,
    Complete,
    Failed,
    Cancelled,
}

/// Full derived workflow snapshot for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSnapshot {
    pub objective: String,
    pub status: GoalDisplayStatus,
    pub elapsed_ms: u64,
    pub phases: Vec<WorkflowPhase>,
    pub agents: Vec<WorkflowAgentRow>,
    /// Id of the phase currently focused in the agents column.
    pub active_phase_id: &'static str,
    /// Phase the user is VIEWING (browsed to with Left/Right), when it
    /// differs from the pipeline's active phase. View-only: the ▶ marker
    /// and the phase statuses stay on `active_phase_id`. `None` = follow
    /// the active phase.
    pub view_phase_id: Option<&'static str>,
    pub total_agents: u32,
    pub completed_agents: u32,
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// Classify a subagent into a workflow phase from role / type / description.
pub fn classify_subagent_phase(info: &SubagentInfo) -> &'static str {
    let role = info.role.as_deref().unwrap_or("").to_ascii_lowercase();
    let typ = info.subagent_type.as_ref().to_ascii_lowercase();
    let desc = info.description.as_ref().to_ascii_lowercase();
    let blob = format!("{role} {typ} {desc}");

    if blob.contains("plan") || blob.contains("planner") {
        "plan"
    } else if blob.contains("verif")
        || blob.contains("skeptic")
        || blob.contains("attacker")
        || blob.contains("audit")
        || blob.contains("review")
        || blob.contains("classif")
    {
        "verify"
    } else {
        // Default worker / general-purpose / implementer → Execute.
        "execute"
    }
}

fn agent_status(info: &SubagentInfo) -> WorkflowAgentStatus {
    if !info.finished {
        return WorkflowAgentStatus::Running;
    }
    match info.status.as_deref() {
        Some("failed") => WorkflowAgentStatus::Failed,
        Some("cancelled") | Some("canceled") => WorkflowAgentStatus::Cancelled,
        _ if info.error.is_some() => WorkflowAgentStatus::Failed,
        _ => WorkflowAgentStatus::Complete,
    }
}

fn retry_count(info: &SubagentInfo) -> u32 {
    // "resumed" context or an explicit "(retry N)" in the description.
    let mut n = 0u32;
    if info
        .context_source
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("resumed"))
    {
        n = n.saturating_add(1);
    }
    if let Some(idx) = info.description.find("retry ") {
        let rest = &info.description[idx + "retry ".len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = digits.parse::<u32>() {
            n = n.max(v);
        }
    }
    n
}

/// Structured phase from the shell when present (goal subagents), falling
/// back to the keyword sniffer only for non-goal sessions.
///
/// The structured value is authoritative: a verifier the model happened to
/// name "quality-gate-1" still lands in Verify because the shell emitted
/// `goal_phase = "verify"` on the wire, not because its description matched
/// a keyword. Unknown structured values default to Execute (they are not
/// plan/verify agents by the shell's own accounting).
pub(crate) fn structured_or_sniffed_phase(info: &SubagentInfo) -> &'static str {
    match info.goal_phase.as_deref() {
        Some("plan") => "plan",
        Some("verify") => "verify",
        Some(_) => "execute",
        None => classify_subagent_phase(info),
    }
}

/// Structured attempt/round from the shell when present (goal subagents),
/// falling back to the prose sniffer only for non-goal sessions.
///
/// `goal_attempt` is 1-based; the panel shows 0-based retries (`attempt - 1`)
/// so a first-round agent renders "retry 0" equivalent (no retry badge),
/// matching the prose-derived convention.
fn structured_or_sniffed_retry(info: &SubagentInfo) -> u32 {
    info.goal_attempt
        .map(|a| a.saturating_sub(1))
        .unwrap_or_else(|| retry_count(info))
}

/// Build a Claude-style workflow snapshot from the live goal + subagents.
///
/// Phases are always Plan / Execute / Verify. Status is inferred from the
/// goal's phase flags and per-phase agent progress so the panel stays useful
/// even when the shell has not yet emitted an explicit multi-phase wire
/// payload.
pub fn derive_workflow_snapshot(
    goal: &GoalDisplayState,
    subagents: &HashMap<String, SubagentInfo>,
) -> WorkflowSnapshot {
    let mut agents: Vec<WorkflowAgentRow> = subagents
        .values()
        .map(|info| {
            let (type_label, desc) = format_subagent_label(info);
            let label = if desc.is_empty() {
                type_label
            } else if type_label.is_empty() {
                desc
            } else {
                // Prefer short description for the agent column (Claude style:
                // `adr:022`, `integrate:nodes`).
                if desc.len() <= 48 {
                    desc
                } else {
                    format!("{type_label} · {desc}")
                }
            };
            WorkflowAgentRow {
                // Child session id is the single source of truth for
                // selection highlight, j/k order, and WorkflowDrillDown.
                id: info.child_session_id.to_string(),
                label,
                phase_id: structured_or_sniffed_phase(info),
                status: agent_status(info),
                retry: structured_or_sniffed_retry(info),
                model: info.model.as_ref().map(|m| m.to_string()),
                tokens: info.tokens_used.unwrap_or(0),
                duration: info.display_elapsed(),
                activity: info.activity_label.clone(),
            }
        })
        .collect();

    // Running first, then newest (longest-running first among running).
    agents.sort_by(|a, b| {
        let ar = matches!(a.status, WorkflowAgentStatus::Running);
        let br = matches!(b.status, WorkflowAgentStatus::Running);
        br.cmp(&ar)
            .then(b.duration.cmp(&a.duration))
            .then(a.label.cmp(&b.label))
    });

    let count_phase = |id: &str| -> (u32, u32) {
        let mut done = 0u32;
        let mut total = 0u32;
        for a in &agents {
            if a.phase_id == id {
                total = total.saturating_add(1);
                if matches!(
                    a.status,
                    WorkflowAgentStatus::Complete | WorkflowAgentStatus::Cancelled
                ) {
                    done = done.saturating_add(1);
                }
            }
        }
        (done, total)
    };

    let (plan_done, plan_total) = count_phase("plan");
    let (exec_done, exec_total) = count_phase("execute");
    let (verify_done, verify_total) = count_phase("verify");

    // Infer active phase: shell flags win; otherwise first incomplete phase
    // that has agents, else the goal's GoalDisplayPhase.
    let active_phase_id = if goal.verifying_completion {
        "verify"
    } else if goal.planning || goal.phase == GoalDisplayPhase::Planning {
        "plan"
    } else if goal.phase == GoalDisplayPhase::Executing {
        "execute"
    } else if verify_total > 0 && verify_done < verify_total {
        "verify"
    } else if exec_total > 0 && exec_done < exec_total {
        "execute"
    } else if plan_total > 0 && plan_done < plan_total {
        "plan"
    } else {
        match goal.phase {
            GoalDisplayPhase::Planning => "plan",
            GoalDisplayPhase::Executing => "execute",
            GoalDisplayPhase::Idle => {
                if goal.status == GoalDisplayStatus::Complete {
                    "verify"
                } else {
                    "execute"
                }
            }
        }
    };

    let phase_status = |id: &str, done: u32, total: u32| -> WorkflowPhaseStatus {
        if goal.status == GoalDisplayStatus::Complete {
            return WorkflowPhaseStatus::Complete;
        }
        if id == active_phase_id && goal.status == GoalDisplayStatus::Active {
            return WorkflowPhaseStatus::Active;
        }
        // Ordering: plan → execute → verify. A later phase being active
        // implies earlier ones are complete (unless they failed).
        let order = |p: &str| match p {
            "plan" => 0,
            "execute" => 1,
            _ => 2,
        };
        let active_ord = order(active_phase_id);
        let this_ord = order(id);
        if this_ord < active_ord || (total > 0 && done >= total) {
            WorkflowPhaseStatus::Complete
        } else if total > 0
            && agents
                .iter()
                .any(|a| a.phase_id == id && matches!(a.status, WorkflowAgentStatus::Failed))
            && done < total
            && agents
                .iter()
                .filter(|a| a.phase_id == id)
                .all(|a| !matches!(a.status, WorkflowAgentStatus::Running))
        {
            // All agents finished and at least one failed → phase failed.
            if agents
                .iter()
                .filter(|a| a.phase_id == id)
                .all(|a| matches!(a.status, WorkflowAgentStatus::Failed | WorkflowAgentStatus::Cancelled | WorkflowAgentStatus::Complete))
                && agents
                    .iter()
                    .any(|a| a.phase_id == id && matches!(a.status, WorkflowAgentStatus::Failed))
            {
                WorkflowPhaseStatus::Failed
            } else {
                WorkflowPhaseStatus::Pending
            }
        } else {
            WorkflowPhaseStatus::Pending
        }
    };

    // When a phase has no agents yet, show a synthetic total of 1 so the
    // column still reads as "0/1" like Claude's pending phases.
    let synth = |done: u32, total: u32| -> (u32, u32) {
        if total == 0 {
            (0, 1)
        } else {
            (done, total)
        }
    };
    let (pd, pt) = synth(plan_done, plan_total);
    let (ed, et) = synth(exec_done, exec_total);
    let (vd, vt) = synth(verify_done, verify_total);

    // If the goal is complete, mark synthetic totals as done.
    let (pd, pt, ed, et, vd, vt) = if goal.status == GoalDisplayStatus::Complete {
        (
            pt.max(1),
            pt.max(1),
            et.max(1),
            et.max(1),
            vt.max(1),
            vt.max(1),
        )
    } else {
        (pd, pt, ed, et, vd, vt)
    };

    let phases = vec![
        WorkflowPhase {
            id: "plan",
            name: "Plan",
            status: phase_status("plan", pd, pt),
            agents_done: pd,
            agents_total: pt,
        },
        WorkflowPhase {
            id: "execute",
            name: "Execute",
            status: phase_status("execute", ed, et),
            agents_done: ed,
            agents_total: et,
        },
        WorkflowPhase {
            id: "verify",
            name: "Verify",
            status: phase_status("verify", vd, vt),
            agents_done: vd,
            agents_total: vt,
        },
    ];

    let total_agents = agents.len() as u32;
    let completed_agents = agents
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                WorkflowAgentStatus::Complete | WorkflowAgentStatus::Cancelled
            )
        })
        .count() as u32;

    WorkflowSnapshot {
        objective: goal.objective.clone(),
        status: goal.status,
        elapsed_ms: goal.live_elapsed_ms(),
        phases,
        agents,
        active_phase_id,
        view_phase_id: None,
        total_agents,
        completed_agents,
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

fn phase_icon(status: WorkflowPhaseStatus, active: bool) -> &'static str {
    match status {
        WorkflowPhaseStatus::Complete => "✓",
        WorkflowPhaseStatus::Failed => "✗",
        WorkflowPhaseStatus::Active => {
            if active {
                "▶"
            } else {
                "·"
            }
        }
        WorkflowPhaseStatus::Pending => " ",
    }
}

fn agent_icon(status: WorkflowAgentStatus) -> &'static str {
    match status {
        WorkflowAgentStatus::Complete => "✓",
        WorkflowAgentStatus::Failed => "✗",
        WorkflowAgentStatus::Cancelled => "–",
        WorkflowAgentStatus::Running => "⟳",
    }
}

/// One-line summary for the bottom tasks strip when a goal is active:
/// `Execute · 2/5 agents · 13m26s`.
pub fn workflow_one_line_summary(snap: &WorkflowSnapshot) -> String {
    let phase_name = snap
        .phases
        .iter()
        .find(|p| p.id == snap.active_phase_id)
        .map(|p| p.name)
        .unwrap_or("Workflow");
    let elapsed = format_elapsed_ms(snap.elapsed_ms);
    if snap.total_agents == 0 {
        format!("{phase_name} · {elapsed}")
    } else {
        format!(
            "{phase_name} · {}/{} agents · {elapsed}",
            snap.completed_agents, snap.total_agents
        )
    }
}

fn format_elapsed_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Agent rows the panel's agents column currently shows for `snap`.
///
/// When following the pipeline (`view_phase_id` is `None`), filters to the
/// active phase and falls back to all agents only if that phase is empty.
/// When browsing an explicit phase, returns exactly that phase's agents
/// (empty when the phase has none — no silent mix-in of other phases).
///
/// Shared by render and j/k selection so the selection can never point at
/// a row the panel is not drawing.
pub fn focused_agent_rows(snap: &WorkflowSnapshot) -> Vec<&WorkflowAgentRow> {
    let focus_phase_id = snap.view_phase_id.unwrap_or(snap.active_phase_id);
    let agent_rows_for_active: Vec<&WorkflowAgentRow> = snap
        .agents
        .iter()
        .filter(|a| a.phase_id == focus_phase_id)
        .collect();
    // Fall back to all agents only when FOLLOWING the pipeline and the
    // active phase has none yet (e.g. a completed goal whose phase
    // bookkeeping drained). Browsing to an explicitly chosen phase always
    // shows exactly that phase's agents — an empty phase renders empty
    // rather than silently mixing other phases' rows under its header.
    if agent_rows_for_active.is_empty() {
        if snap.view_phase_id.is_none() {
            snap.agents.iter().collect()
        } else {
            Vec::new()
        }
    } else {
        agent_rows_for_active
    }
}

/// Render the two-column workflow panel into `area`.
///
/// `selected` is the currently selected agent row id (`child_session_id`).
/// Returns the number of rows written (for height budgeting).
pub fn render_workflow_panel(
    buf: &mut Buffer,
    area: Rect,
    snap: &WorkflowSnapshot,
    theme: &Theme,
    selected: Option<&str>,
) -> u16 {
    if area.width < 24 || area.height == 0 {
        return 0;
    }

    // Left column ~28% for phases, rest for agents (Claude layout).
    let left_w = (area.width as f32 * 0.30).round() as u16;
    let left_w = left_w.clamp(14, 28).min(area.width.saturating_sub(10));
    let right_x = area.x + left_w + 1;
    let right_w = area.width.saturating_sub(left_w + 1);

    let mut rows_used = 0u16;
    // One shared row set drives the header, body, selection, and height.
    let agent_rows = focused_agent_rows(snap);

    // Header row: "Phases" | "<ViewedPhase> · N agents"
    if rows_used < area.height {
        let focus_phase_id = snap.view_phase_id.unwrap_or(snap.active_phase_id);
        let active_name = snap
            .phases
            .iter()
            .find(|p| p.id == focus_phase_id)
            .map(|p| p.name)
            .unwrap_or("Workflow");
        let n_in_phase = snap
            .agents
            .iter()
            .filter(|a| a.phase_id == focus_phase_id)
            .count();
        let following_all_agents =
            snap.view_phase_id.is_none() && n_in_phase == 0 && !agent_rows.is_empty();
        let left = Line::from(Span::styled(
            " Phases",
            Style::default()
                .fg(theme.gray_bright)
                .add_modifier(Modifier::BOLD),
        ));
        let viewing = snap
            .view_phase_id
            .is_some_and(|vp| vp != snap.active_phase_id);
        let viewing_suffix = if viewing { " (viewing)" } else { "" };
        let right_label = if following_all_agents {
            format!(
                " All agents · {} agent{}",
                agent_rows.len(),
                if agent_rows.len() == 1 { "" } else { "s" },
            )
        } else if n_in_phase == 0 {
            format!(" {active_name}{viewing_suffix}")
        } else {
            format!(
                " {active_name} · {n_in_phase} agent{}{viewing_suffix}",
                if n_in_phase == 1 { "" } else { "s" },
            )
        };
        let right = Line::from(Span::styled(
            right_label,
            Style::default()
                .fg(theme.gray_bright)
                .add_modifier(Modifier::BOLD),
        ));
        buf.set_line_safe(area.x, area.y + rows_used, &left, left_w);
        if right_w > 0 {
            buf.set_line_safe(right_x, area.y + rows_used, &right, right_w);
        }
        // Dim vertical separator
        if left_w < area.width {
            buf.set_span_safe(
                area.x + left_w,
                area.y + rows_used,
                &Span::styled("│", Style::default().fg(theme.gray_dim)),
                1,
            );
        }
        rows_used += 1;
    }

    let phase_rows = snap.phases.len() as u16;
    // The agents column shows the VIEWED phase when the user browsed away
    // from the pipeline's active phase (Left/Right).
    let body_rows = phase_rows
        .max(agent_rows.len() as u16)
        .min(area.height.saturating_sub(rows_used));

    for row in 0..body_rows {
        let y = area.y + rows_used + row;

        // Left: phase row
        if (row as usize) < snap.phases.len() {
            let p = &snap.phases[row as usize];
            let icon = phase_icon(p.status, p.id == snap.active_phase_id);
            let is_active = p.id == snap.active_phase_id;
            let name_style = if is_active {
                Style::default()
                    .fg(theme.accent_plan)
                    .add_modifier(Modifier::BOLD)
            } else if matches!(p.status, WorkflowPhaseStatus::Complete) {
                Style::default().fg(theme.accent_success)
            } else if matches!(p.status, WorkflowPhaseStatus::Failed) {
                Style::default().fg(theme.accent_error)
            } else {
                Style::default().fg(theme.gray)
            };
            let icon_style = match p.status {
                WorkflowPhaseStatus::Complete => Style::default().fg(theme.accent_success),
                WorkflowPhaseStatus::Failed => Style::default().fg(theme.accent_error),
                WorkflowPhaseStatus::Active => Style::default().fg(theme.accent_plan),
                WorkflowPhaseStatus::Pending => Style::default().fg(theme.gray_dim),
            };
            let progress = format!("{}/{}", p.agents_done, p.agents_total);
            let line = Line::from(vec![
                Span::styled(format!(" {icon} "), icon_style),
                Span::styled(format!("{:<8}", p.name), name_style),
                Span::styled(progress, Style::default().fg(theme.gray)),
            ]);
            buf.set_line_safe(area.x, y, &line, left_w);
        }

        // Separator
        if left_w < area.width {
            buf.set_span_safe(
                area.x + left_w,
                y,
                &Span::styled("│", Style::default().fg(theme.gray_dim)),
                1,
            );
        }

        // Right: agent row
        if (row as usize) < agent_rows.len() && right_w > 0 {
            let a = agent_rows[row as usize];
            let icon = agent_icon(a.status);
            let icon_style = match a.status {
                WorkflowAgentStatus::Complete => Style::default().fg(theme.accent_success),
                WorkflowAgentStatus::Failed => Style::default().fg(theme.accent_error),
                WorkflowAgentStatus::Running => Style::default().fg(theme.accent_running),
                WorkflowAgentStatus::Cancelled => Style::default().fg(theme.gray),
            };
            let mut label = a.label.clone();
            if a.retry > 0 {
                label = format!("{label} (retry {})", a.retry);
            }
            // Live activity inline for running agents (Thinking / tool
            // title from `SubagentProgress` ticks).
            if let Some(act) = a.activity.as_deref()
                && !act.is_empty()
            {
                label = format!("{label} · {act}");
            }
            let is_selected = selected.is_some_and(|sel| sel == a.id);
            // Budget label so model/tokens/duration still fit.
            let meta_parts: Vec<String> = [
                a.model.clone(),
                (a.tokens > 0).then(|| format!("{} tok", format_tokens_compact(a.tokens as i64))),
                Some(format_duration(a.duration)),
            ]
            .into_iter()
            .flatten()
            .collect();
            let meta = meta_parts.join(" · ");
            let meta_w = unicode_width::UnicodeWidthStr::width(meta.as_str()) as u16;
            let label_budget = right_w
                .saturating_sub(3) // icon + spaces
                .saturating_sub(meta_w.saturating_add(2))
                .max(8) as usize;
            let label_disp = truncate_width(&label, label_budget);
            let pad = right_w
                .saturating_sub(3)
                .saturating_sub(unicode_width::UnicodeWidthStr::width(label_disp.as_str()) as u16)
                .saturating_sub(meta_w);
            let line = Line::from(vec![
                Span::styled(format!(" {icon} "), icon_style),
                Span::styled(
                    label_disp,
                    if is_selected {
                        Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default().fg(theme.text_primary)
                    },
                ),
                Span::raw(" ".repeat(pad as usize)),
                Span::styled(meta, Style::default().fg(theme.gray)),
            ]);
            buf.set_line_safe(right_x, y, &line, right_w);
        }
    }

    rows_used + body_rows
}

/// Preferred height for the workflow panel given a snapshot.
pub fn workflow_panel_height(snap: &WorkflowSnapshot, max: u16) -> u16 {
    // Budget for the same rows the render will draw (view phase + empty
    // active-phase fallback when following the pipeline).
    let agents_shown = focused_agent_rows(snap).len();
    let body = (snap.phases.len()).max(agents_shown) as u16;
    // header + body, at least 4 (header + 3 phases)
    (1 + body.max(3)).min(max).max(4)
}

fn truncate_width(s: &str, budget: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if UnicodeWidthStr::width(s) <= budget {
        return s.to_owned();
    }
    if budget <= 1 {
        return "…".to_owned();
    }
    let target = budget.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > target {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent::{GoalDisplayPhase, GoalDisplayState, GoalDisplayStatus};
    use std::sync::Arc;
    use std::time::Instant;

    fn stub_goal(phase: GoalDisplayPhase, planning: bool, verifying: bool) -> GoalDisplayState {
        let mut g = GoalDisplayState::test_stub();
        g.phase = phase;
        g.planning = planning;
        g.verifying_completion = verifying;
        g.status = GoalDisplayStatus::Active;
        g.objective = "Integrate the 7 open gaps".into();
        g
    }

    fn stub_subagent(id: &str, role: &str, typ: &str, finished: bool) -> SubagentInfo {
        SubagentInfo {
            subagent_id: Arc::from(id),
            child_session_id: Arc::from(format!("child-{id}")),
            description: Arc::from(format!("{role} work")),
            subagent_type: Arc::from(typ),
            persona: None,
            role: Some(Arc::from(role)),
            model: Some(Arc::from("deepseek-v4-pro")),
            context_source: None,
            resumed_from: None,
            capability_mode: None,
            context_normalized: false,
            parent_prompt_id: None,
            started_at: Instant::now(),
            last_progress_at: Instant::now(),
            finished,
            status: finished.then_some(Arc::from("completed")),
            error: None,
            duration_ms: finished.then_some(12_000),
            tool_calls: Some(3),
            turns: Some(2),
            turn_count: Some(2),
            tool_call_count: Some(3),
            tokens_used: Some(12_500),
            context_window_tokens: None,
            context_usage_pct: None,
            tools_used: Vec::new(),
            error_count: None,
            activity_label: None,
            is_background: true,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            prompt: None,
            child_cwd: None,
            worktree_path: None,
            goal_phase: None,
            goal_attempt: None,
            child_updates_replayed: false,
        }
    }

    #[test]
    fn classify_plan_execute_verify() {
        let planner = stub_subagent("1", "planner", "plan", true);
        assert_eq!(classify_subagent_phase(&planner), "plan");
        let worker = stub_subagent("2", "worker", "general-purpose", false);
        assert_eq!(classify_subagent_phase(&worker), "execute");
        let skeptic = stub_subagent("3", "skeptic-0", "attacker-code", false);
        assert_eq!(classify_subagent_phase(&skeptic), "verify");
    }

    #[test]
    fn derive_marks_planning_active() {
        let goal = stub_goal(GoalDisplayPhase::Planning, true, false);
        let mut map = HashMap::new();
        map.insert("1".into(), stub_subagent("1", "planner", "plan", false));
        let snap = derive_workflow_snapshot(&goal, &map);
        assert_eq!(snap.active_phase_id, "plan");
        assert!(
            snap.phases
                .iter()
                .any(|p| p.id == "plan" && p.status == WorkflowPhaseStatus::Active)
        );
    }

    #[test]
    fn derive_marks_verify_when_verifying_flag_set() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, true);
        let snap = derive_workflow_snapshot(&goal, &HashMap::new());
        assert_eq!(snap.active_phase_id, "verify");
    }

    /// Structured wire metadata is authoritative: a verifier whose prose
    /// carries NO keyword hints ("quality-gate-1") still lands in Verify,
    /// and a retry whose description lacks "retry N" still shows the
    /// structured attempt.
    #[test]
    fn structured_phase_wins_over_sniffed_keywords() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        let mut verifier = stub_subagent("v1", "quality-gate-1", "general-purpose", true);
        verifier.goal_phase = Some(Arc::from("verify"));
        verifier.goal_attempt = Some(2);
        let mut worker = stub_subagent("w1", "stage-2", "general-purpose", false);
        worker.goal_phase = Some(Arc::from("execute"));
        worker.goal_attempt = Some(1);
        map.insert("v1".into(), verifier);
        map.insert("w1".into(), worker);
        let snap = derive_workflow_snapshot(&goal, &map);
        // Row id is child_session_id (canonical for selection/drill-down).
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-v1")
                .map(|a| a.phase_id),
            Some("verify"),
            "verifier with keyword-free name must land in Verify via structured phase"
        );
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-w1")
                .map(|a| a.phase_id),
            Some("execute"),
            "worker with keyword-free name must land in Execute via structured phase"
        );
        // attempt 2 (1-based) => one retry shown.
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-v1")
                .map(|a| a.retry),
            Some(1),
            "structured attempt must drive the retry count without prose"
        );
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-w1")
                .map(|a| a.retry),
            Some(0)
        );
        // Phase totals group by the structured phase.
        let verify_phase = snap.phases.iter().find(|p| p.id == "verify").unwrap();
        assert_eq!((verify_phase.agents_done, verify_phase.agents_total), (1, 1));
    }

    /// Without structured metadata the sniffer remains as a fallback for
    /// non-goal sessions.
    #[test]
    fn sniffer_fallback_still_classifies_non_goal_subagents() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        map.insert(
            "a".into(),
            stub_subagent("a", "skeptic-0", "attacker-code", false),
        );
        map.insert("b".into(), stub_subagent("b", "planner", "plan", true));
        let snap = derive_workflow_snapshot(&goal, &map);
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-a")
                .map(|a| a.phase_id),
            Some("verify")
        );
        assert_eq!(
            snap.agents
                .iter()
                .find(|a| a.id == "child-b")
                .map(|a| a.phase_id),
            Some("plan")
        );
    }

    #[test]
    fn one_line_summary_includes_phase_and_counts() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        map.insert("1".into(), stub_subagent("1", "worker", "general-purpose", true));
        map.insert("2".into(), stub_subagent("2", "worker", "general-purpose", false));
        let snap = derive_workflow_snapshot(&goal, &map);
        let line = workflow_one_line_summary(&snap);
        assert!(line.contains("Execute"), "line={line}");
        assert!(line.contains("agents"), "line={line}");
    }

    #[test]
    fn render_workflow_panel_does_not_panic() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        map.insert("1".into(), stub_subagent("1", "worker", "general-purpose", false));
        let snap = derive_workflow_snapshot(&goal, &map);
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        let theme = Theme::current();
        let rows = render_workflow_panel(&mut buf, area, &snap, &theme, None);
        assert!(rows >= 4);
    }

    #[test]
    fn workflow_panel_height_at_least_four() {
        let goal = stub_goal(GoalDisplayPhase::Idle, false, false);
        let snap = derive_workflow_snapshot(&goal, &HashMap::new());
        assert!(workflow_panel_height(&snap, 20) >= 4);
    }

    /// The agents column and header follow the VIEWED phase when the user
    /// browsed away with Left/Right, while the ▶ active marker stays on
    /// the pipeline's real active phase.
    #[test]
    fn render_focuses_viewed_phase_and_keeps_active_marker() {
        let goal = stub_goal(GoalDisplayPhase::Planning, true, false);
        let mut map = HashMap::new();
        let mut planner = stub_subagent("p1", "planner", "plan", true);
        planner.goal_phase = Some(Arc::from("plan"));
        let mut skeptic1 = stub_subagent("s1", "quality-gate-1", "general-purpose", true);
        skeptic1.goal_phase = Some(Arc::from("verify"));
        let mut skeptic2 = stub_subagent("s2", "quality-gate-2", "general-purpose", true);
        skeptic2.goal_phase = Some(Arc::from("verify"));
        map.insert("p1".into(), planner);
        map.insert("s1".into(), skeptic1);
        map.insert("s2".into(), skeptic2);
        let mut snap = derive_workflow_snapshot(&goal, &map);
        // Browsing to verify: the column + header show the two skeptics,
        // but the ▶ stays on plan (the pipeline's active phase).
        snap.view_phase_id = Some("verify");
        let screen = Rect::new(0, 0, 100, 20);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let area = Rect::new(0, 0, 100, 12);
        render_workflow_panel(
            &mut buf,
            area,
            &snap,
            &Theme::current(),
            None,
        );
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y)) {
                    text.push_str(cell.symbol());
                }
            }
            text.push('\n');
        }
        assert!(
            text.contains("Verify · 2 agents (viewing)"),
            "header must name the viewed phase, got:\n{text}"
        );
        assert!(
            text.contains("quality-gate-1") && text.contains("quality-gate-2"),
            "viewed phase agents must render in the column, got:\n{text}"
        );
        // The ▶ marker must stay on the pipeline's active phase (plan),
        // not follow the view.
        let plan_line = text.lines().find(|l| l.contains("Plan")).unwrap_or("");
        assert!(
            plan_line.contains("▶"),
            "▶ must stay on the active phase (plan), got:\n{text}"
        );
        // A phase the user browsed to that has no agents renders EMPTY —
        // it must not silently fall back to showing other phases' agents.
        snap.view_phase_id = Some("execute");
        let mut buf2 = ratatui::buffer::Buffer::empty(screen);
        render_workflow_panel(
            &mut buf2,
            area,
            &snap,
            &Theme::current(),
            None,
        );
        let mut text2 = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf2.cell(ratatui::layout::Position::new(x, y)) {
                    text2.push_str(cell.symbol());
                }
            }
            text2.push('\n');
        }
        assert!(
            !text2.contains("quality-gate-1"),
            "browsing to an empty phase must not fall back to other phases, got:\n{text2}"
        );
    }

    /// Row id is the child session id so selection highlight matches the
    /// id stored in `workflow_selected` / used for `WorkflowDrillDown`
    /// when `subagent_id ≠ child_session_id`.
    #[test]
    fn row_id_is_child_session_id_not_subagent_id() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        let mut worker = stub_subagent("sa-distinct", "worker", "general-purpose", false);
        // stub already sets child_session_id = "child-sa-distinct"
        worker.goal_phase = Some(Arc::from("execute"));
        map.insert("child-sa-distinct".into(), worker);
        let snap = derive_workflow_snapshot(&goal, &map);
        assert_eq!(snap.agents.len(), 1);
        assert_eq!(
            snap.agents[0].id, "child-sa-distinct",
            "row id must be child_session_id for highlight/drill-down alignment"
        );
        assert_ne!(
            snap.agents[0].id, "sa-distinct",
            "row id must not be the shell subagent_id when they differ"
        );
    }

    fn buffer_has_reversed(buf: &Buffer, area: Rect) -> bool {
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y))
                    && cell.style().add_modifier.contains(Modifier::REVERSED)
                {
                    return true;
                }
            }
        }
        false
    }

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y)) {
                    text.push_str(cell.symbol());
                }
            }
            text.push('\n');
        }
        text
    }

    /// Rendering with `selected = child_session_id` reverse-styles that
    /// row's label; selecting the mismatched subagent_id does not.
    #[test]
    fn selected_highlight_matches_child_session_id() {
        let goal = stub_goal(GoalDisplayPhase::Executing, false, false);
        let mut map = HashMap::new();
        let mut worker = stub_subagent("sa-1", "worker", "general-purpose", false);
        worker.goal_phase = Some(Arc::from("execute"));
        worker.description = Arc::from("unique-label-xyz");
        map.insert("child-sa-1".into(), worker);
        let snap = derive_workflow_snapshot(&goal, &map);
        let area = Rect::new(0, 0, 100, 12);
        let theme = Theme::current();

        let mut buf_ok = Buffer::empty(area);
        render_workflow_panel(&mut buf_ok, area, &snap, &theme, Some("child-sa-1"));
        let text = buffer_text(&buf_ok, area);
        assert!(
            text.contains("unique-label-xyz"),
            "agent label must render, got:\n{text}"
        );
        assert!(
            buffer_has_reversed(&buf_ok, area),
            "selecting by child_session_id must reverse-style the row"
        );

        let mut buf_bad = Buffer::empty(area);
        render_workflow_panel(&mut buf_bad, area, &snap, &theme, Some("sa-1"));
        assert!(
            !buffer_has_reversed(&buf_bad, area),
            "selecting by subagent_id must not highlight when ids differ"
        );
    }

    /// When following the pipeline, focused rows exclude other phases'
    /// agents; browsing an empty phase yields no rows.
    #[test]
    fn focused_rows_scope_to_active_or_viewed_phase() {
        let goal = stub_goal(GoalDisplayPhase::Planning, true, false);
        let mut map = HashMap::new();
        let mut planner = stub_subagent("p1", "planner", "plan", false);
        planner.goal_phase = Some(Arc::from("plan"));
        let mut worker = stub_subagent("w1", "stage-2", "general-purpose", false);
        worker.goal_phase = Some(Arc::from("execute"));
        let mut skeptic = stub_subagent("s1", "quality-gate", "general-purpose", false);
        skeptic.goal_phase = Some(Arc::from("verify"));
        map.insert("child-p1".into(), planner);
        map.insert("child-w1".into(), worker);
        map.insert("child-s1".into(), skeptic);

        let mut snap = derive_workflow_snapshot(&goal, &map);
        assert_eq!(snap.active_phase_id, "plan");
        let following = focused_agent_rows(&snap);
        assert_eq!(following.len(), 1);
        assert_eq!(following[0].id, "child-p1");
        assert!(!following.iter().any(|a| a.id == "child-w1"));
        assert!(!following.iter().any(|a| a.id == "child-s1"));

        snap.view_phase_id = Some("execute");
        let exec_rows = focused_agent_rows(&snap);
        assert_eq!(exec_rows.len(), 1);
        assert_eq!(exec_rows[0].id, "child-w1");

        snap.agents.retain(|a| a.phase_id != "execute");
        snap.view_phase_id = Some("execute");
        assert!(
            focused_agent_rows(&snap).is_empty(),
            "browsed empty phase must yield no focused rows"
        );
    }

    /// When following an empty active phase, the column deliberately falls
    /// back to every workflow agent. Its header must describe those same rows
    /// instead of labelling a mixed list as the empty active phase.
    #[test]
    fn fallback_header_describes_all_rendered_agents() {
        let mut goal = stub_goal(GoalDisplayPhase::Idle, false, false);
        goal.status = GoalDisplayStatus::Complete;
        let mut map = HashMap::new();
        let mut planner = stub_subagent("p1", "planner", "plan", true);
        planner.goal_phase = Some(Arc::from("plan"));
        let mut worker = stub_subagent("w1", "worker", "general-purpose", true);
        worker.goal_phase = Some(Arc::from("execute"));
        map.insert("child-p1".into(), planner);
        map.insert("child-w1".into(), worker);

        let snap = derive_workflow_snapshot(&goal, &map);
        assert_eq!(snap.active_phase_id, "verify");
        assert_eq!(focused_agent_rows(&snap).len(), 2, "fallback precondition");

        let area = Rect::new(0, 0, 100, 12);
        let mut buf = Buffer::empty(area);
        render_workflow_panel(&mut buf, area, &snap, &Theme::current(), None);
        let text = buffer_text(&buf, area);
        assert!(
            text.contains("All agents · 2 agents"),
            "fallback header must describe the mixed-phase rows, got:\n{text}"
        );
    }
}
