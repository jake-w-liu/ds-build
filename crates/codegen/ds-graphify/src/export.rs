//! Export graph.json + interactive graph.html.

use crate::schema::{Analysis, GraphJson};
use std::path::Path;

/// Write graph.json (NetworkX node-link format with `links`).
pub fn write_graph_json(graph: &GraphJson, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(graph)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Write a self-contained force-directed graph.html.
pub fn write_graph_html(
    graph: &GraphJson,
    analysis: &Analysis,
    path: &Path,
    title: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let nodes_json = serde_json::to_string(
        &graph
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "label": n.label,
                    "group": n.community.unwrap_or(0),
                    "title": format!("{} · {} · {}", n.label, n.source_file, n.source_location.as_deref().unwrap_or("")),
                    "file_type": n.file_type.as_str(),
                })
            })
            .collect::<Vec<_>>(),
    )?;
    let edges_json = serde_json::to_string(
        &graph
            .links
            .iter()
            .map(|e| {
                serde_json::json!({
                    "from": e.source,
                    "to": e.target,
                    "label": e.relation,
                    "title": format!("{} [{}]", e.relation, e.confidence.as_str()),
                    "dashes": e.confidence.as_str() != "EXTRACTED",
                })
            })
            .collect::<Vec<_>>(),
    )?;

    let legend: Vec<String> = analysis
        .community_labels
        .iter()
        .map(|(cid, lab)| format!("<li><span class=\"swatch g{cid}\"></span>{lab}</li>"))
        .collect();

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title} — graphify</title>
<script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
<style>
  html, body {{ margin:0; height:100%; font-family: system-ui, sans-serif; background:#0d1117; color:#e6edf3; }}
  #mynetwork {{ width:100%; height:100vh; }}
  #panel {{ position:fixed; top:12px; left:12px; background:rgba(22,27,34,.92); border:1px solid #30363d;
            border-radius:8px; padding:12px 14px; max-width:280px; max-height:70vh; overflow:auto; z-index:10; }}
  #panel h1 {{ font-size:14px; margin:0 0 8px; }}
  #panel input {{ width:100%; box-sizing:border-box; background:#0d1117; border:1px solid #30363d;
                 color:#e6edf3; border-radius:4px; padding:6px 8px; margin-bottom:8px; }}
  #panel ul {{ list-style:none; padding:0; margin:0; font-size:12px; }}
  #panel li {{ margin:3px 0; display:flex; align-items:center; gap:6px; }}
  .swatch {{ width:10px; height:10px; border-radius:2px; display:inline-block; }}
  .g0{{background:#58a6ff}}.g1{{background:#3fb950}}.g2{{background:#d29922}}.g3{{background:#f85149}}
  .g4{{background:#a371f7}}.g5{{background:#79c0ff}}.g6{{background:#ffa657}}.g7{{background:#ff7b72}}
  #meta {{ font-size:11px; color:#8b949e; margin-top:8px; }}
</style>
</head>
<body>
<div id="panel">
  <h1>{title}</h1>
  <input id="q" type="search" placeholder="Filter nodes…"/>
  <ul id="legend">{legend}</ul>
  <div id="meta">{nodes} nodes · {edges} edges · {comms} communities</div>
</div>
<div id="mynetwork"></div>
<script>
const nodes = new vis.DataSet({nodes_json});
const edges = new vis.DataSet({edges_json});
const container = document.getElementById('mynetwork');
const data = {{ nodes, edges }};
const options = {{
  nodes: {{ shape: 'dot', size: 10, font: {{ color: '#e6edf3', size: 12 }}, borderWidth: 1 }},
  edges: {{ arrows: {{ to: {{ enabled: true, scaleFactor: 0.4 }} }}, color: {{ color: '#484f58' }},
           font: {{ size: 9, color: '#8b949e', strokeWidth: 0 }}, smooth: {{ type: 'continuous' }} }},
  physics: {{ forceAtlas2Based: {{ gravitationalConstant: -40, springLength: 80 }},
             solver: 'forceAtlas2Based', stabilization: {{ iterations: 120 }} }},
  interaction: {{ hover: true, tooltipDelay: 80 }},
  groups: {{
    0: {{ color: '#58a6ff' }}, 1: {{ color: '#3fb950' }}, 2: {{ color: '#d29922' }},
    3: {{ color: '#f85149' }}, 4: {{ color: '#a371f7' }}, 5: {{ color: '#79c0ff' }},
    6: {{ color: '#ffa657' }}, 7: {{ color: '#ff7b72' }}
  }}
}};
const network = new vis.Network(container, data, options);
const all = nodes.get();
document.getElementById('q').addEventListener('input', (e) => {{
  const t = e.target.value.toLowerCase();
  if (!t) {{ nodes.update(all.map(n => ({{ id: n.id, hidden: false }}))); return; }}
  nodes.update(all.map(n => ({{
    id: n.id,
    hidden: !(n.label.toLowerCase().includes(t) || (n.title||'').toLowerCase().includes(t))
  }})));
}});
</script>
</body>
</html>
"##,
        title = html_escape(title),
        legend = legend.join("\n"),
        nodes = graph.nodes.len(),
        edges = graph.links.len(),
        comms = analysis.communities.len(),
        nodes_json = nodes_json,
        edges_json = edges_json,
    );

    std::fs::write(path, html)?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
