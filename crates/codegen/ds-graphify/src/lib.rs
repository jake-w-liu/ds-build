//! # ds-graphify
//!
//! Native, Graphify-compatible knowledge-graph pipeline for ds-build.
//!
//! Pipeline (matches Graphify-Labs/graphify):
//! ```text
//! detect → extract → build → cluster → analyze → report → export
//! ```
//!
//! Outputs under `graphify-out/`:
//! - `graph.html` — interactive force-directed graph
//! - `GRAPH_REPORT.md` — god nodes, communities, surprises, questions
//! - `graph.json` — NetworkX node-link graph (query anytime)
//!
//! CLI binary: `graphify` (`query` / `path` / `explain` / build / `update`).

pub mod analyze;
pub mod cluster;
pub mod detect;
pub mod export;
pub mod extract;
pub mod ids;
pub mod pipeline;
pub mod query;
pub mod report;
pub mod schema;

pub use pipeline::{PipelineOptions, PipelineResult, OUT_DIR_NAME, format_detection_summary, run};
pub use schema::{
    Analysis, Confidence, DetectionResult, Edge, Extraction, FileType, GraphJson, Node,
};
