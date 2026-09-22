//! Compatibility test (spec, section 16): the published protocol artifacts match the code.
//!
//! - `sdk/schema/kpp.schema.json` equals the schema generated from the catalog;
//! - the generated block of `docs/protocol/v1.md` equals the catalog tables;
//! - the WIT package parses, carries the protocol version and has the expected functions;
//! - `CHANGELOG-protocol.md` has an entry for the protocol version.
//!
//! After an intentional change run `KUMIKO_BLESS=1 cargo test -p kumiko-protocol --test artifacts`
//! to regenerate the schema and the docs block, then review the diff.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kumiko_protocol::PROTOCOL_VERSION;
use kumiko_protocol::schema::{catalog_markdown, protocol_schema_file};

const BEGIN: &str = "<!-- BEGIN GENERATED: catalog -->\n";
const END: &str = "<!-- END GENERATED: catalog -->";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bless() -> bool {
    std::env::var_os("KUMIKO_BLESS").is_some_and(|v| v == "1")
}

fn check_file(path: &Path, expected: &str) {
    let actual = std::fs::read_to_string(path).unwrap_or_default();
    if actual == expected {
        return;
    }
    if bless() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, expected).unwrap();
        return;
    }
    panic!(
        "{} is out of date with kumiko-protocol; run \
         `KUMIKO_BLESS=1 cargo test -p kumiko-protocol --test artifacts`, review the diff \
         and add an entry to CHANGELOG-protocol.md",
        path.display()
    );
}

#[test]
fn schema_file_is_up_to_date() {
    check_file(
        &repo_root().join("sdk/schema/kpp.schema.json"),
        &protocol_schema_file(),
    );
}

#[test]
fn docs_catalog_is_up_to_date() {
    let path = repo_root().join("docs/protocol/v1.md");
    let doc = std::fs::read_to_string(&path).unwrap();
    let start = doc.find(BEGIN).expect("begin marker") + BEGIN.len();
    let end = doc.find(END).expect("end marker");
    let expected = format!("{}{}{}", &doc[..start], catalog_markdown(), &doc[end..]);
    check_file(&path, &expected);
}

#[test]
fn wit_package_matches_protocol() {
    let mut resolve = wit_parser::Resolve::new();
    let (package, _) = resolve
        .push_dir(repo_root().join("sdk/wit/kumiko-plugin"))
        .unwrap();

    let name = &resolve.packages[package].name;
    assert_eq!(
        (name.namespace.as_str(), name.name.as_str()),
        ("kumiko", "plugin")
    );
    assert_eq!(
        name.version.as_ref().map(ToString::to_string).as_deref(),
        Some(PROTOCOL_VERSION),
        "WIT package version must equal PROTOCOL_VERSION"
    );

    let world = &resolve.worlds[resolve
        .select_world(&[package], Some("kumiko-plugin"))
        .unwrap()];
    let functions = |items: &wit_parser::IndexMap<wit_parser::WorldKey, wit_parser::WorldItem>,
                     interface: &str| {
        items
            .values()
            .find_map(|item| match item {
                wit_parser::WorldItem::Interface { id, .. }
                    if resolve.interfaces[*id].name.as_deref() == Some(interface) =>
                {
                    Some(
                        resolve.interfaces[*id]
                            .functions
                            .keys()
                            .cloned()
                            .collect::<BTreeSet<_>>(),
                    )
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("world does not have interface `{interface}`"))
    };

    let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(
        functions(&world.imports, "host"),
        set(&["call", "notify", "stream-write", "stream-close"])
    );
    assert_eq!(
        functions(&world.exports, "plugin"),
        set(&["handle", "on-notify", "on-stream", "on-stream-close"])
    );
}

#[test]
fn changelog_has_current_version() {
    let changelog = std::fs::read_to_string(repo_root().join("CHANGELOG-protocol.md")).unwrap();
    assert!(
        changelog.contains(&format!("## {PROTOCOL_VERSION}")),
        "CHANGELOG-protocol.md has no `## {PROTOCOL_VERSION}` section"
    );
}
