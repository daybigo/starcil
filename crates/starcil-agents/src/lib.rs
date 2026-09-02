//! Manifest-driven agent detection and lifecycle state machines.

mod kind;
mod integration;
mod lifecycle;
mod manifest;
mod wait;

pub use kind::{AgentKind, ParseAgentKindError};
pub use integration::{
    integration_for_kind, integration_spec, IntegrationRole, IntegrationSpec, ResumeCommand,
    ResumeTemplate, INTEGRATIONS,
};
pub use lifecycle::{
    Clock, DecisionAuthority, DetectionSnapshot, EvaluationInput, FallbackEvidence,
    IntegrationEvidence, IntegrationReport, LifecycleConfig, LifecycleDecision, LifecycleEngine,
    LifecycleExplanation, LifecycleState, ProcessInfo, ReportAcceptance, ReportedState,
    RuleEvidence, SystemClock,
};
pub use manifest::{
    AgentDefinition, AgentDetection, AgentDetectionSource, AgentManifest, CompiledManifest,
    DetectionManifest, ManifestError, ManifestMetadata, ManifestSource, MatcherKind,
    RemoteUpdateStatus, ScreenMatch, ScreenRule, ScreenState, DETECTION_MANIFEST_SCHEMA_VERSION,
    DETECTION_ROWS,
};
pub use wait::{AgentWait, PromptWait, WaitConfig, WaitOutcome};
