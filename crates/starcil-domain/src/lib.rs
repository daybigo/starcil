//! starcil-domain — pure model: ids, split tree, session hierarchy.
//! No I/O, no async, fully unit-tested. Everything the server mutates and the
//! clients render is defined here.

pub mod ids;
pub mod model;
pub mod tree;

pub use ids::{AnyId, IdParseError, PaneId, TabId, WorkspaceId};
pub use model::{AgentStatus, ClosedPane, ModelError, PaneMeta, SessionModel, Tab, Workspace};
pub use tree::{Axis, Direction, Edges, Node, Rect, SplitDirection};
