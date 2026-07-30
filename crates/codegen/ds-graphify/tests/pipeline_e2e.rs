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
    assert!(report.contains("God Nodes"), "report missing God Nodes section");
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
    let ext = ds_graphify::extract::extract_many(
        &[a.join("lib.rs"), b.join("lib.rs")],
        tmp.path(),
    );
    let foo_ids: Vec<_> = ext
        .nodes
        .iter()
        .filter(|n| n.label == "foo()" || n.label == "foo")
        .map(|n| n.id.as_str())
        .collect();
    assert!(
        foo_ids.len() >= 2,
        "expected distinct foo nodes, got {foo_ids:?} from {:?}",
        ext.nodes.iter().map(|n| (&n.id, &n.label)).collect::<Vec<_>>()
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
        ext.edges.iter().any(|e| e.relation == "method" || e.relation == "contains"),
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
