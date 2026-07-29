//! Graph analysis: god nodes, surprising connections, suggested questions.

use crate::schema::{
    Analysis, Confidence, GodNode, GraphJson, SuggestedQuestion, SurpriseEdge,
};
use std::collections::{HashMap, HashSet};

const BUILTIN_NOISE: &[&str] = &[
    "str", "int", "float", "bool", "bytes", "object", "Path", "Any", "Optional", "List",
    "Dict", "Set", "Tuple", "Union", "Callable", "String", "Self", "self", "true", "false",
    "None", "null", "undefined", "Error", "Result", "Option", "Vec", "Box", "Arc", "Rc",
    "HashMap", "HashSet", "ok", "err", "Some", "None",
];

/// Fill god nodes / surprises / questions on top of a clustered analysis.
pub fn analyze(graph: &GraphJson, mut analysis: Analysis) -> Analysis {
    let degrees = degree_map(graph);
    let label: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect();
    let node_by_id: HashMap<&str, &crate::schema::Node> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // God nodes
    let mut ranked: Vec<_> = degrees.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut gods = Vec::new();
    for (id, deg) in ranked {
        if *deg == 0 {
            continue;
        }
        let Some(n) = node_by_id.get(id.as_str()) else {
            continue;
        };
        if is_file_or_stub(n, *deg) || is_noise(&n.label) {
            continue;
        }
        gods.push(GodNode {
            id: id.clone(),
            label: n.label.clone(),
            degree: *deg,
            community: n.community,
        });
        if gods.len() >= 10 {
            break;
        }
    }
    analysis.god_nodes = gods;

    // Surprising: edges crossing communities
    let mut surprises = Vec::new();
    for e in &graph.links {
        let ca = node_by_id.get(e.source.as_str()).and_then(|n| n.community);
        let cb = node_by_id.get(e.target.as_str()).and_then(|n| n.community);
        if ca.is_none() || cb.is_none() || ca == cb {
            continue;
        }
        let sa = node_by_id
            .get(e.source.as_str())
            .map(|n| n.source_file.as_str())
            .unwrap_or("");
        let sb = node_by_id
            .get(e.target.as_str())
            .map(|n| n.source_file.as_str())
            .unwrap_or("");
        // same file less surprising
        if !sa.is_empty() && sa == sb {
            continue;
        }
        let sl = label.get(e.source.as_str()).copied().unwrap_or(&e.source);
        let tl = label.get(e.target.as_str()).copied().unwrap_or(&e.target);
        if is_noise(sl) || is_noise(tl) {
            continue;
        }
        surprises.push(SurpriseEdge {
            source: sl.to_string(),
            target: tl.to_string(),
            relation: e.relation.clone(),
            confidence: e.confidence.as_str().to_string(),
            source_files: [sa.to_string(), sb.to_string()],
            note: None,
        });
        if surprises.len() >= 25 {
            break;
        }
    }
    analysis.surprises = surprises;

    // Suggested questions from god nodes + communities
    let mut questions = Vec::new();
    if let Some(g) = analysis.god_nodes.first() {
        questions.push(SuggestedQuestion {
            question: format!("What depends on `{}`, and what does it depend on?", g.label),
            why: "Highest-degree abstraction — most of the graph flows through it.".into(),
        });
    }
    if analysis.god_nodes.len() >= 2 {
        let a = &analysis.god_nodes[0];
        let b = &analysis.god_nodes[1];
        questions.push(SuggestedQuestion {
            question: format!("How does `{}` connect to `{}`?", a.label, b.label),
            why: "Two core abstractions — the path reveals architectural coupling.".into(),
        });
    }
    if let Some((cid, members)) = analysis.communities.iter().max_by_key(|(_, m)| m.len()) {
        let lab = analysis
            .community_labels
            .get(cid)
            .cloned()
            .unwrap_or_else(|| format!("Community {cid}"));
        questions.push(SuggestedQuestion {
            question: format!("What is the responsibility of the `{lab}` subsystem?"),
            why: format!(
                "Largest community ({} nodes) — a natural subsystem boundary.",
                members.len()
            ),
        });
    }
    if !analysis.surprises.is_empty() {
        let s = &analysis.surprises[0];
        questions.push(SuggestedQuestion {
            question: format!(
                "Why does `{}` {} `{}` across community boundaries?",
                s.source, s.relation, s.target
            ),
            why: "Cross-community edge — often a hidden coupling or shared utility.".into(),
        });
    }
    if questions.len() < 4 {
        questions.push(SuggestedQuestion {
            question: "Which modules have the densest internal connections?".into(),
            why: "Community cohesion highlights tightly-coupled areas worth careful changes."
                .into(),
        });
    }
    analysis.suggested_questions = questions;
    analysis
}

fn degree_map(graph: &GraphJson) -> HashMap<String, usize> {
    let mut d: HashMap<String, usize> = graph.nodes.iter().map(|n| (n.id.clone(), 0)).collect();
    for e in &graph.links {
        *d.entry(e.source.clone()).or_default() += 1;
        *d.entry(e.target.clone()).or_default() += 1;
    }
    d
}

fn is_noise(label: &str) -> bool {
    let bare = label.trim_end_matches("()").trim_start_matches('.');
    BUILTIN_NOISE.iter().any(|n| bare.eq_ignore_ascii_case(n))
}

fn is_file_or_stub(n: &crate::schema::Node, deg: usize) -> bool {
    let label = &n.label;
    if label.starts_with('.') && label.ends_with("()") {
        return true;
    }
    if label.ends_with("()") && deg <= 1 {
        return true;
    }
    // file hub: label equals basename of source_file
    if !n.source_file.is_empty() {
        let base = std::path::Path::new(&n.source_file)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if label == base {
            return true;
        }
    }
    false
}

/// Find simple import cycles among file-level nodes.
pub fn find_import_cycles(graph: &GraphJson) -> Vec<Vec<String>> {
    let mut adj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in &graph.links {
        if e.relation == "imports" || e.relation == "imports_from" {
            adj.entry(e.source.as_str())
                .or_default()
                .insert(e.target.as_str());
        }
    }
    let mut cycles = Vec::new();
    let nodes: Vec<&str> = adj.keys().copied().collect();
    for &start in &nodes {
        if let Some(nbrs) = adj.get(start) {
            for &mid in nbrs {
                if adj.get(mid).is_some_and(|s| s.contains(start)) && start < mid {
                    let sl = graph
                        .nodes
                        .iter()
                        .find(|n| n.id == start)
                        .map(|n| n.label.clone())
                        .unwrap_or_else(|| start.to_string());
                    let ml = graph
                        .nodes
                        .iter()
                        .find(|n| n.id == mid)
                        .map(|n| n.label.clone())
                        .unwrap_or_else(|| mid.to_string());
                    cycles.push(vec![sl, ml, /* back */ start.to_string()]);
                }
            }
        }
    }
    cycles.truncate(20);
    cycles
}

pub fn confidence_breakdown(graph: &GraphJson) -> (u32, u32, u32) {
    let mut ext = 0u32;
    let mut inf = 0u32;
    let mut amb = 0u32;
    for e in &graph.links {
        match e.confidence {
            Confidence::Extracted => ext += 1,
            Confidence::Inferred => inf += 1,
            Confidence::Ambiguous => amb += 1,
        }
    }
    (ext, inf, amb)
}
