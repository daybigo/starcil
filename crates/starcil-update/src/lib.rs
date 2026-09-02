//! Release discovery, verified staging, and restart-required binary swaps.

mod channel;
mod http;
mod updater;

pub use channel::{Channel, ParseChannelError, Platform};
pub use http::{HttpClient, HttpError, HttpRequest, HttpResponse, UreqHttpClient};
pub use updater::{
    apply, default_repo_slug, ApplyOutcome, ReleaseAsset, ReleaseInfo, StagedUpdate,
    UpdateConfig, UpdateError, Updater, CHECKSUM_ASSET, DEFAULT_REPOSITORY,
};
