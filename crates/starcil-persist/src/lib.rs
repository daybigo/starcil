//! Versioned session persistence and pure restore planning.

mod restore;
mod state;

pub use restore::{
    plan_restore, RestoreBindings, RestoreFocus, RestoreLaunch, RestoreLayout,
    RestoreOptions, RestorePane, RestorePlan, RestoreTab, RestoreWorkspace,
    ResumeFailure, SlotKey,
};
pub use state::{
    backup_path, load, save_atomic, temporary_path, LoadError, LoadOutcome,
    LoadWarning, PaneExtras, SaveError, SessionRef, StateDoc, StateDocError,
    CURRENT_SCHEMA_VERSION,
};
