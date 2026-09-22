//! Architecture guards (docs/spec, section 10):
//!
//! 1. The internal crate graph follows the table of allowed dependencies.
//! 2. L0/L1 code does not mention specific languages, build tools or VCS
//!    (`java`, `maven`, `gradle`, `git`), except in comments and `#[cfg(test)]` code.
//!
//! Only normal and build dependencies are checked: dev-dependencies do not ship.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, MetadataCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Types,
    Protocol,
    Ipc,
    Service,
    Core,
    Ui,
    Headless,
    Sdk,
    Plugin,
    App,
    TestHarness,
}

impl Layer {
    fn may_depend_on(self, target: Layer) -> bool {
        use Layer::*;
        match self {
            Types => false,
            Protocol => matches!(target, Types),
            Ipc => matches!(target, Types | Protocol),
            // Services talk to each other only through traits, never through direct crate edges.
            Service => matches!(target, Types | Protocol | Ipc),
            Core => matches!(target, Types | Protocol | Ipc | Service),
            Ui | Headless => matches!(target, Types | Core),
            Sdk => matches!(target, Types | Protocol | Ipc),
            Plugin => matches!(target, Sdk),
            App => matches!(target, Types | Protocol | Ipc | Service | Core | Ui),
            TestHarness => true,
        }
    }

    /// L0/L1 layers must stay free of language, build tool and VCS knowledge.
    fn is_language_agnostic(self) -> bool {
        use Layer::*;
        matches!(self, Types | Protocol | Ipc | Service | Core | Ui | App)
    }
}

const SERVICES: &[&str] = &[
    "kumiko-events",
    "kumiko-commands",
    "kumiko-config",
    "kumiko-fs",
    "kumiko-documents",
    "kumiko-project",
    "kumiko-process",
    "kumiko-search",
    "kumiko-syntax",
    "kumiko-lsp",
    "kumiko-resources",
    "kumiko-plugin-host",
    "kumiko-wasm-host",
];

const UI_CRATES: &[&str] = &["kumiko-ui", "kumiko-editor-view", "kumiko-terminal-view"];

const FORBIDDEN_WORDS: &[&str] = &["java", "maven", "gradle", "git"];

/// `rel_path` is the package directory relative to the workspace root, with `/` separators.
fn classify(name: &str, rel_path: &str) -> Option<Layer> {
    if rel_path.starts_with("plugins/") || rel_path.starts_with("sdk/examples/") {
        return Some(Layer::Plugin);
    }
    let layer = match name {
        "kumiko-types" => Layer::Types,
        "kumiko-protocol" => Layer::Protocol,
        "kumiko-ipc" => Layer::Ipc,
        "kumiko-core" => Layer::Core,
        "kumiko-headless" => Layer::Headless,
        "kumiko-plugin-sdk" => Layer::Sdk,
        "kumiko" => Layer::App,
        "kumiko-tests" => Layer::TestHarness,
        n if SERVICES.contains(&n) => Layer::Service,
        n if UI_CRATES.contains(&n) => Layer::Ui,
        _ => return None,
    };
    Some(layer)
}

struct Package {
    name: String,
    rel_path: String,
    /// Internal (workspace) normal and build dependencies.
    deps: Vec<String>,
}

fn check_graph(packages: &[Package]) -> Vec<String> {
    let mut violations = Vec::new();
    let layers: BTreeMap<&str, Option<Layer>> = packages
        .iter()
        .map(|p| (p.name.as_str(), classify(&p.name, &p.rel_path)))
        .collect();

    for p in packages {
        let Some(from) = layers[p.name.as_str()] else {
            violations.push(format!(
                "{} ({}): crate is not classified; add it to tests/architecture.rs",
                p.name, p.rel_path
            ));
            continue;
        };
        for dep in &p.deps {
            let Some(Some(to)) = layers.get(dep.as_str()) else {
                continue; // unclassified target is reported on its own
            };
            if !from.may_depend_on(*to) {
                violations.push(format!(
                    "{} ({from:?}) must not depend on {dep} ({to:?})",
                    p.name
                ));
            }
        }
    }
    violations
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tests crate lives in <root>/tests")
        .to_path_buf()
}

fn workspace_packages() -> Vec<Package> {
    let root = workspace_root();
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("cargo metadata");
    let members: BTreeSet<&str> = metadata.packages.iter().map(|p| p.name.as_str()).collect();

    metadata
        .packages
        .iter()
        .map(|p| {
            let dir = p.manifest_path.parent().expect("manifest has a parent");
            let rel = dir
                .as_std_path()
                .strip_prefix(&root)
                .expect("member inside workspace root")
                .to_string_lossy()
                .replace('\\', "/");
            let deps = p
                .dependencies
                .iter()
                .filter(|d| matches!(d.kind, DependencyKind::Normal | DependencyKind::Build))
                .filter(|d| members.contains(d.name.as_str()))
                .map(|d| d.name.clone())
                .collect();
            Package {
                name: p.name.to_string(),
                rel_path: rel,
                deps,
            }
        })
        .collect()
}

#[test]
fn crate_graph_follows_allowed_dependencies() {
    let violations = check_graph(&workspace_packages());
    assert!(
        violations.is_empty(),
        "forbidden crate dependencies (docs/spec, section 10):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn graph_check_rejects_forbidden_edges() {
    let pkg = |name: &str, rel: &str, deps: &[&str]| Package {
        name: name.into(),
        rel_path: rel.into(),
        deps: deps.iter().map(|d| d.to_string()).collect(),
    };
    let packages = [
        pkg("kumiko-types", "crates/kumiko-types", &[]),
        pkg("kumiko-core", "crates/kumiko-core", &["kumiko-events"]),
        pkg(
            "kumiko-events",
            "crates/kumiko-events",
            &["kumiko-ui", "kumiko-types"],
        ),
        pkg(
            "kumiko-documents",
            "crates/kumiko-documents",
            &["kumiko-events"],
        ),
        pkg(
            "kumiko-ui",
            "crates/kumiko-ui",
            &["kumiko-core", "kumiko-documents"],
        ),
        pkg(
            "kumiko-plugin-sdk",
            "sdk/rust/kumiko-plugin-sdk",
            &["kumiko-core"],
        ),
        pkg(
            "kumiko-git",
            "plugins/git",
            &["kumiko-plugin-sdk", "kumiko-types"],
        ),
        pkg("kumiko", "app/kumiko", &["kumiko-git"]),
        pkg("kumiko-mystery", "crates/kumiko-mystery", &[]),
    ];
    let violations = check_graph(&packages);
    let expected = [
        "kumiko-events (Service) must not depend on kumiko-ui (Ui)",
        "kumiko-documents (Service) must not depend on kumiko-events (Service)",
        "kumiko-ui (Ui) must not depend on kumiko-documents (Service)",
        "kumiko-plugin-sdk (Sdk) must not depend on kumiko-core (Core)",
        "kumiko-git (Plugin) must not depend on kumiko-types (Types)",
        "kumiko (App) must not depend on kumiko-git (Plugin)",
        "kumiko-mystery (crates/kumiko-mystery): crate is not classified; add it to tests/architecture.rs",
    ];
    assert_eq!(violations, expected);
}

// ---------------------------------------------------------------------------
// Language-agnostic L0/L1 code
// ---------------------------------------------------------------------------

/// Replaces comments with spaces (keeping newlines, so line numbers survive) and cuts
/// everything from the first `#[cfg(test)]` on: by convention test modules close a file.
fn strip_comments_and_tests(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                let mut depth = 0usize;
                while i < chars.len() {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        out.push_str("  ");
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        out.push_str("  ");
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        out.push(blank(chars[i]));
                        i += 1;
                    }
                }
            }
            'r' if matches!(next, Some('"') | Some('#'))
                && (i == 0 || !is_ident_char(chars[i - 1])) =>
            {
                // raw string: r"..." or r#"..."#
                let mut j = i + 1;
                let mut hashes = 0;
                while chars.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                if chars.get(j) != Some(&'"') {
                    out.push(c);
                    i += 1;
                    continue;
                }
                let end = (j + 1..chars.len()).find(|&k| {
                    chars[k] == '"' && (1..=hashes).all(|h| chars.get(k + h) == Some(&'#'))
                });
                let stop = end.map_or(chars.len(), |k| k + 1 + hashes);
                out.extend(&chars[i..stop]);
                i = stop;
            }
            '"' => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    let s = chars[i];
                    out.push(s);
                    i += 1;
                    if s == '\\' {
                        if let Some(&e) = chars.get(i) {
                            out.push(e);
                            i += 1;
                        }
                    } else if s == '"' {
                        break;
                    }
                }
            }
            '\'' => {
                // char literal ('x', '\n', '"') or lifetime ('a)
                let len = if next == Some('\\') {
                    (i + 2..chars.len())
                        .find(|&k| chars[k] == '\'')
                        .map_or(1, |k| k + 1 - i)
                } else if chars.get(i + 2) == Some(&'\'') {
                    3
                } else {
                    1
                };
                out.extend(&chars[i..i + len]);
                i += len;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    match out.find("#[cfg(test)]") {
        Some(pos) => out[..pos].to_string(),
        None => out,
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits code into lowercase sub-words: `GitStatus` -> `git`, `status`; `java_home` -> `java`, `home`.
fn words(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    for token in line.split(|c: char| !c.is_alphanumeric()) {
        let mut current = String::new();
        let mut prev_lower = false;
        for c in token.chars() {
            if c.is_uppercase() && prev_lower && !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            current.extend(c.to_lowercase());
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    result
}

fn find_forbidden_words(src: &str) -> Vec<(usize, String)> {
    strip_comments_and_tests(src)
        .lines()
        .enumerate()
        .flat_map(|(n, line)| {
            words(line)
                .into_iter()
                .filter(|w| FORBIDDEN_WORDS.contains(&w.as_str()))
                .map(move |w| (n + 1, w))
        })
        .collect()
}

/// Collects `.rs` files under `path` (a directory or a single file), skipping `tests/` directories.
fn rust_sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.file_name().is_some_and(|n| n == "tests") {
            continue;
        }
        rust_sources(&path, out);
    }
}

#[test]
fn core_and_ui_code_is_language_agnostic() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for p in workspace_packages() {
        let Some(layer) = classify(&p.name, &p.rel_path) else {
            continue; // reported by the graph test
        };
        if !layer.is_language_agnostic() {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&root.join(&p.rel_path).join("src"), &mut files);
        rust_sources(&root.join(&p.rel_path).join("build.rs"), &mut files);
        for file in files {
            let src = fs::read_to_string(&file).expect("readable source file");
            for (line, word) in find_forbidden_words(&src) {
                let rel = file.strip_prefix(&root).unwrap_or(&file).display();
                violations.push(format!("{rel}:{line}: `{word}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "L0/L1 code must not know about specific languages, build tools or VCS \
         (docs/spec, section 30, rule 2):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn language_scan_finds_code_and_skips_comments_and_tests() {
    let src = r##"
//! Uri schemes like `git:` are not interpreted by the core.
fn digit_count() {} // mentions git and java, but only in a comment
/* block about maven /* nested gradle */ still comment */
let url = "https://example.com/"; let status = GitStatus::new();
let id = "kumiko.java";
let s = r#"plain "raw" text"#; let q = '"'; let life: &'static str = "ok";
let home = java_home;
#[cfg(test)]
mod tests { fn git() {} }
"##;
    let found = find_forbidden_words(src);
    assert_eq!(
        found,
        [
            (5, "git".to_string()),
            (6, "java".to_string()),
            (8, "java".to_string())
        ]
    );
}
