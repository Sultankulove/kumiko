//! kumiko.git (L2w, spec section 19): Git through the system `git` (process/run), shown with
//! declarative UI only: the Commit view (`ui/tree/setModel`), colours of changed files in the
//! project tree (`ui/fileDecorations`), the branch in the status bar. Provides the interface
//! `kumiko.git.repository` to other plugins.

pub mod status;

use std::cell::RefCell;

use kumiko_plugin_sdk::wasm::{Context, Plugin, PluginMain};
use kumiko_plugin_sdk::{ErrorCode, ResponseError};
use serde_json::{Value, json};

use crate::status::Status;

thread_local! {
    /// Files decorated last time, to clear the ones that are clean now.
    static DECORATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The view of the manifest.
pub const VIEW: &str = "git.changes";

fn failed(message: impl Into<String>) -> ResponseError {
    ResponseError::new(ErrorCode::InternalError, message.into())
}

/// Runs `git` in the first workspace folder; the output, or the error git printed.
fn git(ctx: &Context, args: &[&str]) -> Result<String, ResponseError> {
    let ran = ctx.call(
        "process/run",
        json!({ "spec": { "command": "git", "args": args, "cwd": null } }),
    )?;
    if ran["exitCode"] == json!(0) {
        Ok(ran["stdout"].as_str().unwrap_or_default().to_owned())
    } else {
        let stderr = ran["stderr"].as_str().unwrap_or_default().trim();
        let stdout = ran["stdout"].as_str().unwrap_or_default().trim();
        Err(failed(if stderr.is_empty() { stdout } else { stderr }))
    }
}

/// The workspace folder's URI, where repository paths start.
fn root(ctx: &Context) -> Option<String> {
    ctx.params()
        .workspace
        .folders
        .first()
        .map(|f| f.uri.as_str().trim_end_matches('/').to_owned())
}

/// A repository path as a URI under `root`: path segments percent-encoded.
pub fn file_uri(root: &str, path: &str) -> String {
    let mut out = String::from(root);
    for segment in path.split('/') {
        out.push('/');
        for byte in segment.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

fn read_status(ctx: &Context) -> Result<Status, ResponseError> {
    let output = git(ctx, &["status", "--porcelain=v2", "-z", "--branch"])?;
    Ok(status::parse(&output))
}

/// The Commit view: changes and unversioned files, a click opens the file.
pub fn tree(root: &str, status: &Status) -> Value {
    let node = |f: &status::FileStatus| {
        let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
        json!({
            "id": format!("file:{}", f.path),
            "label": name,
            "description": dir,
            "icon": format!("vcs.{}", f.change()),
            "tooltip": f.change(),
            "contextValue": f.change(),
            "command": { "command": "workbench.openFile", "args": [file_uri(root, &f.path)] },
        })
    };
    let (unversioned, changes): (Vec<_>, Vec<_>) = status
        .files
        .iter()
        .partition(|f| f.kind == status::Kind::Untracked);
    let group = |id: &str, label: &str, files: &[&status::FileStatus]| {
        json!({
            "id": id,
            "label": label,
            "description": files.len().to_string(),
            "collapsible": "expanded",
            "children": files.iter().map(|f| node(f)).collect::<Vec<_>>(),
        })
    };
    let mut roots = vec![group("changes", "Changes", &changes)];
    if !unversioned.is_empty() {
        roots.push(group("unversioned", "Unversioned Files", &unversioned));
    }
    json!(roots)
}

/// Reads the status and shows it: the view, file colours, the branch.
fn refresh(ctx: &Context) -> Result<Value, ResponseError> {
    let status = read_status(ctx)?;
    let root = root(ctx).unwrap_or_default();
    ctx.call(
        "ui/tree/setModel",
        json!({ "viewId": VIEW, "roots": tree(&root, &status) }),
    )?;
    let decorated: Vec<String> = status
        .files
        .iter()
        .map(|f| file_uri(&root, &f.path))
        .collect();
    let set: Vec<Value> = status
        .files
        .iter()
        .map(|f| {
            json!({
                "uri": file_uri(&root, &f.path),
                "badge": f.change().chars().next().map(|c| c.to_ascii_uppercase().to_string()),
                "color": format!("vcs.{}", f.change()),
                "tooltip": f.change(),
            })
        })
        .collect();
    let clear: Vec<String> = DECORATED.with_borrow(|old| {
        old.iter()
            .filter(|u| !decorated.contains(u))
            .cloned()
            .collect()
    });
    ctx.call("ui/fileDecorations", json!({ "set": set, "clear": clear }))?;
    DECORATED.with_borrow_mut(|old| *old = decorated);
    let branch = status.branch.clone().unwrap_or_else(|| "detached".into());
    let mut text = format!("⎇ {branch}");
    if status.ahead > 0 {
        text.push_str(&format!(" ↑{}", status.ahead));
    }
    if status.behind > 0 {
        text.push_str(&format!(" ↓{}", status.behind));
    }
    ctx.call(
        "ui/setStatusItem",
        json!({
            "id": "branch",
            "item": {
                "text": text,
                "tooltip": status.upstream,
                "command": { "command": "git.checkout", "args": [] },
                "alignment": "right",
                "priority": 10,
            },
        }),
    )?;
    Ok(status.to_json())
}

/// Shows git's answer of a network command (pull, push) and refreshes.
fn report(ctx: &Context, result: Result<String, ResponseError>) -> Result<Value, ResponseError> {
    let (level, message) = match &result {
        Ok(out) => (
            "info",
            if out.trim().is_empty() {
                "Done".to_owned()
            } else {
                out.trim().to_owned()
            },
        ),
        Err(error) => ("error", error.message.clone()),
    };
    let _ = ctx.call(
        "ui/showMessage",
        json!({ "level": level, "message": message, "actions": [] }),
    );
    let _ = refresh(ctx);
    result.map(Value::String)
}

struct Git;

impl PluginMain for Git {
    fn build() -> Plugin {
        Plugin::new()
            .command("git.status", |ctx, _| refresh(ctx))
            .command("git.refresh", |ctx, _| refresh(ctx))
            .command("git.add", |ctx, args| {
                let paths: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
                let mut command = vec!["add", "--"];
                if paths.is_empty() {
                    command.push(".");
                } else {
                    command.extend(paths);
                }
                git(ctx, &command)?;
                refresh(ctx)
            })
            .command("git.commit", |ctx, args| {
                let message = match args.first().and_then(Value::as_str) {
                    Some(message) => Some(message.to_owned()),
                    None => ctx
                        .call(
                            "ui/showInput",
                            json!({ "prompt": "Commit message", "placeholder": "Commit message" }),
                        )?
                        .as_str()
                        .map(str::to_owned),
                };
                let Some(message) = message.filter(|m| !m.trim().is_empty()) else {
                    return Ok(Value::Null); // cancelled
                };
                // As IntelliJ: the changes of tracked files, staged or not.
                git(ctx, &["commit", "-a", "-m", &message])?;
                refresh(ctx)
            })
            .command("git.checkout", |ctx, args| {
                let branch = match args.first().and_then(Value::as_str) {
                    Some(branch) => Some(branch.to_owned()),
                    None => {
                        let list = git(ctx, &["branch", "--format=%(refname:short)"])?;
                        let items: Vec<Value> = list
                            .lines()
                            .map(|b| json!({ "id": b, "label": b }))
                            .collect();
                        let picked = ctx.call(
                            "ui/showQuickPick",
                            json!({ "items": items, "placeholder": "Branch to check out" }),
                        )?;
                        picked["selected"][0].as_str().map(str::to_owned)
                    }
                };
                let Some(branch) = branch else {
                    return Ok(Value::Null);
                };
                git(ctx, &["checkout", &branch])?;
                refresh(ctx)
            })
            .command("git.createBranch", |ctx, args| {
                let name = match args.first().and_then(Value::as_str) {
                    Some(name) => Some(name.to_owned()),
                    None => ctx
                        .call("ui/showInput", json!({ "prompt": "New branch name" }))?
                        .as_str()
                        .map(str::to_owned),
                };
                let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
                    return Ok(Value::Null);
                };
                git(ctx, &["checkout", "-b", name.trim()])?;
                refresh(ctx)
            })
            .command("git.pull", |ctx, _| {
                let result = git(ctx, &["pull", "--ff-only"]);
                report(ctx, result)
            })
            .command("git.push", |ctx, _| {
                let result = git(ctx, &["push"]);
                report(ctx, result)
            })
            .command("git.diff", |ctx, args| {
                let path = args.first().and_then(Value::as_str).unwrap_or(".");
                git(ctx, &["diff", "--", path]).map(Value::String)
            })
            .interface(
                "kumiko.git.repository",
                "status",
                |ctx, _caller, _params| Ok(read_status(ctx)?.to_json()),
            )
            .interface(
                "kumiko.git.repository",
                "branch",
                |ctx, _caller, _params| Ok(json!(read_status(ctx)?.branch)),
            )
    }
}

kumiko_plugin_sdk::export_wasm_plugin!(Git);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_the_view() {
        assert_eq!(
            file_uri("file:///w", "src/New File.java"),
            "file:///w/src/New%20File.java"
        );
        let status = status::parse("1 .M N... a b c d e src/A.java\0? notes.txt\0");
        let roots = tree("file:///w", &status);
        assert_eq!(roots[0]["label"], "Changes");
        assert_eq!(roots[0]["children"][0]["label"], "A.java");
        assert_eq!(roots[0]["children"][0]["description"], "src");
        assert_eq!(roots[1]["children"][0]["icon"], "vcs.untracked");
        assert_eq!(
            roots[0]["children"][0]["command"]["args"][0],
            "file:///w/src/A.java"
        );
    }
}
