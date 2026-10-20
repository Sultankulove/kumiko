//! Workbench layout (spec, section 20), modelled on the Classic UI of JetBrains IDEs: a menu bar
//! and a toolbar on top, tool-window stripes on the left, right and bottom edges, a status bar
//! at the bottom. A tool window opens next to its stripe; the editor area takes the rest.
//!
//! The model is plain data: [`LayoutState`] is what is saved per workspace, [`Regions`] is the
//! geometry for a window size. The GPUI view only draws these rectangles, so the layout is
//! tested without a window.

use serde::{Deserialize, Serialize};

pub const MENU_BAR_HEIGHT: f32 = 26.0;
pub const TOOLBAR_HEIGHT: f32 = 32.0;
pub const STRIPE_WIDTH: f32 = 30.0;
pub const BOTTOM_STRIPE_HEIGHT: f32 = 22.0;
pub const STATUS_BAR_HEIGHT: f32 = 24.0;
pub const TAB_BAR_HEIGHT: f32 = 28.0;

const MIN_SIDE_WIDTH: f32 = 120.0;
const MIN_BOTTOM_HEIGHT: f32 = 80.0;
/// Room the editor area keeps when a tool window grows.
const MIN_EDITOR: f32 = 160.0;

/// A tool window: a panel that opens from a stripe button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolWindow {
    Project,
    Problems,
    Output,
    Terminal,
    /// The Commit tool window: views plugins declare (Git changes).
    Commit,
}

/// Which edge a tool window belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Bottom,
}

impl ToolWindow {
    pub const ALL: [ToolWindow; 5] = [
        ToolWindow::Project,
        ToolWindow::Commit,
        ToolWindow::Problems,
        ToolWindow::Output,
        ToolWindow::Terminal,
    ];

    pub fn side(self) -> Side {
        match self {
            ToolWindow::Project | ToolWindow::Commit => Side::Left,
            ToolWindow::Problems | ToolWindow::Output | ToolWindow::Terminal => Side::Bottom,
        }
    }

    /// The l10n key of its title.
    pub fn title_key(self) -> &'static str {
        match self {
            ToolWindow::Project => "toolWindow.project",
            ToolWindow::Problems => "toolWindow.problems",
            ToolWindow::Output => "toolWindow.output",
            ToolWindow::Terminal => "toolWindow.terminal",
            ToolWindow::Commit => "toolWindow.commit",
        }
    }

    /// The icon token of its stripe button (spec, section 21).
    pub fn icon_token(self) -> &'static str {
        match self {
            ToolWindow::Project => "toolwindow.project",
            ToolWindow::Problems => "toolwindow.problems",
            ToolWindow::Output => "toolwindow.output",
            ToolWindow::Terminal => "toolwindow.terminal",
            ToolWindow::Commit => "toolwindow.commit",
        }
    }

    /// The command that shows or hides it.
    pub fn toggle_command(self) -> &'static str {
        match self {
            ToolWindow::Project => "workbench.toggleProject",
            ToolWindow::Problems => "workbench.toggleProblems",
            ToolWindow::Output => "workbench.toggleOutput",
            ToolWindow::Terminal => "workbench.toggleTerminal",
            ToolWindow::Commit => "workbench.toggleCommit",
        }
    }
}

/// The saved layout of a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LayoutState {
    /// The open tool window on the left edge.
    pub left: Option<ToolWindow>,
    /// The open tool window on the bottom edge.
    pub bottom: Option<ToolWindow>,
    pub left_width: f32,
    pub bottom_height: f32,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            left: Some(ToolWindow::Project),
            bottom: None,
            left_width: 280.0,
            bottom_height: 220.0,
        }
    }
}

impl LayoutState {
    pub fn is_open(&self, window: ToolWindow) -> bool {
        match window.side() {
            Side::Left => self.left == Some(window),
            Side::Bottom => self.bottom == Some(window),
        }
    }

    /// Shows the tool window (replacing the one open on its edge) or hides it.
    pub fn toggle(&mut self, window: ToolWindow) {
        let slot = match window.side() {
            Side::Left => &mut self.left,
            Side::Bottom => &mut self.bottom,
        };
        *slot = if *slot == Some(window) {
            None
        } else {
            Some(window)
        };
    }

    /// Geometry of every region for a window of `width` × `height` logical pixels.
    pub fn regions(&self, width: f32, height: f32) -> Regions {
        let width = width.max(0.0);
        let height = height.max(0.0);
        let top = MENU_BAR_HEIGHT + TOOLBAR_HEIGHT;
        let bottom = (height - STATUS_BAR_HEIGHT - BOTTOM_STRIPE_HEIGHT).max(top);
        let band = bottom - top;
        let inner_x = STRIPE_WIDTH;
        let inner_width = (width - 2.0 * STRIPE_WIDTH).max(0.0);

        let bottom_panel = self.bottom.map(|_| {
            let h = clamp(self.bottom_height, MIN_BOTTOM_HEIGHT, band - MIN_EDITOR);
            Rect::new(inner_x, bottom - h, inner_width, h)
        });
        let upper_height = band - bottom_panel.map_or(0.0, |r| r.h);
        let left_panel = self.left.map(|_| {
            let w = clamp(self.left_width, MIN_SIDE_WIDTH, inner_width - MIN_EDITOR);
            Rect::new(inner_x, top, w, upper_height)
        });
        let editor_x = inner_x + left_panel.map_or(0.0, |r| r.w);
        Regions {
            menu_bar: Rect::new(0.0, 0.0, width, MENU_BAR_HEIGHT),
            toolbar: Rect::new(0.0, MENU_BAR_HEIGHT, width, TOOLBAR_HEIGHT),
            left_stripe: Rect::new(0.0, top, STRIPE_WIDTH, band),
            right_stripe: Rect::new((width - STRIPE_WIDTH).max(0.0), top, STRIPE_WIDTH, band),
            bottom_stripe: Rect::new(0.0, bottom, width, BOTTOM_STRIPE_HEIGHT),
            status_bar: Rect::new(0.0, bottom + BOTTOM_STRIPE_HEIGHT, width, STATUS_BAR_HEIGHT),
            left_panel,
            bottom_panel,
            editor: Rect::new(
                editor_x,
                top,
                (inner_x + inner_width - editor_x).max(0.0),
                upper_height,
            ),
        }
    }
}

/// `value` within `[min, max]`; when the room is smaller than `min`, the room.
fn clamp(value: f32, min: f32, max: f32) -> f32 {
    value.min(max).max(min.min(max)).max(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Where each part of the workbench is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Regions {
    pub menu_bar: Rect,
    pub toolbar: Rect,
    pub left_stripe: Rect,
    pub right_stripe: Rect,
    pub bottom_stripe: Rect,
    pub status_bar: Rect,
    pub left_panel: Option<Rect>,
    pub bottom_panel: Option<Rect>,
    pub editor: Rect,
}

impl Regions {
    /// One line per region, `name x y w h`: the layout snapshot of the tests.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        let mut line = |name: &str, rect: Option<Rect>| {
            if let Some(r) = rect {
                out.push_str(&format!(
                    "{name:<13} {:>6} {:>6} {:>6} {:>6}\n",
                    r.x, r.y, r.w, r.h
                ));
            }
        };
        line("menu_bar", Some(self.menu_bar));
        line("toolbar", Some(self.toolbar));
        line("left_stripe", Some(self.left_stripe));
        line("left_panel", self.left_panel);
        line("editor", Some(self.editor));
        line("right_stripe", Some(self.right_stripe));
        line("bottom_panel", self.bottom_panel);
        line("bottom_stripe", Some(self.bottom_stripe));
        line("status_bar", Some(self.status_bar));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_snapshot() {
        let regions = LayoutState::default().regions(1280.0, 800.0);
        assert_eq!(
            regions.describe(),
            "\
menu_bar           0      0   1280     26
toolbar            0     26   1280     32
left_stripe        0     58     30    696
left_panel        30     58    280    696
editor           310     58    940    696
right_stripe    1250     58     30    696
bottom_stripe      0    754   1280     22
status_bar         0    776   1280     24
"
        );
    }

    #[test]
    fn bottom_panel_spans_the_width_between_stripes() {
        let mut state = LayoutState::default();
        state.toggle(ToolWindow::Problems);
        let regions = state.regions(1280.0, 800.0);
        assert_eq!(
            regions.bottom_panel,
            Some(Rect::new(30.0, 534.0, 1220.0, 220.0))
        );
        assert_eq!(
            regions.left_panel,
            Some(Rect::new(30.0, 58.0, 280.0, 476.0))
        );
        assert_eq!(regions.editor, Rect::new(310.0, 58.0, 940.0, 476.0));
    }

    #[test]
    fn toggling_replaces_the_window_on_the_same_edge() {
        let mut state = LayoutState::default();
        state.toggle(ToolWindow::Problems);
        state.toggle(ToolWindow::Output);
        assert!(state.is_open(ToolWindow::Output));
        assert!(!state.is_open(ToolWindow::Problems));
        assert!(state.is_open(ToolWindow::Project));
        state.toggle(ToolWindow::Output);
        state.toggle(ToolWindow::Project);
        assert_eq!((state.left, state.bottom), (None, None));
        let regions = state.regions(1280.0, 800.0);
        assert_eq!(regions.editor, Rect::new(30.0, 58.0, 1220.0, 696.0));
    }

    #[test]
    fn panels_leave_room_for_the_editor_and_tiny_windows_do_not_break() {
        let state = LayoutState {
            left_width: 5000.0,
            bottom: Some(ToolWindow::Output),
            bottom_height: 5000.0,
            ..LayoutState::default()
        };
        let regions = state.regions(1000.0, 700.0);
        assert_eq!(regions.editor.w, MIN_EDITOR);
        assert_eq!(regions.editor.h, MIN_EDITOR);
        for (w, h) in [(0.0, 0.0), (50.0, 30.0), (-10.0, 200.0)] {
            let regions = state.regions(w, h);
            for rect in [regions.editor, regions.left_stripe, regions.status_bar] {
                assert!(rect.w >= 0.0 && rect.h >= 0.0, "{w}x{h}: {rect:?}");
            }
        }
    }

    #[test]
    fn state_round_trips_and_tolerates_missing_fields() {
        let state = LayoutState {
            bottom: Some(ToolWindow::Problems),
            ..LayoutState::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<LayoutState>(&json).unwrap(), state);
        let partial: LayoutState = serde_json::from_str(r#"{"left":null}"#).unwrap();
        assert_eq!(partial.left, None);
        assert_eq!(partial.left_width, 280.0);
    }
}
