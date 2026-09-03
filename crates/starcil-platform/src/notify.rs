//! Desktop notifications through what each OS already ships, no daemon
//! library and no long-lived connection: a WinRT toast raised by Windows
//! PowerShell, `terminal-notifier` or `osascript` on macOS, `notify-send`
//! on Linux. Each call is one short-lived process, so callers run it off
//! the render thread.

use std::io;
use std::process::{Command, Stdio};

/// Show `title` / `body` through the OS notification service. `Ok(false)`
/// means no notifier exists on this machine (nothing was shown, nothing
/// failed); `Err` is a notifier that could not be started.
pub fn show_desktop_notification(title: &str, body: &str) -> io::Result<bool> {
    platform::show(title, body)
}

fn run(mut command: Command) -> io::Result<bool> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.status() {
        Ok(status) => Ok(status.success()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Windows PowerShell's own AppUserModelID: toasts need a registered app
    /// to show under, and this one exists on every Windows install.
    const POWERSHELL_APP_ID: &str =
        r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

    /// The text travels in environment variables, never inside the script:
    /// nothing to quote, nothing to inject.
    const TOAST_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
$null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$text = $xml.GetElementsByTagName('text')
$null = $text.Item(0).AppendChild($xml.CreateTextNode($env:STARCIL_NOTIFY_TITLE))
$null = $text.Item(1).AppendChild($xml.CreateTextNode($env:STARCIL_NOTIFY_BODY))
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:STARCIL_NOTIFY_APP_ID).Show($toast)
"#;

    pub(super) fn show(title: &str, body: &str) -> std::io::Result<bool> {
        // Windows PowerShell 5.1 on purpose: it projects WinRT types out of
        // the box, PowerShell 7 does not.
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                TOAST_SCRIPT,
            ])
            .env("STARCIL_NOTIFY_TITLE", title)
            .env("STARCIL_NOTIFY_BODY", body)
            .env("STARCIL_NOTIFY_APP_ID", POWERSHELL_APP_ID)
            .creation_flags(CREATE_NO_WINDOW);
        super::run(command)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    /// `terminal-notifier` (Homebrew) first: its notification can bring the
    /// terminal back when clicked. Without it, `osascript`'s `display
    /// notification`, which shows under Script Editor and activates nothing.
    pub(super) fn show(title: &str, body: &str) -> std::io::Result<bool> {
        let mut notifier = Command::new("terminal-notifier");
        notifier.args(["-title", title, "-message", body]);
        // macOS terminals stamp their bundle id on their children: that is
        // the app to bring back.
        if let Ok(bundle) = std::env::var("__CFBundleIdentifier") {
            if !bundle.trim().is_empty() {
                notifier.args(["-activate", bundle.trim()]);
            }
        }
        if super::run(notifier)? {
            return Ok(true);
        }
        let mut script = Command::new("/usr/bin/osascript");
        script.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ]);
        super::run(script)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::process::Command;

    pub(super) fn show(title: &str, body: &str) -> std::io::Result<bool> {
        // No display, no notification daemon to talk to (ssh, a container).
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
        {
            return Ok(false);
        }
        let mut command = Command::new("notify-send");
        command.args(["--app-name=starcil", "--", title, body]);
        super::run(command)
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub(super) fn show(_title: &str, _body: &str) -> std::io::Result<bool> {
        Ok(false)
    }
}
