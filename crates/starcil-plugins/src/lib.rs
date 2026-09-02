//! Starcil plugin manifests, registry, command preparation, execution, and logs.

mod error;
mod logs;
mod manifest;
mod registry;
mod runtime;

pub use error::{PluginError, PluginResult};
pub use logs::{CommandKind, CommandLog, CommandState, LogStore};
pub use manifest::{
    load_manifest, ActionSpec, BuildSpec, EventHookSpec, LinkHandlerSpec, ManifestValidator,
    LoadedManifest, PaneDimension, PanePlacement, PaneSpec, Platform, PluginManifest, StartupSpec,
    ValidationReport, STARCIL_PLUGIN_MANIFEST,
};
pub use registry::{
    GithubSourceMetadata, PluginEntry, PluginRegistry, RegistryPaths, SourceMetadata,
};
pub use runtime::{
    build_invocation_context, ActionInfo, ActionInvocation, ActiveContext, HostEnvironment,
    PaneOpenOptions, PluginExecutor, PreparedCommand, PreparedPane,
};
