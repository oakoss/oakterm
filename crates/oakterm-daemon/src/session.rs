//! Spec-0010 session persistence: serialize multiplexer state to
//! `session.json` so a later daemon can restore it (TREK-120). Today's
//! state is a single implicit workspace/tab wrapping the `PaneManager`
//! layout tree; the on-disk shape already carries the full hierarchy so
//! the format survives the tab model landing.

use crate::pane::PaneManager;
use oakterm_mux::{LayoutNode, PaneId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

/// Spec-0010 format version 1.
const SESSION_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SessionFile {
    pub(crate) version: u32,
    /// Unix epoch seconds at save time.
    pub(crate) saved_at: u64,
    pub(crate) daemon_version: String,
    pub(crate) workspaces: Vec<SavedWorkspace>,
    pub(crate) active_workspace: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedWorkspace {
    pub(crate) name: String,
    pub(crate) tabs: Vec<SavedTab>,
    pub(crate) active_tab: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedTab {
    pub(crate) name: String,
    pub(crate) layout: SavedLayoutNode,
    pub(crate) floating: Vec<SavedFloatingPane>,
    pub(crate) focused_pane: SavedFocusTarget,
}

/// Pane IDs are regenerated on restore, so focus is saved by position.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SavedFocusTarget {
    /// Index in depth-first traversal order of the layout tree.
    Tiled(usize),
    /// Index into the tab's floating list.
    Floating(usize),
}

/// Serialized DTO form of the layout tree (Spec-0007 keeps the in-memory
/// model serde-free; this parallel-array shape is the on-disk contract).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SavedLayoutNode {
    Container {
        direction: SavedDirection,
        children: Vec<SavedLayoutNode>,
        weights: Vec<f32>,
    },
    Leaf(SavedPane),
}

/// Serializes to the Spec-0010 `"horizontal"`/`"vertical"` strings while
/// keeping other values unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SavedDirection {
    Horizontal,
    Vertical,
}

impl From<oakterm_mux::SplitDirection> for SavedDirection {
    fn from(d: oakterm_mux::SplitDirection) -> Self {
        match d {
            oakterm_mux::SplitDirection::Horizontal => Self::Horizontal,
            oakterm_mux::SplitDirection::Vertical => Self::Vertical,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedPane {
    pub(crate) cwd: String,
    /// Restored command. `None` restores the default shell. Always `None`
    /// until `restartable_commands` (Spec-0010) is plumbed through config —
    /// restoring arbitrary commands without the allowlist would re-run
    /// scripts the user never marked safe.
    pub(crate) command: Option<String>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedFloatingPane {
    pub(crate) pane: SavedPane,
    pub(crate) x_frac: f32,
    pub(crate) y_frac: f32,
    pub(crate) width_frac: f32,
    pub(crate) height_frac: f32,
    pub(crate) visible: bool,
}

/// Resolve `$OAKTERM_STATE_DIR`, falling back to the Spec-0010 platform
/// default.
pub(crate) fn default_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OAKTERM_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(home).join("Library/Application Support/oakterm")
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
            PathBuf::from(xdg).join("oakterm")
        } else {
            PathBuf::from(home).join(".local/state/oakterm")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        std::env::temp_dir().join("oakterm-state")
    }
}

/// Save the session to `<state_dir>/session.json` (Spec-0010): write to a
/// temporary file in the same directory, then rename, so an interrupted
/// write never corrupts an existing session file.
///
/// # Errors
/// Returns an error if the directory cannot be created, serialization
/// fails, or the write/rename fails. The caller aborts the shutdown on
/// error (ADR-0020).
pub(crate) async fn save_session(
    panes: &Arc<Mutex<PaneManager>>,
    state_dir: &Path,
) -> io::Result<PathBuf> {
    let session = build_session(panes).await?;
    let json = serde_json::to_vec_pretty(&session)?;

    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join("session.json");
    let tmp = state_dir.join("session.json.tmp");
    if let Err(e) = std::fs::write(&tmp, &json).and_then(|()| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    info!(path = %path.display(), panes = session.workspaces[0].tabs[0].pane_count(), "session saved");
    Ok(path)
}

impl SavedTab {
    fn pane_count(&self) -> usize {
        fn leaves(n: &SavedLayoutNode) -> usize {
            match n {
                SavedLayoutNode::Leaf(_) => 1,
                SavedLayoutNode::Container { children, .. } => children.iter().map(leaves).sum(),
            }
        }
        leaves(&self.layout) + self.floating.len()
    }
}

/// Snapshot the manager topology, then read each pane's data one pane
/// lock at a time (manager→pane lock order).
async fn build_session(panes: &Arc<Mutex<PaneManager>>) -> io::Result<SessionFile> {
    let (layout, focused, pane_list) = {
        let pm = panes.lock().await;
        let Some(layout) = pm.layout().cloned() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no panes to save"));
        };
        (layout, pm.focused(), pm.snapshot())
    };

    let mut pane_data: HashMap<u32, SavedPane> = HashMap::new();
    for (id, pane) in pane_list {
        let pane = pane.lock().await;
        let g = pane.screens.active_grid();
        pane_data.insert(
            id,
            SavedPane {
                // Spawn-time cwd; the live OSC 7 cwd is not surfaced from
                // the grid yet (TREK-134).
                cwd: pane.cwd.clone(),
                command: None,
                cols: g.cols,
                rows: g.rows,
                title: g.title.clone().unwrap_or_default(),
            },
        );
    }

    let focused_index = focused
        .and_then(|f| layout.pane_ids().iter().position(|p| p.0 == f))
        .unwrap_or(0);
    let saved_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(SessionFile {
        version: SESSION_VERSION,
        saved_at,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        workspaces: vec![SavedWorkspace {
            name: "default".to_string(),
            tabs: vec![SavedTab {
                name: String::new(),
                layout: to_saved_node(&layout, &pane_data)?,
                floating: Vec::new(),
                focused_pane: SavedFocusTarget::Tiled(focused_index),
            }],
            active_tab: 0,
        }],
        active_workspace: 0,
    })
}

/// Tree and map are snapshotted under one manager lock, so every leaf has
/// pane data (leaves==keys invariant). A miss means the invariant broke —
/// erroring routes it into the loud `save_failed` abort instead of
/// persisting a corrupt 0x0 pane that restore would trip over later.
fn to_saved_node(
    node: &LayoutNode,
    pane_data: &HashMap<u32, SavedPane>,
) -> io::Result<SavedLayoutNode> {
    match node {
        LayoutNode::Leaf(PaneId(id)) => {
            let Some(pane) = pane_data.get(id) else {
                error!(pane_id = id, "layout leaf missing pane data at save");
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("layout leaf {id} has no pane data"),
                ));
            };
            Ok(SavedLayoutNode::Leaf(pane.clone()))
        }
        LayoutNode::Container(c) => Ok(SavedLayoutNode::Container {
            direction: c.direction.into(),
            children: c
                .children
                .iter()
                .map(|ch| to_saved_node(&ch.node, pane_data))
                .collect::<io::Result<Vec<_>>>()?,
            weights: c.children.iter().map(|ch| ch.weight).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_mux::SplitDirection;

    fn manager_with_split() -> Arc<Mutex<PaneManager>> {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), "/tmp".to_string());
        pm.split_create(
            a,
            SplitDirection::Vertical,
            80,
            24,
            String::new(),
            String::new(),
        )
        .unwrap();
        Arc::new(Mutex::new(pm))
    }

    #[tokio::test]
    async fn save_writes_valid_session_json() {
        let panes = manager_with_split();
        // Asymmetric weights so a child/weight order swap is detectable.
        {
            let mut pm = panes.lock().await;
            let ids: Vec<u32> = pm
                .layout()
                .unwrap()
                .pane_ids()
                .iter()
                .map(|p| p.0)
                .collect();
            pm.resize_layout(ids[0], ids[1], 0.2, 0.01).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();

        let path = save_session(&panes, dir.path()).await.unwrap();
        assert_eq!(path, dir.path().join("session.json"));
        assert!(
            !dir.path().join("session.json.tmp").exists(),
            "temp file must be renamed away"
        );

        let json = std::fs::read_to_string(&path).unwrap();
        let session: SessionFile = serde_json::from_str(&json).unwrap();
        assert_eq!(session.version, 1);
        assert_eq!(session.workspaces.len(), 1);
        let tab = &session.workspaces[0].tabs[0];
        assert_eq!(tab.pane_count(), 2);
        // split_create focused the new pane: second leaf in DFS order.
        assert_eq!(tab.focused_pane, SavedFocusTarget::Tiled(1));
        let SavedLayoutNode::Container {
            direction,
            children,
            weights,
        } = &tab.layout
        else {
            panic!("two panes serialize as a container root");
        };
        assert_eq!(*direction, SavedDirection::Vertical);
        assert!(json.contains("\"vertical\""), "spec-0010 direction string");
        assert_eq!(children.len(), 2);
        assert!(
            (weights[0] - 0.7).abs() < 1e-5,
            "first weight follows resize"
        );
        assert!(
            (weights[1] - 0.3).abs() < 1e-5,
            "second weight follows resize"
        );
        let SavedLayoutNode::Leaf(first) = &children[0] else {
            panic!("first child is a leaf");
        };
        assert_eq!(first.cwd, "/tmp");
        assert_eq!((first.cols, first.rows), (80, 24));
        assert!(
            first.command.is_none(),
            "commands need the restartable allowlist"
        );
    }

    #[tokio::test]
    async fn save_into_unwritable_dir_errors() {
        let panes = manager_with_split();
        let dir = tempfile::tempdir().unwrap();
        // A file where the state dir should be makes create_dir_all fail.
        let blocked = dir.path().join("not-a-dir");
        std::fs::write(&blocked, b"x").unwrap();

        assert!(save_session(&panes, &blocked).await.is_err());
    }

    #[tokio::test]
    async fn save_overwrites_previous_session_atomically() {
        let panes = manager_with_split();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.json"), b"old").unwrap();

        save_session(&panes, dir.path()).await.unwrap();
        let json = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
        assert!(json.contains("\"version\""), "old content replaced");
    }
}
