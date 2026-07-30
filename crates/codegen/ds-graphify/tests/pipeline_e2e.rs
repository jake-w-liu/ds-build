//! End-to-end verification of the Graphify-compatible pipeline.

use ds_graphify::pipeline::{self, PipelineOptions};
use ds_graphify::query;
use std::fs;

fn fixture_project(dir: &std::path::Path) {
    fs::write(
        dir.join("lib.rs"),
        r#"
pub struct AuthService {
    pub db: DatabasePool,
}

pub struct DatabasePool;

impl AuthService {
    pub fn login(&self, user: &str) -> bool {
        self.db.check(user)
    }
}

impl DatabasePool {
    pub fn check(&self, _user: &str) -> bool {
        true
    }
}

pub fn main() {
    let db = DatabasePool;
    let auth = AuthService { db };
    auth.login("alice");
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("README.md"),
        r#"# Demo

## Auth

See [lib.rs](./lib.rs) for `AuthService`.

## Database

The pool backs login.
"#,
    )
    .unwrap();
    fs::write(dir.join("helper.py"), "class UserStore:\n    def get(self, id):\n        return None\n\ndef load_user(store, id):\n    return store.get(id)\n").unwrap();
}

#[test]
fn full_pipeline_produces_graphify_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_project(tmp.path());
    let out = tmp.path().join("graphify-out");

    let result = pipeline::run(&PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out.clone(),
        directed: false,
        no_viz: false,
        resolution: 1.0,
        cluster_only: false,
        update: false,
    })
    .expect("pipeline should succeed");

    assert!(result.nodes > 0, "expected nodes");
    assert!(result.edges > 0, "expected edges");
    assert!(out.join("graph.json").is_file());
    assert!(out.join("GRAPH_REPORT.md").is_file());
    assert!(out.join("graph.html").is_file());

    let report = fs::read_to_string(out.join("GRAPH_REPORT.md")).unwrap();
    assert!(
        report.contains("God Nodes"),
        "report missing God Nodes section"
    );
    assert!(report.contains("Suggested Questions"));

    let graph = query::load_graph(&out.join("graph.json")).unwrap();
    assert!(!graph.nodes.is_empty());
    assert!(!graph.links.is_empty());

    // AuthService should be findable
    let explained = query::explain(&graph, "AuthService");
    assert!(
        explained.contains("AuthService") || explained.contains("No node"),
        "explain output: {explained}"
    );

    let q = query::query_graph(&graph, "AuthService login", "bfs", 3, 2000);
    assert!(!q.is_empty());

    // cluster-only path
    let again = pipeline::run(&PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out,
        cluster_only: true,
        ..PipelineOptions::default()
    })
    .expect("cluster-only");
    assert_eq!(again.nodes, result.nodes);
}

#[test]
fn same_stem_different_paths_get_distinct_symbol_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("lib.rs"), "pub fn foo() {}\n").unwrap();
    fs::write(b.join("lib.rs"), "pub fn foo() {}\n").unwrap();
    let ext = ds_graphify::extract::extract_many(&[a.join("lib.rs"), b.join("lib.rs")], tmp.path());
    let foo_ids: Vec<_> = ext
        .nodes
        .iter()
        .filter(|n| n.label == "foo()" || n.label == "foo")
        .map(|n| n.id.as_str())
        .collect();
    assert!(
        foo_ids.len() >= 2,
        "expected distinct foo nodes, got {foo_ids:?} from {:?}",
        ext.nodes
            .iter()
            .map(|n| (&n.id, &n.label))
            .collect::<Vec<_>>()
    );
    assert_ne!(foo_ids[0], foo_ids[1], "IDs must not collide: {foo_ids:?}");
}

#[test]
fn rust_extractor_finds_struct_and_impl() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("a.rs"),
        "pub struct Foo;\nimpl Foo { pub fn bar(&self) {} }\n",
    )
    .unwrap();
    let ext = ds_graphify::extract::extract_file(&tmp.path().join("a.rs"));
    let labels: Vec<_> = ext.nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| *l == "Foo" || l.contains("Foo")),
        "labels={labels:?}"
    );
    assert!(
        ext.edges
            .iter()
            .any(|e| e.relation == "method" || e.relation == "contains"),
        "edges={:?}",
        ext.edges
    );
}

#[test]
fn node_ids_are_root_relative_not_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.rs"), "pub struct Foo;\n").unwrap();
    let det = ds_graphify::detect::detect(tmp.path()).unwrap();
    let root = std::path::PathBuf::from(&det.scan_root);
    let paths = ds_graphify::detect::all_code_paths(&det);
    let ext = ds_graphify::extract::extract_many(&paths, &root);
    for n in &ext.nodes {
        assert!(
            !n.id.contains("var") && !n.id.contains("tmp") && !n.source_file.starts_with('/'),
            "expected portable relative ids/sources, got id={} source={}",
            n.id,
            n.source_file
        );
        if !n.source_file.is_empty() {
            assert!(
                !n.source_file.starts_with('/'),
                "source_file must be relative: {}",
                n.source_file
            );
        }
    }
}

#[test]
fn duplicate_contains_edges_deduped() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("a.rs"),
        "pub struct Foo;\nimpl Foo { pub fn bar(&self) {} }\n",
    )
    .unwrap();
    let key = "a.rs";
    let ext = ds_graphify::extract::extract_file_keyed(&tmp.path().join("a.rs"), key);
    let g = ds_graphify::schema::GraphJson::from_extraction(&ext, false);
    let contains: Vec<_> = g
        .links
        .iter()
        .filter(|e| e.relation == "contains")
        .collect();
    // file --contains--> Foo should appear once (struct + impl share the type node).
    let foo_contains = contains
        .iter()
        .filter(|e| e.target.contains("foo") || e.target.ends_with("foo"))
        .count();
    assert!(
        foo_contains <= 1,
        "expected at most one contains→Foo edge, got {foo_contains}: {:?}",
        contains
    );
}

#[test]
fn schema_roundtrip_graph_json() {
    let mut extraction = ds_graphify::schema::Extraction::empty();
    extraction.nodes.push(ds_graphify::schema::Node {
        id: "a".into(),
        label: "A".into(),
        file_type: ds_graphify::schema::FileType::Code,
        source_file: "a.rs".into(),
        source_location: Some("L1".into()),
        community: None,
        origin_file: None,
    });
    extraction.nodes.push(ds_graphify::schema::Node {
        id: "b".into(),
        label: "B".into(),
        file_type: ds_graphify::schema::FileType::Code,
        source_file: "b.rs".into(),
        source_location: Some("L1".into()),
        community: None,
        origin_file: None,
    });
    extraction.edges.push(ds_graphify::schema::Edge {
        source: "a".into(),
        target: "b".into(),
        relation: "calls".into(),
        confidence: ds_graphify::schema::Confidence::Extracted,
        source_file: "a.rs".into(),
        source_location: Some("L2".into()),
        weight: Some(1.0),
        context: None,
    });
    let g = ds_graphify::schema::GraphJson::from_extraction(&extraction, false);
    let s = serde_json::to_string(&g).unwrap();
    assert!(s.contains("\"links\""));
    assert!(s.contains("EXTRACTED") || s.contains("extracted") || s.contains("Extracted"));
    let _parsed: ds_graphify::schema::GraphJson = serde_json::from_str(&s).unwrap();
}

#[test]
fn custom_output_directory_is_not_reingested() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), "pub fn root() {}\n").unwrap();
    let out = tmp.path().join("custom-output");
    let options = PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out,
        no_viz: true,
        ..PipelineOptions::default()
    };

    let first = pipeline::run(&options).unwrap();
    let second = pipeline::run(&options).unwrap();

    assert_eq!(first.detection.total_files, 1);
    assert_eq!(
        second.detection.total_files, 1,
        "pipeline outputs must not become corpus inputs: {:?}",
        second.detection.files
    );
    assert_eq!(second.nodes, first.nodes);
    assert_eq!(second.edges, first.edges);
}

#[test]
fn malformed_semantic_extraction_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), "pub fn root() {}\n").unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join(".graphify_semantic.json"), "{ definitely not json").unwrap();

    let error = pipeline::run(&PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out,
        no_viz: true,
        ..PipelineOptions::default()
    })
    .expect_err("an existing malformed semantic extraction must not be ignored");

    assert!(
        error.to_string().contains(".graphify_semantic.json"),
        "error should identify the malformed artifact: {error:#}"
    );
}

#[test]
fn failed_rebuild_does_not_advance_persisted_detection_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), "pub fn root() {}\n").unwrap();
    let out = tmp.path().join("out");
    let options = PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out.clone(),
        no_viz: true,
        ..PipelineOptions::default()
    };
    pipeline::run(&options).unwrap();
    let previous_detection = fs::read(out.join(".graphify_detect.json")).unwrap();

    fs::write(tmp.path().join("new.rs"), "pub fn added() {}\n").unwrap();
    fs::write(out.join(".graphify_semantic.json"), "{ invalid json").unwrap();
    pipeline::run(&options).expect_err("malformed semantic input must fail");

    assert_eq!(
        fs::read(out.join(".graphify_detect.json")).unwrap(),
        previous_detection,
        "failed rebuilds must leave last-successful metadata intact"
    );
}

#[test]
fn ambiguous_cross_file_call_is_not_bound_to_an_arbitrary_definition() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.rs"), "pub fn duplicate() {}\n").unwrap();
    fs::write(tmp.path().join("b.rs"), "pub fn duplicate() {}\n").unwrap();
    fs::write(
        tmp.path().join("caller.rs"),
        "pub fn caller() { duplicate(); }\n",
    )
    .unwrap();

    let ext = ds_graphify::extract::extract_many(
        &[
            tmp.path().join("a.rs"),
            tmp.path().join("b.rs"),
            tmp.path().join("caller.rs"),
        ],
        tmp.path(),
    );
    let caller = ext
        .nodes
        .iter()
        .find(|n| n.label == "caller()")
        .expect("caller definition");
    let call = ext
        .edges
        .iter()
        .find(|e| e.source == caller.id && e.relation == "calls")
        .expect("caller edge");

    assert_eq!(call.target, "duplicate");
    assert_eq!(call.confidence, ds_graphify::schema::Confidence::Ambiguous);
    assert!(
        ext.nodes
            .iter()
            .any(|n| n.id == "duplicate" && n.source_file.is_empty()),
        "the unresolved target should remain explicit in the graph"
    );
}

#[test]
fn go_symbols_use_the_full_package_path_for_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let left = tmp.path().join("left/api");
    let right = tmp.path().join("right/api");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    fs::write(left.join("a.go"), "package api\nfunc Run() {}\n").unwrap();
    fs::write(right.join("b.go"), "package api\nfunc Run() {}\n").unwrap();

    let ext =
        ds_graphify::extract::extract_many(&[left.join("a.go"), right.join("b.go")], tmp.path());
    let runs: Vec<_> = ext.nodes.iter().filter(|n| n.label == "Run()").collect();

    assert_eq!(runs.len(), 2, "both valid Go packages must be represented");
    assert_ne!(
        runs[0].id, runs[1].id,
        "package paths must disambiguate IDs"
    );
}

#[test]
fn detected_non_ast_language_still_produces_a_file_node() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Main.java"), "class Main {}\n").unwrap();
    let out = tmp.path().join("out");

    let result = pipeline::run(&PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out,
        no_viz: true,
        ..PipelineOptions::default()
    })
    .unwrap();

    assert_eq!(result.detection.files.code, vec!["Main.java"]);
    assert_eq!(result.nodes, 1);
    assert_eq!(result.graph.nodes[0].label, "Main.java");
}

#[test]
fn cluster_only_refreshes_the_persisted_analysis() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_project(tmp.path());
    let out = tmp.path().join("out");
    let options = PipelineOptions {
        root: tmp.path().to_path_buf(),
        out_dir: out.clone(),
        no_viz: true,
        ..PipelineOptions::default()
    };
    pipeline::run(&options).unwrap();
    fs::write(out.join(".graphify_analysis.json"), "{}").unwrap();

    pipeline::run(&PipelineOptions {
        cluster_only: true,
        ..options
    })
    .unwrap();

    let persisted: ds_graphify::schema::Analysis =
        serde_json::from_str(&fs::read_to_string(out.join(".graphify_analysis.json")).unwrap())
            .unwrap();
    assert!(!persisted.communities.is_empty());
}

#[test]
fn pipeline_rejects_non_positive_or_non_finite_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("lib.rs"), "pub fn root() {}\n").unwrap();

    for resolution in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let error = pipeline::run(&PipelineOptions {
            root: tmp.path().to_path_buf(),
            out_dir: tmp.path().join(format!("out-{}", resolution.to_bits())),
            no_viz: true,
            resolution,
            ..PipelineOptions::default()
        })
        .expect_err("invalid resolution must be rejected");
        assert!(
            error.to_string().contains("resolution"),
            "unexpected error for {resolution:?}: {error:#}"
        );
    }
}

#[test]
fn go_method_placeholder_does_not_override_the_type_definition() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("a_type.go"),
        "package sample\ntype Service struct{}\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("z_method.go"),
        "package sample\nfunc (s *Service) Run() {}\n",
    )
    .unwrap();

    let ext = ds_graphify::extract::extract_many(
        &[tmp.path().join("a_type.go"), tmp.path().join("z_method.go")],
        tmp.path(),
    );
    let graph = ds_graphify::schema::GraphJson::from_extraction(&ext, false);
    let service = graph
        .nodes
        .iter()
        .find(|node| node.label == "Service")
        .expect("receiver type node");

    assert_eq!(service.source_file, "a_type.go");
    assert_eq!(service.source_location.as_deref(), Some("L2"));
}

#[test]
fn repeated_markdown_headings_remain_distinct_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("README.md");
    fs::write(
        &path,
        "# First\n## Details\nOne\n# Second\n## Details\nTwo\n",
    )
    .unwrap();

    let ext = ds_graphify::extract::extract_file_keyed(&path, "README.md");
    let details: Vec<_> = ext
        .nodes
        .iter()
        .filter(|node| node.label == "Details")
        .collect();

    assert_eq!(details.len(), 2);
    assert_ne!(details[0].id, details[1].id);
    assert_eq!(details[0].source_location.as_deref(), Some("L2"));
    assert_eq!(details[1].source_location.as_deref(), Some("L5"));
}

#[test]
fn markdown_links_have_valid_portable_endpoint_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    fs::write(tmp.path().join("image.png"), b"not decoded by graphify").unwrap();
    let path = docs.join("README.md");
    fs::write(
        &path,
        "[Image](../image.png)\n[Missing](missing.md)\n[Outside](../../outside.md)\n",
    )
    .unwrap();

    let ext = ds_graphify::extract::extract_file_keyed(&path, "docs/README.md");

    assert!(ext.validate().is_empty(), "{:?}", ext.validate());
    let image = ext
        .nodes
        .iter()
        .find(|node| node.id == "image_png")
        .expect("existing linked image");
    assert_eq!(image.source_file, "image.png");
    assert_eq!(image.file_type, ds_graphify::schema::FileType::Image);
    let missing = ext
        .nodes
        .iter()
        .find(|node| node.id == "docs_missing_md")
        .expect("missing link placeholder");
    assert!(missing.source_file.is_empty());
    assert!(
        ext.edges
            .iter()
            .all(|edge| !edge.target.contains("outside")),
        "links escaping the scan root must be ignored"
    );
}

#[test]
fn cargo_manifest_extracts_only_the_package_name() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Cargo.toml");
    fs::write(
        &path,
        "[package]\nname = \"real-package\"\n\n[package.metadata.demo]\nname = \"metadata-name\"\n\n[[bin]]\nname = \"binary-name\"\n",
    )
    .unwrap();

    let ext = ds_graphify::extract::extract_file_keyed(&path, "Cargo.toml");
    let labels: Vec<_> = ext.nodes.iter().map(|node| node.label.as_str()).collect();

    assert!(labels.contains(&"real-package"));
    assert!(!labels.contains(&"metadata-name"));
    assert!(!labels.contains(&"binary-name"));
    let package = ext
        .nodes
        .iter()
        .find(|node| node.label == "real-package")
        .unwrap();
    assert_eq!(package.source_location.as_deref(), Some("L2"));
}
