//! Multiplexer domain model (Spec-0007, ADR-0010).
//!
//! Pure data model for the pane hierarchy — the split-tree layout, and later
//! tabs and workspaces. The daemon owns this state and drives it in response
//! to wire-protocol messages; keeping it in its own crate lets the tree logic
//! be unit-tested without the daemon's async and socket machinery.

pub mod geometry;
pub mod layout;
pub mod ops;

pub use geometry::{BorderExtents, SplitPreview};
pub use layout::{Child, Container, InvariantViolation, LayoutNode, PaneId, SplitDirection};
pub use ops::{CloseOutcome, LayoutError};
