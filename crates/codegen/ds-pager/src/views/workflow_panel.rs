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
//! Used by the goal-detail overlay (`/workflows` / `g` toggle) so multi-agent
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
    pub id: String,
    pub label: String,
    pub phase_id: &'static str,
    pub status: WorkflowAgentStatus,
    pub retry: u32,
    pub model: Option<String>,
    pub tokens: u64,
    pub duration: std::time::Duration,
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
                id: info.subagent_id.to_string(),
                label,
                phase_id: classify_subagent_phase(info),
                status: agent_status(info),
                retry: retry_count(info),
                model: info.model.as_ref().map(|m| m.to_string()),
                tokens: info.tokens_used.unwrap_or(0),
                duration: info.display_elapsed(),
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
        if this_ord < active_ord {
            WorkflowPhaseStatus::Complete
        } else if total > 0 && done >= total && total > 0 {
            WorkflowPhaseStatus::Complete
        } else if total > 0
            && agents
                .iter()
                .any(|a| a.phase_id == id && matches!(a.status, WorkflowAgentStatus::Failed))
            && done + 1 <= total
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

/// Render the two-column workflow panel into `area`.
///
/// Returns the number of rows written (for height budgeting).
pub fn render_workflow_panel(
    buf: &mut Buffer,
    area: Rect,
    snap: &WorkflowSnapshot,
    theme: &Theme,
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

    // Header row: "Phases" | "<ActivePhase> · N agents"
    if rows_used < area.height {
        let active_name = snap
            .phases
            .iter()
            .find(|p| p.id == snap.active_phase_id)
            .map(|p| p.name)
            .unwrap_or("Workflow");
        let n_in_phase = snap
            .agents
            .iter()
            .filter(|a| a.phase_id == snap.active_phase_id)
            .count();
        let left = Line::from(Span::styled(
            " Phases",
            Style::default()
                .fg(theme.gray_bright)
                .add_modifier(Modifier::BOLD),
        ));
        let right_label = if n_in_phase == 0 {
            format!(" {active_name}")
        } else {
            format!(
                " {active_name} · {n_in_phase} agent{}",
                if n_in_phase == 1 { "" } else { "s" }
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
    let agent_rows_for_active: Vec<&WorkflowAgentRow> = snap
        .agents
        .iter()
        .filter(|a| a.phase_id == snap.active_phase_id)
        .collect();
    // Fall back to all agents if the active phase has none yet.
    let agent_rows: Vec<&WorkflowAgentRow> = if agent_rows_for_active.is_empty() {
        snap.agents.iter().collect()
    } else {
        agent_rows_for_active
    };

    let body_rows = phase_rows.max(agent_rows.len() as u16).min(
        area.height.saturating_sub(rows_used),
    );

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
                Span::styled(label_disp, Style::default().fg(theme.text_primary)),
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
    let agents_in_active = snap
        .agents
        .iter()
        .filter(|a| a.phase_id == snap.active_phase_id)
        .count()
        .max(if snap.agents.is_empty() {
            0
        } else {
            // When active phase is empty, we fall back to all agents.
            snap.agents.len()
        });
    let body = (snap.phases.len()).max(agents_in_active) as u16;
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
        let rows = render_workflow_panel(&mut buf, area, &snap, &theme);
        assert!(rows >= 4);
    }

    #[test]
    fn workflow_panel_height_at_least_four() {
        let goal = stub_goal(GoalDisplayPhase::Idle, false, false);
        let snap = derive_workflow_snapshot(&goal, &HashMap::new());
        assert!(workflow_panel_height(&snap, 20) >= 4);
    }
}
