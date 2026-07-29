//! Community detection — Louvain-style greedy modularity (Graphify fallback).
//! Deterministic: seeded iteration order by node id.

use crate::schema::{Analysis, GraphJson, Node};
use petgraph::graph::{NodeIndex, UnGraph};
use petgraph::visit::EdgeRef;
use std::collections::{BTreeMap, HashMap};

/// Cluster the graph, write `community` onto each node, fill Analysis community fields.
pub fn cluster(graph: &mut GraphJson, resolution: f64) -> Analysis {
    let mut g: UnGraph<String, f64> = UnGraph::new_undirected();
    let mut id_to_idx: HashMap<String, NodeIndex> = HashMap::new();
    let mut idx_to_id: HashMap<NodeIndex, String> = HashMap::new();

    // Stable order
    let mut node_ids: Vec<String> = graph.nodes.iter().map(|n| n.id.clone()).collect();
    node_ids.sort();
    for id in &node_ids {
        let idx = g.add_node(id.clone());
        id_to_idx.insert(id.clone(), idx);
        idx_to_id.insert(idx, id.clone());
    }
    for e in &graph.links {
        if let (Some(&a), Some(&b)) = (id_to_idx.get(&e.source), id_to_idx.get(&e.target)) {
            let w = e.weight.unwrap_or(1.0);
            if a != b {
                g.add_edge(a, b, w);
            }
        }
    }

    let partition = louvain(&g, resolution);

    // community id -> members
    let mut communities: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (idx, cid) in &partition {
        let id = idx_to_id[idx].clone();
        communities.entry(*cid).or_default().push(id);
    }
    for members in communities.values_mut() {
        members.sort();
    }

    // Label by hub (highest degree in full graph)
    let degrees: HashMap<String, usize> = {
        let mut d = HashMap::new();
        for n in &graph.nodes {
            d.insert(n.id.clone(), 0);
        }
        for e in &graph.links {
            *d.entry(e.source.clone()).or_default() += 1;
            *d.entry(e.target.clone()).or_default() += 1;
        }
        d
    };

    let mut community_labels: BTreeMap<u32, String> = BTreeMap::new();
    let mut cohesion: BTreeMap<u32, f64> = BTreeMap::new();
    let label_by_id: HashMap<String, String> = graph
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.label.clone()))
        .collect();

    for (cid, members) in &communities {
        let hub = members
            .iter()
            .max_by(|a, b| {
                degrees
                    .get(*a)
                    .cmp(&degrees.get(*b))
                    .then_with(|| a.cmp(b))
            })
            .cloned();
        let label = hub
            .as_ref()
            .and_then(|h| label_by_id.get(h))
            .cloned()
            .unwrap_or_else(|| format!("Community {cid}"));
        community_labels.insert(*cid, label);

        // Cohesion: internal edges / possible pairs
        let member_set: std::collections::HashSet<&str> =
            members.iter().map(|s| s.as_str()).collect();
        let mut internal = 0usize;
        for e in &graph.links {
            if member_set.contains(e.source.as_str()) && member_set.contains(e.target.as_str()) {
                internal += 1;
            }
        }
        let n = members.len().max(1);
        let possible = n * n.saturating_sub(1) / 2;
        let score = if possible == 0 {
            0.0
        } else {
            internal as f64 / possible as f64
        };
        cohesion.insert(*cid, score);
    }

    // Write community onto nodes
    let node_comm: HashMap<String, u32> = partition
        .iter()
        .map(|(idx, cid)| (idx_to_id[idx].clone(), *cid))
        .collect();
    for n in &mut graph.nodes {
        if let Some(&cid) = node_comm.get(&n.id) {
            n.community = Some(cid);
        }
    }

    Analysis {
        communities,
        community_labels,
        cohesion,
        ..Analysis::default()
    }
}

/// Simple Louvain: assign each node its own community, repeatedly move to neighbor
/// community that increases modularity until stable. Deterministic by sorted node ids.
fn louvain(g: &UnGraph<String, f64>, resolution: f64) -> HashMap<NodeIndex, u32> {
    let n = g.node_count();
    if n == 0 {
        return HashMap::new();
    }

    let mut comm: HashMap<NodeIndex, u32> = HashMap::new();
    for (i, idx) in g.node_indices().enumerate() {
        comm.insert(idx, i as u32);
    }

    let m: f64 = g.edge_weights().sum::<f64>().max(1.0);
    let mut degrees: HashMap<NodeIndex, f64> = HashMap::new();
    for idx in g.node_indices() {
        let d: f64 = g.edges(idx).map(|e| *e.weight()).sum();
        degrees.insert(idx, d);
    }

    let mut improved = true;
    let mut pass = 0;
    while improved && pass < 20 {
        improved = false;
        pass += 1;
        let mut nodes: Vec<NodeIndex> = g.node_indices().collect();
        nodes.sort_by_key(|i| g[*i].clone());

        for idx in nodes {
            let cur = comm[&idx];
            let k_i = degrees[&idx];
            // Neighbor community total edge weight
            let mut neigh_w: BTreeMap<u32, f64> = BTreeMap::new();
            for e in g.edges(idx) {
                let other = if e.source() == idx {
                    e.target()
                } else {
                    e.source()
                };
                let c = comm[&other];
                *neigh_w.entry(c).or_default() += *e.weight();
            }
            if neigh_w.is_empty() {
                continue;
            }

            // Total degree of each community
            let mut sigma_tot: HashMap<u32, f64> = HashMap::new();
            for (nidx, &c) in &comm {
                *sigma_tot.entry(c).or_default() += degrees[nidx];
            }

            let mut best_c = cur;
            let mut best_gain = 0.0;
            for (&c, &k_i_in) in &neigh_w {
                let sigma = sigma_tot.get(&c).copied().unwrap_or(0.0);
                // ΔQ ≈ [k_i_in / m - resolution * sigma_tot * k_i / (2m²)]
                let gain = k_i_in / m - resolution * (sigma * k_i) / (2.0 * m * m);
                let cur_in = neigh_w.get(&cur).copied().unwrap_or(0.0);
                let cur_sigma = sigma_tot.get(&cur).copied().unwrap_or(0.0);
                let cur_gain = cur_in / m - resolution * (cur_sigma * k_i) / (2.0 * m * m);
                let delta = gain - cur_gain;
                if delta > best_gain + 1e-12 || (delta > best_gain - 1e-12 && c < best_c) {
                    best_gain = delta;
                    best_c = c;
                }
            }
            if best_c != cur && best_gain > 1e-12 {
                comm.insert(idx, best_c);
                improved = true;
            }
        }
    }

    // Renumber communities to 0..k-1
    let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
    let mut next = 0u32;
    for &c in comm.values() {
        remap.entry(c).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
    }
    for c in comm.values_mut() {
        *c = remap[c];
    }
    comm
}

/// Apply community attrs from analysis onto a node list (utility).
pub fn stamp_communities(nodes: &mut [Node], analysis: &Analysis) {
    let mut map = HashMap::new();
    for (cid, members) in &analysis.communities {
        for m in members {
            map.insert(m.clone(), *cid);
        }
    }
    for n in nodes {
        if let Some(&cid) = map.get(&n.id) {
            n.community = Some(cid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Confidence, Edge, FileType, Node};

    #[test]
    fn clusters_two_cliques() {
        let mut nodes = Vec::new();
        let mut links = Vec::new();
        for id in ["a1", "a2", "a3", "b1", "b2", "b3"] {
            nodes.push(Node {
                id: id.into(),
                label: id.into(),
                file_type: FileType::Code,
                source_file: "t".into(),
                source_location: None,
                community: None,
                origin_file: None,
            });
        }
        for (s, t) in [("a1", "a2"), ("a2", "a3"), ("a1", "a3"), ("b1", "b2"), ("b2", "b3"), ("b1", "b3")] {
            links.push(Edge {
                source: s.into(),
                target: t.into(),
                relation: "uses".into(),
                confidence: Confidence::Extracted,
                source_file: "t".into(),
                source_location: None,
                weight: Some(1.0),
                context: None,
            });
        }
        let mut g = GraphJson {
            directed: false,
            multigraph: false,
            graph: BTreeMap::new(),
            nodes,
            links,
        };
        let analysis = cluster(&mut g, 1.0);
        assert!(analysis.communities.len() >= 2);
    }
}
