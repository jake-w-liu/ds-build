//! Query / path / explain against graph.json (Graphify-compatible).

use crate::schema::GraphJson;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

pub fn load_graph(path: &Path) -> anyhow::Result<GraphJson> {
    let text = std::fs::read_to_string(path)?;
    let mut v: serde_json::Value = serde_json::from_str(&text)?;
    // Accept edges or links
    if v.get("links").is_none()
        && let Some(edges) = v.get("edges").cloned()
    {
        let obj = v
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("graph.json root must be an object"))?;
        obj.insert("links".into(), edges);
    }
    Ok(serde_json::from_value(v)?)
}

/// BFS/DFS scoped subgraph text for a natural-language question.
pub fn query_graph(
    graph: &GraphJson,
    question: &str,
    mode: &str,
    depth: usize,
    token_budget: usize,
) -> String {
    let terms = query_terms(question);
    if terms.is_empty() {
        return "No matching nodes found.".into();
    }
    let seeds = find_seeds(graph, &terms);
    if seeds.is_empty() {
        return "No matching nodes found.".into();
    }
    let nodes = if mode.eq_ignore_ascii_case("dfs") {
        dfs_nodes(graph, &seeds, depth)
    } else {
        bfs_nodes(graph, &seeds, depth)
    };
    // Display only real directed edges from the stored graph (no invented reverse labels).
    let edges = real_edges_among(graph, &nodes);
    let start_labels: Vec<_> = seeds
        .iter()
        .filter_map(|id| {
            graph
                .nodes
                .iter()
                .find(|n| n.id == *id)
                .map(|n| n.label.clone())
        })
        .collect();
    let mut out = format!(
        "Traversal: {} depth={depth} | Start: {start_labels:?} | {} nodes found\n\n",
        mode.to_uppercase(),
        nodes.len()
    );
    out.push_str(&subgraph_to_text(
        graph,
        &nodes,
        &edges,
        token_budget,
        &seeds,
    ));
    out
}

/// Shortest path between two labels.
pub fn path_between(graph: &GraphJson, a: &str, b: &str) -> String {
    let sa = find_node(graph, a);
    let sb = find_node(graph, b);
    if sa.is_empty() {
        return format!("No node matching `{a}`.");
    }
    if sb.is_empty() {
        return format!("No node matching `{b}`.");
    }
    let src = &sa[0];
    let tgt = &sb[0];
    if src == tgt {
        return format!("`{a}` and `{b}` resolve to the same node.");
    }
    // Undirected BFS for connectivity; reconstruct labels from real directed edges.
    let adj = undirected_neighbors(graph);
    let mut prev: HashMap<String, Option<String>> = HashMap::new();
    let mut q = VecDeque::new();
    q.push_back(src.clone());
    prev.insert(src.clone(), None);
    while let Some(cur) = q.pop_front() {
        if &cur == tgt {
            break;
        }
        if let Some(nbrs) = adj.get(&cur) {
            for nbr in nbrs {
                if prev.contains_key(nbr) {
                    continue;
                }
                prev.insert(nbr.clone(), Some(cur.clone()));
                q.push_back(nbr.clone());
            }
        }
    }
    if !prev.contains_key(tgt) {
        return format!(
            "No path between `{}` and `{}`.",
            label_of(graph, src),
            label_of(graph, tgt)
        );
    }
    let mut nodes_path = vec![tgt.clone()];
    let mut cur = tgt.clone();
    while let Some(Some(p)) = prev.get(&cur).cloned() {
        nodes_path.push(p.clone());
        cur = p;
    }
    nodes_path.reverse();
    let hops = nodes_path.len().saturating_sub(1);
    let mut lines = vec![format!("Shortest path ({hops} hops):")];
    for w in nodes_path.windows(2) {
        let (s, t) = (&w[0], &w[1]);
        let rel = directed_relation(graph, s, t);
        lines.push(format!(
            "  {} --{}--> {}",
            label_of(graph, s),
            rel,
            label_of(graph, t)
        ));
    }
    lines.join("\n")
}

/// Explain a single concept node.
pub fn explain(graph: &GraphJson, name: &str) -> String {
    let matches = find_node(graph, name);
    if matches.is_empty() {
        return format!("No node matching `{name}`.");
    }
    let id = &matches[0];
    let Some(n) = graph.nodes.iter().find(|n| n.id == *id) else {
        return format!("No node matching `{name}`.");
    };
    let mut in_edges = Vec::new();
    let mut out_edges = Vec::new();
    let mut seen_out = HashSet::new();
    let mut seen_in = HashSet::new();
    for e in &graph.links {
        if e.source == *id {
            let key = (e.target.clone(), e.relation.clone());
            if seen_out.insert(key) {
                out_edges.push(e);
            }
        }
        if e.target == *id {
            let key = (e.source.clone(), e.relation.clone());
            if seen_in.insert(key) {
                in_edges.push(e);
            }
        }
    }
    let mut lines = vec![
        format!("Node: {}", n.label),
        format!(
            "  Source:    {} {}",
            n.source_file,
            n.source_location.as_deref().unwrap_or("")
        ),
        format!(
            "  Community: {}",
            n.community
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        format!("  Degree:    {}", in_edges.len() + out_edges.len()),
        String::new(),
        format!("Connections ({}):", in_edges.len() + out_edges.len()),
    ];
    for e in out_edges.iter().take(40) {
        lines.push(format!(
            "  --> {} [{}] [{}]",
            label_of(graph, &e.target),
            e.relation,
            e.confidence.as_str()
        ));
    }
    for e in in_edges.iter().take(40) {
        lines.push(format!(
            "  <-- {} [{}] [{}]",
            label_of(graph, &e.source),
            e.relation,
            e.confidence.as_str()
        ));
    }
    lines.join("\n")
}

fn query_terms(q: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "what", "how", "where", "when", "who", "why", "which", "the", "a", "an", "is", "are",
        "does", "do", "of", "to", "in", "on", "for", "and", "or", "with", "from", "this",
        "that", "it", "be", "by", "show", "me", "about", "connects", "connected", "between",
    ];
    q.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        .filter(|t| t.len() > 1)
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| !STOP.contains(&t.as_str()))
        .collect()
}

fn find_seeds(graph: &GraphJson, terms: &[String]) -> Vec<String> {
    let mut scored: Vec<(i32, String)> = Vec::new();
    for n in &graph.nodes {
        let label = n.label.to_ascii_lowercase();
        let bare = label.trim_end_matches("()").trim_start_matches('.');
        let id = n.id.to_ascii_lowercase();
        let mut score = 0i32;
        for t in terms {
            if bare == t || label == *t || id == *t {
                score += 100;
            } else if bare.starts_with(t.as_str()) || label.contains(t.as_str()) {
                score += 40;
            } else if id.contains(t.as_str()) {
                score += 20;
            }
        }
        if score > 0 {
            scored.push((score, n.id.clone()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(5).map(|(_, id)| id).collect()
}

fn find_node(graph: &GraphJson, label: &str) -> Vec<String> {
    let term = label.to_ascii_lowercase();
    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut sub = Vec::new();
    for n in &graph.nodes {
        let l = n.label.to_ascii_lowercase();
        let bare = l.trim_end_matches("()").trim_start_matches('.').to_string();
        let id = n.id.to_ascii_lowercase();
        if bare == term || l == term || id == term {
            exact.push(n.id.clone());
        } else if bare.starts_with(&term) || l.starts_with(&term) {
            prefix.push(n.id.clone());
        } else if bare.contains(&term) || l.contains(&term) || id.contains(&term) {
            sub.push(n.id.clone());
        }
    }
    exact.extend(prefix);
    exact.extend(sub);
    exact
}

fn label_of(graph: &GraphJson, id: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.label.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Undirected adjacency for traversal only (no relation labels).
fn undirected_neighbors(graph: &GraphJson) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for e in &graph.links {
        adj.entry(e.source.clone())
            .or_default()
            .push(e.target.clone());
        adj.entry(e.target.clone())
            .or_default()
            .push(e.source.clone());
    }
    for v in adj.values_mut() {
        v.sort();
        v.dedup();
    }
    adj
}

/// Prefer forward relation a→b; else reverse with "(rev)"; else "related".
fn directed_relation(graph: &GraphJson, a: &str, b: &str) -> String {
    for e in &graph.links {
        if e.source == a && e.target == b {
            return e.relation.clone();
        }
    }
    for e in &graph.links {
        if e.source == b && e.target == a {
            return format!("{} (rev)", e.relation);
        }
    }
    "related".into()
}

fn real_edges_among(
    graph: &GraphJson,
    nodes: &HashSet<String>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for e in &graph.links {
        if nodes.contains(&e.source) && nodes.contains(&e.target) {
            let key = (
                e.source.clone(),
                e.target.clone(),
                e.relation.clone(),
                e.confidence.as_str(),
            );
            if seen.insert(key) {
                out.push((e.source.clone(), e.relation.clone(), e.target.clone()));
            }
        }
    }
    out
}

fn bfs_nodes(graph: &GraphJson, seeds: &[String], depth: usize) -> HashSet<String> {
    let adj = undirected_neighbors(graph);
    let mut seen = HashSet::new();
    let mut q = VecDeque::new();
    for s in seeds {
        q.push_back((s.clone(), 0usize));
        seen.insert(s.clone());
    }
    while let Some((cur, d)) = q.pop_front() {
        if d >= depth {
            continue;
        }
        if let Some(nbrs) = adj.get(&cur) {
            for nbr in nbrs {
                if seen.insert(nbr.clone()) {
                    q.push_back((nbr.clone(), d + 1));
                }
            }
        }
    }
    seen
}

fn dfs_nodes(graph: &GraphJson, seeds: &[String], depth: usize) -> HashSet<String> {
    let adj = undirected_neighbors(graph);
    let mut seen = HashSet::new();
    fn rec(
        cur: &str,
        d: usize,
        depth: usize,
        adj: &HashMap<String, Vec<String>>,
        seen: &mut HashSet<String>,
    ) {
        if d >= depth {
            return;
        }
        if let Some(nbrs) = adj.get(cur) {
            for nbr in nbrs {
                if seen.insert(nbr.clone()) {
                    rec(nbr, d + 1, depth, adj, seen);
                }
            }
        }
    }
    for s in seeds {
        seen.insert(s.clone());
        rec(s, 0, depth, &adj, &mut seen);
    }
    seen
}

fn subgraph_to_text(
    graph: &GraphJson,
    nodes: &HashSet<String>,
    edges: &[(String, String, String)],
    token_budget: usize,
    seeds: &[String],
) -> String {
    let mut lines = Vec::new();
    let mut ordered: Vec<&String> = seeds
        .iter()
        .filter(|s| nodes.contains(s.as_str()))
        .collect();
    for n in nodes {
        if !seeds.contains(n) {
            ordered.push(n);
        }
    }
    for id in ordered {
        let n = match graph.nodes.iter().find(|n| n.id == *id) {
            Some(n) => n,
            None => continue,
        };
        lines.push(format!(
            "NODE {} | {} | {} {}",
            n.label,
            n.file_type.as_str(),
            n.source_file,
            n.source_location.as_deref().unwrap_or("")
        ));
    }
    for (s, rel, t) in edges.iter().take(200) {
        lines.push(format!(
            "EDGE {} --{}--> {}",
            label_of(graph, s),
            rel,
            label_of(graph, t)
        ));
    }
    let mut out = String::new();
    let mut words = 0usize;
    for line in lines {
        let w = line.split_whitespace().count();
        if words + w > token_budget {
            out.push_str("…(truncated)\n");
            break;
        }
        out.push_str(&line);
        out.push('\n');
        words += w;
    }
    out
}
