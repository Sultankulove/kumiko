//! What the workbench remembers per workspace (spec, section 20): the layout, the open tabs of
//! each editor group, the split, recently opened files. Stored as
//! `<state>/workspaces/<hash of the first folder>.json`; a missing or unreadable file means
//! the defaults.

use std::path::{Path, PathBuf};

use kumiko_types::Uri;
use serde::{Deserialize, Serialize};

use crate::layout::LayoutState;
use crate::tabs::Orientation;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceState {
    pub layout: LayoutState,
    pub groups: Vec<GroupState>,
    pub active_group: usize,
    pub orientation: Orientation,
    pub recent: Vec<Uri>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            layout: LayoutState::default(),
            groups: Vec::new(),
            active_group: 0,
            orientation: Orientation::Right,
            recent: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupState {
    pub files: Vec<Uri>,
    pub active: Option<usize>,
}

/// The state file of the workspace whose first folder is `folder`.
pub fn path(workspaces_dir: &Path, folder: &Uri) -> PathBuf {
    workspaces_dir.join(format!("{:016x}.json", fnv1a(folder.as_str().as_bytes())))
}

/// FNV-1a: a stable hash (std's hasher may change between releases).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

pub fn load(path: &Path) -> WorkspaceState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes atomically: a temporary file renamed over the old one.
pub fn save(path: &Path, state: &WorkspaceState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ToolWindow;

    #[test]
    fn state_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let folder = Uri::parse("file:///w/project").unwrap();
        let file = path(dir.path(), &folder);
        assert_eq!(file, path(dir.path(), &folder), "stable");
        assert_ne!(
            file,
            path(dir.path(), &Uri::parse("file:///w/other").unwrap())
        );
        assert_eq!(
            load(&file),
            WorkspaceState::default(),
            "missing file: defaults"
        );

        let state = WorkspaceState {
            layout: LayoutState {
                bottom: Some(ToolWindow::Output),
                ..LayoutState::default()
            },
            groups: vec![GroupState {
                files: vec![Uri::parse("file:///w/project/a.txt").unwrap()],
                active: Some(0),
            }],
            active_group: 0,
            orientation: Orientation::Down,
            recent: vec![Uri::parse("file:///w/project/a.txt").unwrap()],
        };
        save(&file, &state).unwrap();
        assert_eq!(load(&file), state);

        std::fs::write(&file, "not json").unwrap();
        assert_eq!(
            load(&file),
            WorkspaceState::default(),
            "broken file: defaults"
        );
    }
}
