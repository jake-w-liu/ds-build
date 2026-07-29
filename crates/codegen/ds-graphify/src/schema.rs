//! Graphify extraction / graph schema.
//!
//! Compatible with Graphify-Labs/graphify JSON:
//! - nodes: `{id, label, file_type, source_file, source_location?, community?}`
//! - edges: `{source, target, relation, confidence, source_file, source_location?, weight?}`
//! - confidence: `EXTRACTED | INFERRED | AMBIGUOUS`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Confidence tag on every edge (Graphify honesty trail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Confidence {
    #[default]
    Extracted,
    Inferred,
    Ambiguous,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extracted => "EXTRACTED",
            Self::Inferred => "INFERRED",
            Self::Ambiguous => "AMBIGUOUS",
        }
    }
}

/// File / concept category on nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    #[default]
    Code,
    Document,
    Paper,
    Image,
    Rationale,
    Concept,
}

impl FileType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Document => "document",
            Self::Paper => "paper",
            Self::Image => "image",
            Self::Rationale => "rationale",
            Self::Concept => "concept",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub file_type: FileType,
    pub source_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: Confidence,
    pub source_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extraction {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl Extraction {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn merge(&mut self, other: Extraction) {
        self.nodes.extend(other.nodes);
        self.edges.extend(other.edges);
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        if self.error.is_none() {
            self.error = other.error;
        }
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.id.is_empty() {
                errors.push(format!("Node {i} missing id"));
            }
            if n.label.is_empty() {
                errors.push(format!("Node {i} (id={}) missing label", n.id));
            }
            ids.insert(n.id.as_str());
        }
        for (i, e) in self.edges.iter().enumerate() {
            if e.source.is_empty() || e.target.is_empty() {
                errors.push(format!("Edge {i} missing source/target"));
            }
            if !ids.is_empty() && !ids.contains(e.source.as_str()) {
                errors.push(format!(
                    "Edge {i} source '{}' does not match any node id",
                    e.source
                ));
            }
            if !ids.is_empty() && !ids.contains(e.target.as_str()) {
                errors.push(format!(
                    "Edge {i} target '{}' does not match any node id",
                    e.target
                ));
            }
        }
        errors
    }
}

/// node-link JSON graph (NetworkX-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphJson {
    pub directed: bool,
    pub multigraph: bool,
    pub graph: BTreeMap<String, serde_json::Value>,
    pub nodes: Vec<Node>,
    /// NetworkX uses `links`; Graphify also accepts `edges`.
    #[serde(alias = "edges")]
    pub links: Vec<Edge>,
}

impl GraphJson {
    pub fn from_extraction(extraction: &Extraction, directed: bool) -> Self {
        // Dedup nodes by id (last write wins, semantic-over-AST style).
        let mut by_id: BTreeMap<String, Node> = BTreeMap::new();
        for n in &extraction.nodes {
            by_id.insert(n.id.clone(), n.clone());
        }
        let nodes: Vec<Node> = by_id.into_values().collect();
        let id_set: std::collections::HashSet<String> =
            nodes.iter().map(|n| n.id.clone()).collect();
        // Dedup edges by (source, target, relation, confidence).
        let mut seen_edges = std::collections::HashSet::new();
        let mut links = Vec::new();
        for e in &extraction.edges {
            if !id_set.contains(&e.source) || !id_set.contains(&e.target) {
                continue;
            }
            let key = (
                e.source.clone(),
                e.target.clone(),
                e.relation.clone(),
                e.confidence.as_str(),
            );
            if seen_edges.insert(key) {
                links.push(e.clone());
            }
        }
        Self {
            directed,
            multigraph: false,
            graph: BTreeMap::new(),
            nodes,
            links,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionResult {
    pub scan_root: String,
    pub total_files: usize,
    pub total_words: u64,
    pub warning: Option<String>,
    pub files: DetectionFiles,
    #[serde(default)]
    pub skipped_sensitive: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionFiles {
    #[serde(default)]
    pub code: Vec<String>,
    #[serde(default)]
    pub document: Vec<String>,
    #[serde(default)]
    pub paper: Vec<String>,
    #[serde(default)]
    pub image: Vec<String>,
    #[serde(default)]
    pub video: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GodNode {
    pub id: String,
    pub label: String,
    pub degree: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurpriseEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: String,
    pub source_files: [String; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedQuestion {
    pub question: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Analysis {
    pub god_nodes: Vec<GodNode>,
    pub surprises: Vec<SurpriseEdge>,
    pub suggested_questions: Vec<SuggestedQuestion>,
    /// community_id -> member node ids
    pub communities: BTreeMap<u32, Vec<String>>,
    /// community_id -> hub label
    pub community_labels: BTreeMap<u32, String>,
    /// community_id -> cohesion score
    pub cohesion: BTreeMap<u32, f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCost {
    pub input: u64,
    pub output: u64,
}
