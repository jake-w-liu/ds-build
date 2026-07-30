//! `graphify` CLI — Graphify-compatible entrypoint (native Rust).

use clap::{Parser, Subcommand};
use ds_graphify::pipeline::{self, OUT_DIR_NAME, PipelineOptions};
use ds_graphify::query;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "graphify",
    about = "Native Graphify: map a codebase into a queryable knowledge graph",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to scan (default: .) when no subcommand is given
    path: Option<PathBuf>,

    /// Output directory (default: graphify-out)
    #[arg(long, global = true, default_value = OUT_DIR_NAME)]
    out: PathBuf,

    /// Skip HTML visualization
    #[arg(long, global = true)]
    no_viz: bool,

    /// Build a directed graph
    #[arg(long, global = true)]
    directed: bool,

    /// Community resolution (>1 more communities)
    #[arg(long, global = true, default_value_t = 1.0)]
    resolution: f64,

    /// Only re-cluster existing graph.json
    #[arg(long, global = true)]
    cluster_only: bool,

    /// Incremental rebuild entrypoint (re-extract corpus)
    #[arg(long, global = true)]
    update: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the knowledge graph for a path (default command)
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Alias for build with --update
    Update {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Query the graph with a natural-language question
    Query {
        question: String,
        #[arg(long)]
        dfs: bool,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        #[arg(long, default_value_t = 2000)]
        budget: usize,
        #[arg(long)]
        graph: Option<PathBuf>,
    },
    /// Shortest path between two concepts
    Path {
        from: String,
        to: String,
        #[arg(long)]
        graph: Option<PathBuf>,
    },
    /// Explain a single concept node
    Explain {
        name: String,
        #[arg(long)]
        graph: Option<PathBuf>,
    },
    /// Detect files only (no extract)
    Detect {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Extract only (writes .graphify_ast.json)
    Extract {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("graphify: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let out = cli.out;
    let no_viz = cli.no_viz;
    let directed = cli.directed;
    let resolution = cli.resolution;
    let cluster_only = cli.cluster_only;
    let update_flag = cli.update;
    let bare_path = cli.path;

    match cli.command {
        Some(Commands::Query {
            question,
            dfs,
            depth,
            budget,
            graph,
        }) => {
            let gp = graph.unwrap_or_else(|| out.join("graph.json"));
            let g = query::load_graph(&gp)?;
            let mode = if dfs { "dfs" } else { "bfs" };
            println!("{}", query::query_graph(&g, &question, mode, depth, budget));
            Ok(())
        }
        Some(Commands::Path { from, to, graph }) => {
            let gp = graph.unwrap_or_else(|| out.join("graph.json"));
            let g = query::load_graph(&gp)?;
            println!("{}", query::path_between(&g, &from, &to));
            Ok(())
        }
        Some(Commands::Explain { name, graph }) => {
            let gp = graph.unwrap_or_else(|| out.join("graph.json"));
            let g = query::load_graph(&gp)?;
            println!("{}", query::explain(&g, &name));
            Ok(())
        }
        Some(Commands::Detect { path }) => {
            let det = ds_graphify::detect::detect(&path)?;
            println!("{}", pipeline::format_detection_summary(&det));
            Ok(())
        }
        Some(Commands::Extract { path }) => {
            let det = ds_graphify::detect::detect(&path)?;
            let root = PathBuf::from(&det.scan_root);
            let mut paths = ds_graphify::detect::all_code_paths(&det);
            paths.extend(ds_graphify::detect::all_doc_paths(&det));
            let extraction = ds_graphify::extract::extract_many(&paths, &root);
            std::fs::create_dir_all(&out)?;
            let out_file = out.join(".graphify_ast.json");
            std::fs::write(&out_file, serde_json::to_string_pretty(&extraction)?)?;
            println!(
                "AST: {} nodes, {} edges → {}",
                extraction.nodes.len(),
                extraction.edges.len(),
                out_file.display()
            );
            Ok(())
        }
        Some(Commands::Update { path }) => {
            run_build(path, out, directed, no_viz, resolution, cluster_only, true)
        }
        Some(Commands::Build { path }) => run_build(
            path,
            out,
            directed,
            no_viz,
            resolution,
            cluster_only,
            update_flag,
        ),
        None => {
            let path = bare_path.unwrap_or_else(|| PathBuf::from("."));
            run_build(
                path,
                out,
                directed,
                no_viz,
                resolution,
                cluster_only,
                update_flag,
            )
        }
    }
}

fn run_build(
    path: PathBuf,
    out: PathBuf,
    directed: bool,
    no_viz: bool,
    resolution: f64,
    cluster_only: bool,
    update: bool,
) -> anyhow::Result<()> {
    let opts = PipelineOptions {
        root: path,
        out_dir: out,
        directed,
        no_viz,
        resolution,
        cluster_only,
        update,
    };
    let result = pipeline::run(&opts)?;
    println!("{}", pipeline::format_detection_summary(&result.detection));
    println!(
        "\nGraph: {} nodes · {} edges · {} communities",
        result.nodes, result.edges, result.communities
    );
    println!("  {}", result.graph_json_path.display());
    println!("  {}", result.report_path.display());
    if let Some(html) = &result.graph_html_path {
        println!("  {}", html.display());
    }
    Ok(())
}
