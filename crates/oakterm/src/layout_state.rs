//! Client-side layout state: the daemon's layout tree, the pixel
//! geometry computed from it, and split-focus bookkeeping.

use tracing::warn;

use oakterm_protocol::message::LayoutTreeNode;

use crate::layout::{self, LayoutGeometry, PixelRect};

/// Layout state for the window, kept apart from `App`'s socket and GPU
/// concerns so render and focus code ask one place about splits.
#[derive(Default)]
pub struct PaneLayout {
    /// Layout tree from the daemon (`GetLayoutTree`). `None` until the
    /// first split; single-pane rendering needs no tree.
    tree: Option<LayoutTreeNode>,
    /// Pixel geometry computed from `tree` for the current window size.
    /// Recomputed on window resize and topology change.
    geometry: Option<LayoutGeometry>,
    /// Pane awaiting focus once its view exists. Focus must not move to a
    /// split's new pane before `LayoutTree` arrives — the render fallback
    /// draws the focused pane, and a viewless focus blanks the window.
    pending_focus: Option<u32>,
}

impl PaneLayout {
    /// Resize handling keys on the tree, not the geometry it produces —
    /// keying on geometry would wedge single-pane mode if a recompute
    /// was ever skipped.
    #[must_use]
    pub fn has_tree(&self) -> bool {
        self.tree.is_some()
    }

    /// The full computed geometry, if any (including a single-leaf tree).
    #[must_use]
    pub fn geometry(&self) -> Option<&LayoutGeometry> {
        self.geometry.as_ref()
    }

    /// The geometry when the window is actually split: `Some` only with
    /// more than one pane. Single-pane rendering uses the fallback path.
    #[must_use]
    pub fn active_geometry(&self) -> Option<&LayoutGeometry> {
        self.geometry.as_ref().filter(|g| g.panes.len() > 1)
    }

    /// Whether output for `pane_id` is on screen: the focused pane always
    /// is; a background pane only while the split geometry contains it.
    /// Callers keep `focused_pane` pointing at a live pane — a focused
    /// pane absent from the split geometry would report visible here yet
    /// not be drawn by [`Self::visible_panes`].
    #[must_use]
    pub fn pane_is_visible(&self, pane_id: u32, focused_pane: u32) -> bool {
        pane_id == focused_pane
            || self
                .active_geometry()
                .is_some_and(|g| g.panes.iter().any(|p| p.pane_id == pane_id))
    }

    /// The panes to draw this frame with their pixel rects: the split
    /// geometry's panes, or the focused pane filling `fallback` when the
    /// window isn't split.
    #[must_use]
    pub fn visible_panes(&self, focused_pane: u32, fallback: PixelRect) -> Vec<(u32, PixelRect)> {
        match self.active_geometry() {
            Some(geo) => geo.panes.iter().map(|p| (p.pane_id, p.rect)).collect(),
            None => vec![(focused_pane, fallback)],
        }
    }

    /// Adopt a layout tree from the daemon, recompute geometry, and
    /// drain the pending focus target for the caller to apply once the
    /// pane's view exists. A target the caller can't apply is dropped,
    /// not retried on the next tree.
    ///
    /// The previous geometry is cleared first: the stale-keep in
    /// [`Self::recompute`] protects same-tree transients, and a
    /// different tree's geometry must not drive rendering or syncs.
    #[must_use]
    pub fn adopt_tree(&mut self, tree: LayoutTreeNode, content: Option<PixelRect>) -> Option<u32> {
        self.tree = Some(tree);
        self.geometry = None;
        self.recompute(content);
        self.pending_focus.take()
    }

    /// Recompute pixel geometry from the stored tree. Stale geometry is
    /// kept (not cleared) when `content` is unavailable (GPU briefly
    /// gone) — vanishing panes are worse than one-frame-stale rects.
    pub fn recompute(&mut self, content: Option<PixelRect>) {
        match (&self.tree, content) {
            (Some(tree), Some(content)) => {
                self.geometry = Some(layout::compute_layout(tree, content));
            }
            (Some(_), None) => warn!("layout geometry not recomputed: gpu unavailable"),
            (None, _) => self.geometry = None,
        }
    }

    /// Record the pane to focus once its view exists (Spec-0007 moves
    /// focus to a split's new pane). Drained by [`Self::adopt_tree`];
    /// latest-wins on rapid splits.
    pub fn set_pending_focus(&mut self, pane_id: u32) {
        self.pending_focus = Some(pane_id);
    }
}

#[cfg(test)]
mod tests {
    use super::PaneLayout;
    use crate::layout::PixelRect;
    use oakterm_protocol::message::{LayoutDirection, LayoutTreeNode};

    const CONTENT: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };

    fn two_pane_tree() -> LayoutTreeNode {
        LayoutTreeNode::Container {
            direction: LayoutDirection::Horizontal,
            children: vec![
                LayoutTreeNode::Leaf { pane_id: 1 },
                LayoutTreeNode::Leaf { pane_id: 2 },
            ],
            weights: vec![0.5, 0.5],
        }
    }

    #[test]
    fn empty_layout_has_no_tree_or_geometry() {
        let layout = PaneLayout::default();
        assert!(!layout.has_tree());
        assert!(layout.geometry().is_none());
        assert!(layout.active_geometry().is_none());
    }

    #[test]
    fn single_leaf_tree_has_geometry_but_no_active_geometry() {
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(LayoutTreeNode::Leaf { pane_id: 1 }, Some(CONTENT));
        assert!(layout.has_tree());
        assert!(layout.geometry().is_some());
        assert!(
            layout.active_geometry().is_none(),
            "one pane is not a split"
        );
    }

    #[test]
    fn split_tree_activates_geometry() {
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(two_pane_tree(), Some(CONTENT));
        let geo = layout.active_geometry().expect("two panes are a split");
        assert_eq!(geo.panes.len(), 2);
    }

    #[test]
    fn recompute_without_content_keeps_stale_geometry() {
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(two_pane_tree(), Some(CONTENT));
        let before = layout.geometry().cloned();
        layout.recompute(None);
        assert_eq!(layout.geometry().cloned(), before);
    }

    #[test]
    fn adopt_tree_without_content_clears_geometry_but_keeps_tree() {
        // A different tree's geometry must not drive rendering or syncs;
        // rendering falls back to the focused pane until a recompute.
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(two_pane_tree(), Some(CONTENT));
        let _ = layout.adopt_tree(LayoutTreeNode::Leaf { pane_id: 3 }, None);
        assert!(layout.has_tree());
        assert!(layout.geometry().is_none());
        assert_eq!(layout.visible_panes(3, CONTENT), vec![(3, CONTENT)]);
    }

    #[test]
    fn visible_panes_uses_fallback_when_not_split() {
        let layout = PaneLayout::default();
        assert_eq!(layout.visible_panes(7, CONTENT), vec![(7, CONTENT)]);
    }

    #[test]
    fn visible_panes_lists_split_geometry() {
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(two_pane_tree(), Some(CONTENT));
        let panes = layout.visible_panes(1, CONTENT);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].0, 1);
        assert_eq!(panes[1].0, 2);
        assert_ne!(panes[0].1, panes[1].1, "split panes get distinct rects");
    }

    #[test]
    fn focused_pane_is_always_visible() {
        let layout = PaneLayout::default();
        assert!(layout.pane_is_visible(7, 7));
        assert!(!layout.pane_is_visible(8, 7));
    }

    #[test]
    fn background_pane_is_visible_only_in_split_geometry() {
        let mut layout = PaneLayout::default();
        let _ = layout.adopt_tree(two_pane_tree(), Some(CONTENT));
        assert!(layout.pane_is_visible(2, 1));
        assert!(!layout.pane_is_visible(9, 1));
    }

    #[test]
    fn adopt_tree_drains_pending_focus_once() {
        let mut layout = PaneLayout::default();
        layout.set_pending_focus(3);
        assert_eq!(layout.adopt_tree(two_pane_tree(), Some(CONTENT)), Some(3));
        assert_eq!(layout.adopt_tree(two_pane_tree(), Some(CONTENT)), None);
    }
}
