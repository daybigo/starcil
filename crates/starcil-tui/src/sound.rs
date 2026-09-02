//! Background-agent sound policy and injectable playback.

use std::path::{Path, PathBuf};
use std::process::Command;

use starcil_config::{Config, SoundPolicy};
use starcil_domain::AgentStatus;
use starcil_protocol::types::SessionSnapshot;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundCue {
    Done,
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundRequest {
    pub pane_id: String,
    pub agent: String,
    pub cue: SoundCue,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum SoundError {
    #[error("sound playback is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("MP3 playback requires the requested rodio dependency: {0}")]
    Mp3RequiresRodio(PathBuf),
    #[cfg(feature = "mp3")]
    #[error("MP3 playback failed for {path}: {reason}")]
    Mp3Playback { path: PathBuf, reason: String },
    #[error("could not launch PowerShell sound playback: {0}")]
    Launch(#[source] std::io::Error),
    #[error("PowerShell sound playback exited with {0}")]
    Failed(std::process::ExitStatus),
}

pub trait SoundPlayer {
    fn play(&mut self, request: &SoundRequest) -> Result<(), SoundError>;
}

#[derive(Debug, Default)]
pub struct PowerShellSoundPlayer;

impl SoundPlayer for PowerShellSoundPlayer {
    fn play(&mut self, request: &SoundRequest) -> Result<(), SoundError> {
        if !cfg!(windows) {
            return Err(SoundError::UnsupportedPlatform);
        }
        if let Some(path) = request
            .path
            .as_deref()
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
            })
        {
            #[cfg(feature = "mp3")]
            return play_mp3(path);

            #[cfg(not(feature = "mp3"))]
            return Err(SoundError::Mp3RequiresRodio(path.to_owned()));
        }
        let script = if request.path.is_some() {
            "$player = New-Object System.Media.SoundPlayer; \
             $player.SoundLocation = $env:STARCIL_SOUND_PATH; \
             $player.Load(); $player.PlaySync()"
        } else {
            match request.cue {
                SoundCue::Done => "[System.Media.SystemSounds]::Asterisk.Play()",
                SoundCue::Request => "[System.Media.SystemSounds]::Exclamation.Play()",
            }
        };
        let mut command = Command::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script]);
        if let Some(path) = &request.path {
            command.env("STARCIL_SOUND_PATH", path);
        }
        let status = command.status().map_err(SoundError::Launch)?;
        if status.success() {
            Ok(())
        } else {
            Err(SoundError::Failed(status))
        }
    }
}

#[cfg(feature = "mp3")]
fn play_mp3(path: &Path) -> Result<(), SoundError> {
    use std::fs::File;
    use std::io::BufReader;

    let playback_error = |reason: String| SoundError::Mp3Playback {
        path: path.to_owned(),
        reason,
    };
    let (_stream, handle) =
        rodio::OutputStream::try_default().map_err(|error| playback_error(error.to_string()))?;
    let sink = rodio::Sink::try_new(&handle).map_err(|error| playback_error(error.to_string()))?;
    let file = File::open(path).map_err(|error| playback_error(error.to_string()))?;
    let source = rodio::Decoder::new(BufReader::new(file))
        .map_err(|error| playback_error(error.to_string()))?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}

#[derive(Debug)]
pub struct SoundController<P: SoundPlayer> {
    player: P,
}

impl<P: SoundPlayer> SoundController<P> {
    pub fn new(player: P) -> Self {
        Self { player }
    }

    pub fn player(&self) -> &P {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut P {
        &mut self.player
    }

    pub fn play_all(
        &mut self,
        requests: impl IntoIterator<Item = SoundRequest>,
    ) -> Vec<SoundError> {
        requests
            .into_iter()
            .filter_map(|request| self.player.play(&request).err())
            .collect()
    }
}

pub fn request_for_transition(
    config: &Config,
    config_path: Option<&Path>,
    snapshot: &SessionSnapshot,
    pane_id: &str,
    previous: AgentStatus,
    next: AgentStatus,
) -> Option<SoundRequest> {
    if previous == next {
        return None;
    }
    let cue = match next {
        AgentStatus::Done => SoundCue::Done,
        AgentStatus::Blocked => SoundCue::Request,
        _ => return None,
    };
    let pane = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id)?;
    if pane.workspace_id == snapshot.focused_workspace_id {
        return None;
    }
    let agent = pane
        .agent
        .clone()
        .or_else(|| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
                .map(|agent| agent.agent.clone())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let enabled = match config.ui.sound.agents.get(&agent).unwrap_or(SoundPolicy::Default) {
        SoundPolicy::Off => false,
        SoundPolicy::On => true,
        SoundPolicy::Default => config.ui.sound.enabled,
    };
    if !enabled {
        return None;
    }
    let configured_path = match cue {
        SoundCue::Done => config
            .ui
            .sound
            .done_path
            .as_ref()
            .or(config.ui.sound.path.as_ref()),
        SoundCue::Request => config
            .ui
            .sound
            .request_path
            .as_ref()
            .or(config.ui.sound.path.as_ref()),
    };
    let base = config_path.and_then(Path::parent);
    let path = configured_path.map(|value| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.map_or(path.clone(), |base| base.join(path))
        }
    });
    Some(SoundRequest {
        pane_id: pane_id.to_owned(),
        agent,
        cue,
        path,
    })
}
