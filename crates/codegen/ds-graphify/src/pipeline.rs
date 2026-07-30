//! Full Graphify pipeline orchestration.

use crate::analyze;
use crate::cluster;
use crate::detect;
use crate::export;
use crate::extract;
use crate::report;
use crate::schema::{DetectionResult, Extraction, GraphJson, TokenCost};
use anyhow::Context;
use std::path::{Path, PathBuf};

pub const OUT_DIR_NAME: &str = "graphify-out";

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub root: PathBuf,
    pub out_dir: PathBuf,
    pub directed: bool,
    pub no_viz: bool,
    pub resolution: f64,
    pub cluster_only: bool,
    pub update: bool,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            out_dir: PathBuf::from(OUT_DIR_NAME),
            directed: false,
            no_viz: false,
            resolution: 1.0,
            cluster_only: false,
            update: false,
        }
    }
}

#[derive(Debug)]
pub struct PipelineResult {
    pub detection: DetectionResult,
    pub graph: GraphJson,
    pub report_path: PathBuf,
    pub graph_json_path: PathBuf,
    pub graph_html_path: Option<PathBuf>,
    pub nodes: usize,
    pub edges: usize,
    pub communities: usize,
}

/// Run the full detect → extract → build → cluster → analyze → report → export pipeline.
pub fn run(opts: &PipelineOptions) -> anyhow::Result<PipelineResult> {
    anyhow::ensure!(
        opts.resolution.is_finite() && opts.resolution > 0.0,
        "resolution must be a finite number greater than zero"
    );
    std::fs::create_dir_all(&opts.out_dir)?;

    if opts.cluster_only {
        return run_cluster_only(opts);
    }

    let detection_root = absolute_canonical(&opts.root)?;
    let output_root = absolute_canonical(&opts.out_dir)?;
    let exclusions = output_exclusions(&detection_root, &output_root);
    let detection = detect::detect_excluding(&detection_root, &exclusions)?;
    let det_path = opts.out_dir.join(".graphify_detect.json");

    let root = PathBuf::from(&detection.scan_root);
    let mut paths: Vec<PathBuf> = detect::all_code_paths(&detection);
    paths.extend(detect::all_doc_paths(&detection));

    if paths.is_empty() {
        anyhow::bail!("No supported files found in {}", opts.root.display());
    }

    // Incremental: if --update and graph exists, only re-extract changed files.
    // For v1 we still re-extract all (hash-based cache can land later); --update
    // keeps the same pipeline but is the documented incremental entrypoint.
    let _ = opts.update;

    let extraction = extract::extract_many(&paths, &root);
    if let Some(error) = &extraction.error {
        anyhow::bail!("structural extraction failed: {error}");
    }
    let ast_path = opts.out_dir.join(".graphify_ast.json");
    let ast_json = serde_json::to_string_pretty(&extraction)?;

    // Merge optional semantic extraction if present
    let mut merged = extraction;
    let sem_path = opts.out_dir.join(".graphify_semantic.json");
    if sem_path.is_file() {
        let text = std::fs::read_to_string(&sem_path)
            .with_context(|| format!("failed to read {}", sem_path.display()))?;
        let sem = serde_json::from_str::<Extraction>(&text)
            .with_context(|| format!("invalid {}", sem_path.display()))?;
        merged.merge(sem);
    } else {
        // Empty semantic file so tooling that expects it doesn't fail
        let empty = Extraction::empty();
        std::fs::write(&sem_path, serde_json::to_string_pretty(&empty)?)?;
    }
    let validation_errors = merged.validate();
    if !validation_errors.is_empty() {
        anyhow::bail!(
            "merged extraction is invalid:\n{}",
            validation_errors.join("\n")
        );
    }
    // Persist intermediate artifacts only after every input has parsed and
    // the merged graph validates, so a failed rebuild cannot make metadata
    // describe a newer corpus than the last successful graph.
    std::fs::write(&det_path, serde_json::to_string_pretty(&detection)?)?;
    std::fs::write(opts.out_dir.join(".graphify_root"), &detection.scan_root)?;
    std::fs::write(&ast_path, ast_json)?;

    let mut graph = GraphJson::from_extraction(&merged, opts.directed);
    let mut analysis = cluster::cluster(&mut graph, opts.resolution);
    analysis = analyze::analyze(&graph, analysis);

    let token_cost = TokenCost {
        input: merged.input_tokens,
        output: merged.output_tokens,
    };

    let root_label = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".")
        .to_string();
    let commit = git_head(&root);
    let report_md = report::render_report(
        &graph,
        &analysis,
        &detection,
        &token_cost,
        &root_label,
        commit.as_deref(),
    );

    let graph_json_path = opts.out_dir.join("graph.json");
    let report_path = opts.out_dir.join("GRAPH_REPORT.md");
    export::write_graph_json(&graph, &graph_json_path)?;
    std::fs::write(&report_path, report_md)?;
    std::fs::write(
        opts.out_dir.join(".graphify_analysis.json"),
        serde_json::to_string_pretty(&analysis)?,
    )?;

    let graph_html_path = if opts.no_viz {
        None
    } else {
        let p = opts.out_dir.join("graph.html");
        export::write_graph_html(&graph, &analysis, &p, &root_label)?;
        Some(p)
    };

    let communities = analysis.communities.len();
    Ok(PipelineResult {
        detection,
        nodes: graph.nodes.len(),
        edges: graph.links.len(),
        communities,
        graph,
        report_path,
        graph_json_path,
        graph_html_path,
    })
}

fn run_cluster_only(opts: &PipelineOptions) -> anyhow::Result<PipelineResult> {
    let graph_path = opts.out_dir.join("graph.json");
    if !graph_path.is_file() {
        anyhow::bail!(
            "cluster-only requires existing {} — run a full build first",
            graph_path.display()
        );
    }
    let mut graph = crate::query::load_graph(&graph_path)?;
    let mut analysis = cluster::cluster(&mut graph, opts.resolution);
    analysis = analyze::analyze(&graph, analysis);

    let detection = if opts.out_dir.join(".graphify_detect.json").is_file() {
        serde_json::from_str(&std::fs::read_to_string(
            opts.out_dir.join(".graphify_detect.json"),
        )?)?
    } else {
        DetectionResult::default()
    };

    let root_label = Path::new(&detection.scan_root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".")
        .to_string();
    let report_md = report::render_report(
        &graph,
        &analysis,
        &detection,
        &TokenCost::default(),
        &root_label,
        None,
    );
    let report_path = opts.out_dir.join("GRAPH_REPORT.md");
    export::write_graph_json(&graph, &graph_path)?;
    std::fs::write(&report_path, report_md)?;
    std::fs::write(
        opts.out_dir.join(".graphify_analysis.json"),
        serde_json::to_string_pretty(&analysis)?,
    )?;
    let graph_html_path = if opts.no_viz {
        None
    } else {
        let p = opts.out_dir.join("graph.html");
        export::write_graph_html(&graph, &analysis, &p, &root_label)?;
        Some(p)
    };
    Ok(PipelineResult {
        nodes: graph.nodes.len(),
        edges: graph.links.len(),
        communities: analysis.communities.len(),
        detection,
        graph,
        report_path,
        graph_json_path: graph_path,
        graph_html_path,
    })
}

fn git_head(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn absolute_canonical(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(dunce::canonicalize(&absolute).unwrap_or(absolute))
}

fn output_exclusions(root: &Path, out: &Path) -> Vec<PathBuf> {
    if !out.starts_with(root) {
        return Vec::new();
    }
    if out != root {
        return vec![out.to_path_buf()];
    }
    [
        ".graphify_detect.json",
        ".graphify_root",
        ".graphify_ast.json",
        ".graphify_semantic.json",
        ".graphify_analysis.json",
        "graph.json",
        "GRAPH_REPORT.md",
        "graph.html",
    ]
    .into_iter()
    .map(|name| out.join(name))
    .collect()
}

/// Summarize detection for CLI stdout.
pub fn format_detection_summary(det: &DetectionResult) -> String {
    let mut lines = vec![format!(
        "Corpus: {} files · ~{} words",
        det.total_files, det.total_words
    )];
    if !det.files.code.is_empty() {
        lines.push(format!("  code:     {} files", det.files.code.len()));
    }
    if !det.files.document.is_empty() {
        lines.push(format!("  docs:     {} files", det.files.document.len()));
    }
    if !det.files.paper.is_empty() {
        lines.push(format!("  papers:   {} files", det.files.paper.len()));
    }
    if !det.files.image.is_empty() {
        lines.push(format!("  images:   {} files", det.files.image.len()));
    }
    if !det.files.video.is_empty() {
        lines.push(format!("  video:    {} files", det.files.video.len()));
    }
    if let Some(w) = &det.warning {
        lines.push(format!("warning: {w}"));
    }
    if !det.skipped_sensitive.is_empty() {
        lines.push(format!(
            "skipped sensitive: {}",
            det.skipped_sensitive.join(", ")
        ));
    }
    lines.join("\n")
}
