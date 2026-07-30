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
    let node_comm: HashMap<String, u32> = partition
        .iter()
        .map(|(idx, cid)| (idx_to_id[idx].clone(), *cid))
        .collect();

    // Count unique internal node pairs in one edge pass. Scanning every edge
    // separately for every community makes this phase O(communities × edges).
    let mut internal_pairs = std::collections::HashSet::new();
    for edge in &graph.links {
        let Some(&source_community) = node_comm.get(&edge.source) else {
            continue;
        };
        if node_comm.get(&edge.target) != Some(&source_community) || edge.source == edge.target {
            continue;
        }
        let pair = if edge.source <= edge.target {
            (source_community, edge.source.as_str(), edge.target.as_str())
        } else {
            (source_community, edge.target.as_str(), edge.source.as_str())
        };
        internal_pairs.insert(pair);
    }
    let mut internal_counts: HashMap<u32, usize> = HashMap::new();
    for (community, _, _) in internal_pairs {
        *internal_counts.entry(community).or_default() += 1;
    }

    for (cid, members) in &communities {
        let hub = members
            .iter()
            .max_by(|a, b| degrees.get(*a).cmp(&degrees.get(*b)).then_with(|| a.cmp(b)))
            .cloned();
        let label = hub
            .as_ref()
            .and_then(|h| label_by_id.get(h))
            .cloned()
            .unwrap_or_else(|| format!("Community {cid}"));
        community_labels.insert(*cid, label);

        // Cohesion: unique internal node pairs / possible pairs.
        let n = members.len().max(1);
        let possible = n.saturating_mul(n.saturating_sub(1)) / 2;
        let score = if possible == 0 {
            0.0
        } else {
            internal_counts.get(cid).copied().unwrap_or(0) as f64 / possible as f64
        };
        cohesion.insert(*cid, score);
    }

    // Write community onto nodes
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

    let mut sigma_tot: HashMap<u32, f64> = HashMap::new();
    for (&idx, &community) in &comm {
        *sigma_tot.entry(community).or_default() += degrees[&idx];
    }

    let mut improved = true;
    while improved {
        improved = false;
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

            // Evaluate insertion after first removing the node from its
            // current community. Including the node in `sigma_tot[cur]`
            // overstates the benefit of moving and can make partitions
            // oscillate while decreasing actual modularity.
            *sigma_tot.entry(cur).or_default() -= k_i;
            let mut best_c = cur;
            let current_in = neigh_w.get(&cur).copied().unwrap_or(0.0);
            let mut best_gain = current_in
                - resolution * sigma_tot.get(&cur).copied().unwrap_or(0.0) * k_i / (2.0 * m);
            for (&c, &k_i_in) in &neigh_w {
                let sigma = sigma_tot.get(&c).copied().unwrap_or(0.0);
                // Scaled modularity insertion gain. The omitted positive
                // 1/m factor does not affect which community is best.
                let gain = k_i_in - resolution * sigma * k_i / (2.0 * m);
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best_c = c;
                }
            }
            *sigma_tot.entry(best_c).or_default() += k_i;
            if best_c != cur {
                comm.insert(idx, best_c);
                improved = true;
            }
        }
    }

    // Renumber communities to 0..k-1 by their lexicographically smallest
    // member. Iterating HashMap values here would randomize persisted IDs.
    let mut smallest_member: HashMap<u32, String> = HashMap::new();
    for (&idx, &community) in &comm {
        smallest_member
            .entry(community)
            .and_modify(|smallest| {
                if g[idx] < *smallest {
                    *smallest = g[idx].clone();
                }
            })
            .or_insert_with(|| g[idx].clone());
    }
    let mut ordered: Vec<(u32, String)> = smallest_member.into_iter().collect();
    ordered.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let remap: HashMap<u32, u32> = ordered
        .into_iter()
        .enumerate()
        .map(|(new, (old, _))| (old, new as u32))
        .collect();
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
        for (s, t) in [
            ("a1", "a2"),
            ("a2", "a3"),
            ("a1", "a3"),
            ("b1", "b2"),
            ("b2", "b3"),
            ("b1", "b3"),
        ] {
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

    #[test]
    fn community_ids_are_stable_and_lexically_ordered() {
        for _ in 0..100 {
            let mut g = GraphJson {
                directed: false,
                multigraph: false,
                graph: BTreeMap::new(),
                nodes: ["a", "b", "c", "d"]
                    .into_iter()
                    .map(|id| Node {
                        id: id.into(),
                        label: id.into(),
                        file_type: FileType::Code,
                        source_file: "t".into(),
                        source_location: None,
                        community: None,
                        origin_file: None,
                    })
                    .collect(),
                links: [("a", "b"), ("c", "d")]
                    .into_iter()
                    .map(|(source, target)| Edge {
                        source: source.into(),
                        target: target.into(),
                        relation: "uses".into(),
                        confidence: Confidence::Extracted,
                        source_file: "t".into(),
                        source_location: None,
                        weight: Some(1.0),
                        context: None,
                    })
                    .collect(),
            };
            cluster(&mut g, 1.0);
            let communities: BTreeMap<_, _> = g
                .nodes
                .iter()
                .map(|n| (n.id.as_str(), n.community.unwrap()))
                .collect();
            assert_eq!(communities["a"], 0);
            assert_eq!(communities["b"], 0);
            assert_eq!(communities["c"], 1);
            assert_eq!(communities["d"], 1);
        }
    }

    #[test]
    fn cohesion_is_bounded_for_multiple_relations_on_one_pair() {
        let mut g = GraphJson {
            directed: false,
            multigraph: false,
            graph: BTreeMap::new(),
            nodes: ["a", "b"]
                .into_iter()
                .map(|id| Node {
                    id: id.into(),
                    label: id.into(),
                    file_type: FileType::Code,
                    source_file: "t".into(),
                    source_location: None,
                    community: None,
                    origin_file: None,
                })
                .collect(),
            links: ["calls", "references"]
                .into_iter()
                .map(|relation| Edge {
                    source: "a".into(),
                    target: "b".into(),
                    relation: relation.into(),
                    confidence: Confidence::Extracted,
                    source_file: "t".into(),
                    source_location: None,
                    weight: Some(1.0),
                    context: None,
                })
                .collect(),
        };

        let analysis = cluster(&mut g, 1.0);
        assert!(
            analysis
                .cohesion
                .values()
                .all(|score| (0.0..=1.0).contains(score)),
            "cohesion must be a normalized score: {:?}",
            analysis.cohesion
        );
    }

    #[test]
    fn greedy_modularity_does_not_split_a_triangle() {
        let mut g = GraphJson {
            directed: false,
            multigraph: false,
            graph: BTreeMap::new(),
            nodes: ["a", "b", "c"]
                .into_iter()
                .map(|id| Node {
                    id: id.into(),
                    label: id.into(),
                    file_type: FileType::Code,
                    source_file: "t".into(),
                    source_location: None,
                    community: None,
                    origin_file: None,
                })
                .collect(),
            links: [("a", "b"), ("a", "c"), ("b", "c")]
                .into_iter()
                .map(|(source, target)| Edge {
                    source: source.into(),
                    target: target.into(),
                    relation: "uses".into(),
                    confidence: Confidence::Extracted,
                    source_file: "t".into(),
                    source_location: None,
                    weight: Some(1.0),
                    context: None,
                })
                .collect(),
        };

        let analysis = cluster(&mut g, 1.0);

        assert_eq!(
            analysis.communities.len(),
            1,
            "a clique has higher modularity as one community"
        );
    }
}
