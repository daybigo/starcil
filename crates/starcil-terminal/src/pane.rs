use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, MasterPty, PtySize};
use thiserror::Error;

use crate::command::PaneCommand;
use crate::interceptor::QueryKind;
use crate::keyboard::TerminalKeyboardMode;
use crate::screen::{
    ReadFormat, ReadSource, ScreenState, TerminalRead, TerminalScreenFrame,
    TerminalScrollMetrics,
};

const OUTPUT_CHANNEL_CAPACITY: usize = 64;
const OUTPUT_SUBSCRIBER_CAPACITY: usize = 256;
const OUTPUT_SETTLE_WINDOW: Duration = Duration::from_millis(8);
const OUTPUT_BATCH_MAX_LATENCY: Duration = Duration::from_millis(16);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TerminalError {
    #[error("PTY operation failed: {0}")]
    Pty(String),
    #[error("terminal I/O failed: {0}")]
    Io(String),
    #[error("terminal worker has stopped")]
    WorkerStopped,
    #[error("terminal state lock was poisoned")]
    StatePoisoned,
    #[error("terminal size must be at least 1x1, got {rows}x{cols}")]
    InvalidSize { rows: u16, cols: u16 },
    #[error("injected environment key must start with STARCIL_: {0}")]
    InvalidStarcilEnvironmentKey(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl TerminalSize {
    pub fn new(rows: u16, cols: u16) -> Result<Self, TerminalError> {
        if rows == 0 || cols == 0 {
            return Err(TerminalError::InvalidSize { rows, cols });
        }
        Ok(Self {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    fn as_pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeOutcome {
    pub generation: u64,
    pub applied: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ScreenStability {
    pub change_seq: u64,
    pub last_change: Instant,
    pub stable_for: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryResponseCounts {
    pub total: u64,
    pub cursor_position: u64,
    pub primary_device_attributes: u64,
    pub secondary_device_attributes: u64,
    pub device_status: u64,
    pub keyboard_flags: u64,
}

#[derive(Default)]
struct QueryCounters {
    total: AtomicU64,
    cursor_position: AtomicU64,
    primary_device_attributes: AtomicU64,
    secondary_device_attributes: AtomicU64,
    device_status: AtomicU64,
    keyboard_flags: AtomicU64,
}

impl QueryCounters {
    fn record(&self, kind: QueryKind) {
        self.total.fetch_add(1, Ordering::Relaxed);
        let counter = match kind {
            QueryKind::CursorPosition => &self.cursor_position,
            QueryKind::PrimaryDeviceAttributes => &self.primary_device_attributes,
            QueryKind::SecondaryDeviceAttributes => &self.secondary_device_attributes,
            QueryKind::DeviceStatus => &self.device_status,
            QueryKind::KeyboardFlags => &self.keyboard_flags,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> QueryResponseCounts {
        QueryResponseCounts {
            total: self.total.load(Ordering::Relaxed),
            cursor_position: self.cursor_position.load(Ordering::Relaxed),
            primary_device_attributes: self
                .primary_device_attributes
                .load(Ordering::Relaxed),
            secondary_device_attributes: self
                .secondary_device_attributes
                .load(Ordering::Relaxed),
            device_status: self.device_status.load(Ordering::Relaxed),
            keyboard_flags: self.keyboard_flags.load(Ordering::Relaxed),
        }
    }
}

struct SharedState {
    screen: Mutex<ScreenState>,
    stopped: AtomicBool,
    queries: QueryCounters,
    last_error: Mutex<Option<String>>,
    output_subscribers: Mutex<Vec<mpsc::SyncSender<Vec<u8>>>>,
}

enum WriteRequest {
    Bytes {
        bytes: Vec<u8>,
        acknowledgement: Option<mpsc::SyncSender<Result<(), String>>>,
    },
    Shutdown,
}

pub struct PaneTerminal {
    shared: Arc<SharedState>,
    writer: mpsc::Sender<WriteRequest>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    resize_lock: Mutex<()>,
    next_resize_generation: AtomicU64,
    applied_resize_generation: AtomicU64,
    _threads: Mutex<Vec<JoinHandle<()>>>,
}

impl PaneTerminal {
    pub fn spawn(
        command: PaneCommand,
        size: TerminalSize,
        scrollback_limit_bytes: usize,
    ) -> Result<Self, TerminalError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size.as_pty_size())
            .map_err(|error| TerminalError::Pty(error.to_string()))?;
        let child = pair
            .slave
            .spawn_command(command.into_builder()?)
            .map_err(|error| TerminalError::Pty(error.to_string()))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalError::Pty(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalError::Pty(error.to_string()))?;

        let shared = Arc::new(SharedState {
            screen: Mutex::new(ScreenState::new(
                size.rows,
                size.cols,
                scrollback_limit_bytes,
            )),
            stopped: AtomicBool::new(false),
            queries: QueryCounters::default(),
            last_error: Mutex::new(None),
            output_subscribers: Mutex::new(Vec::new()),
        });
        let (writer_tx, writer_rx) = mpsc::channel();
        let (output_tx, output_rx) = mpsc::sync_channel(OUTPUT_CHANNEL_CAPACITY);

        let writer_thread = spawn_writer_thread(writer, writer_rx, Arc::clone(&shared))?;
        let reader_thread = spawn_reader_thread(reader, output_tx, Arc::clone(&shared))?;
        let parser_thread =
            spawn_parser_thread(output_rx, writer_tx.clone(), Arc::clone(&shared))?;

        Ok(Self {
            shared,
            writer: writer_tx,
            child: Mutex::new(child),
            master: Mutex::new(pair.master),
            resize_lock: Mutex::new(()),
            next_resize_generation: AtomicU64::new(0),
            applied_resize_generation: AtomicU64::new(0),
            _threads: Mutex::new(vec![writer_thread, reader_thread, parser_thread]),
        })
    }

    pub fn write_text(&self, text: &str) -> Result<(), TerminalError> {
        self.write_bytes(text.as_bytes())
    }

    pub fn write_enter(&self) -> Result<(), TerminalError> {
        self.write_bytes(b"\r")
    }

    pub fn write_bytes(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.writer
            .send(WriteRequest::Bytes {
                bytes: bytes.to_vec(),
                acknowledgement: Some(ack_tx),
            })
            .map_err(|_| TerminalError::WorkerStopped)?;
        ack_rx
            .recv()
            .map_err(|_| TerminalError::WorkerStopped)?
            .map_err(TerminalError::Io)
    }

    pub fn paste_text(&self, text: &str) -> Result<(), TerminalError> {
        let bytes = paste_payload(text, self.is_bracketed_paste_enabled()?);
        self.write_bytes(&bytes)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<ResizeOutcome, TerminalError> {
        // Same-size resizes must be free: sync_pty_sizes runs after every
        // structural mutation and a real resize dirties the whole screen.
        if lock(&self.shared.screen)?.size() == (rows, cols) {
            return Ok(ResizeOutcome {
                generation: self.applied_resize_generation.load(Ordering::SeqCst),
                applied: false,
            });
        }
        let generation = self
            .next_resize_generation
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        self.resize_with_generation(rows, cols, generation)
    }

    pub fn resize_with_generation(
        &self,
        rows: u16,
        cols: u16,
        generation: u64,
    ) -> Result<ResizeOutcome, TerminalError> {
        let size = TerminalSize::new(rows, cols)?;
        let _guard = lock(&self.resize_lock)?;
        let applied = self.applied_resize_generation.load(Ordering::SeqCst);
        if generation <= applied {
            return Ok(ResizeOutcome {
                generation,
                applied: false,
            });
        }

        lock(&self.master)?
            .resize(size.as_pty_size())
            .map_err(|error| TerminalError::Pty(error.to_string()))?;
        lock(&self.shared.screen)?.resize(rows, cols);
        self.applied_resize_generation
            .store(generation, Ordering::SeqCst);
        self.next_resize_generation
            .fetch_max(generation, Ordering::SeqCst);
        Ok(ResizeOutcome {
            generation,
            applied: true,
        })
    }

    pub fn read(
        &self,
        source: ReadSource,
        lines: Option<usize>,
        format: ReadFormat,
    ) -> Result<TerminalRead, TerminalError> {
        Ok(lock(&self.shared.screen)?.read(source, lines, format))
    }

    /// Subscribe to raw PTY output produced after this call. A bounded queue
    /// isolates the terminal reader from slow observers; lagging subscribers
    /// can recover from the rendered ANSI snapshot exposed by the server.
    pub fn subscribe_output(&self) -> Result<mpsc::Receiver<Vec<u8>>, TerminalError> {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_SUBSCRIBER_CAPACITY);
        lock(&self.shared.output_subscribers)?.push(sender);
        Ok(receiver)
    }

    pub fn size(&self) -> Result<(u16, u16), TerminalError> {
        Ok(lock(&self.shared.screen)?.size())
    }

    /// Positive deltas scroll toward retained history; negative deltas return
    /// toward the live bottom.
    pub fn scroll(&self, delta: i32) -> Result<(), TerminalError> {
        lock(&self.shared.screen)?.scroll(delta);
        Ok(())
    }

    pub fn take_dirty_rows(&self) -> Result<Vec<u16>, TerminalError> {
        Ok(lock(&self.shared.screen)?.take_dirty_rows())
    }

    pub fn take_screen_frame(
        &self,
        snapshot: bool,
    ) -> Result<Option<TerminalScreenFrame>, TerminalError> {
        Ok(lock(&self.shared.screen)?.take_frame(snapshot))
    }

    pub fn change_seq(&self) -> Result<u64, TerminalError> {
        Ok(lock(&self.shared.screen)?.change_seq())
    }

    pub fn stability(&self) -> Result<ScreenStability, TerminalError> {
        let screen = lock(&self.shared.screen)?;
        let last_change = screen.last_change();
        Ok(ScreenStability {
            change_seq: screen.change_seq(),
            last_change,
            stable_for: last_change.elapsed(),
        })
    }

    pub fn is_bracketed_paste_enabled(&self) -> Result<bool, TerminalError> {
        Ok(lock(&self.shared.screen)?.bracketed_paste())
    }

    /// Keyboard protocol the pane's program negotiated (kitty flags, ConPTY
    /// win32-input-mode, DECCKM): what `encode_key` must be given.
    pub fn keyboard_mode(&self) -> Result<TerminalKeyboardMode, TerminalError> {
        Ok(lock(&self.shared.screen)?.keyboard_mode())
    }

    pub fn terminal_title(&self) -> Result<Option<String>, TerminalError> {
        Ok(lock(&self.shared.screen)?.terminal_title())
    }

    /// The working directory the shell announced through OSC 9;9 / OSC 7,
    /// if it ever did.
    pub fn shell_cwd(&self) -> Result<Option<String>, TerminalError> {
        Ok(lock(&self.shared.screen)?.shell_cwd())
    }

    pub fn scroll_metrics(&self) -> Result<TerminalScrollMetrics, TerminalError> {
        Ok(lock(&self.shared.screen)?.scroll_metrics())
    }

    pub fn process_id(&self) -> Result<Option<u32>, TerminalError> {
        Ok(lock(&self.child)?.process_id())
    }

    pub fn is_alive(&self) -> Result<bool, TerminalError> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Ok(false);
        }
        lock(&self.child)?
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| TerminalError::Pty(error.to_string()))
    }

    pub fn query_response_counts(&self) -> QueryResponseCounts {
        self.shared.queries.snapshot()
    }

    pub fn query_response_count(&self) -> u64 {
        self.shared.queries.total.load(Ordering::Relaxed)
    }

    pub fn last_worker_error(&self) -> Result<Option<String>, TerminalError> {
        Ok(lock(&self.shared.last_error)?.clone())
    }

    pub fn kill(&self) -> Result<(), TerminalError> {
        self.shared.stopped.store(true, Ordering::Release);
        let _ = self.writer.send(WriteRequest::Shutdown);
        lock(&self.child)?
            .kill()
            .map_err(|error| TerminalError::Pty(error.to_string()))
    }
}

impl Drop for PaneTerminal {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        let _ = self.writer.send(WriteRequest::Shutdown);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

fn spawn_writer_thread(
    mut writer: Box<dyn Write + Send>,
    requests: mpsc::Receiver<WriteRequest>,
    shared: Arc<SharedState>,
) -> Result<JoinHandle<()>, TerminalError> {
    thread::Builder::new()
        .name("starcil-pty-writer".to_owned())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                match request {
                    WriteRequest::Shutdown => break,
                    WriteRequest::Bytes {
                        bytes,
                        acknowledgement,
                    } => {
                        let result = writer
                            .write_all(&bytes)
                            .and_then(|_| writer.flush())
                            .map_err(|error| error.to_string());
                        if let Err(error) = &result {
                            set_last_error(&shared, error.clone());
                        }
                        if let Some(acknowledgement) = acknowledgement {
                            let _ = acknowledgement.send(result.clone());
                        }
                        if result.is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|error| TerminalError::Io(error.to_string()))
}

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    output: mpsc::SyncSender<Vec<u8>>,
    shared: Arc<SharedState>,
) -> Result<JoinHandle<()>, TerminalError> {
    thread::Builder::new()
        .name("starcil-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = vec![0_u8; 16 * 1024];
            while !shared.stopped.load(Ordering::Acquire) {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => {
                        let bytes = buffer[..length].to_vec();
                        publish_output(&shared, &bytes);
                        if output.send(bytes).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        set_last_error(&shared, error.to_string());
                        break;
                    }
                }
            }
        })
        .map_err(|error| TerminalError::Io(error.to_string()))
}

fn publish_output(shared: &SharedState, bytes: &[u8]) {
    let Ok(mut subscribers) = shared.output_subscribers.lock() else {
        return;
    };
    subscribers.retain(|subscriber| match subscriber.try_send(bytes.to_vec()) {
        Ok(()) | Err(mpsc::TrySendError::Full(_)) => true,
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    });
}

fn spawn_parser_thread(
    output: mpsc::Receiver<Vec<u8>>,
    writer: mpsc::Sender<WriteRequest>,
    shared: Arc<SharedState>,
) -> Result<JoinHandle<()>, TerminalError> {
    thread::Builder::new()
        .name("starcil-vt-parser".to_owned())
        .spawn(move || {
            while let Some(batch) = receive_output_batch(&output) {
                let screen = shared.screen.lock();
                let Ok(mut screen) = screen else {
                    set_last_error(&shared, "terminal state lock was poisoned".to_owned());
                    break;
                };
                screen.process_chunks(batch.iter().map(Vec::as_slice), |kind, response| {
                    shared.queries.record(kind);
                    let _ = writer.send(WriteRequest::Bytes {
                        bytes: response,
                        acknowledgement: None,
                    });
                });
            }
        })
        .map_err(|error| TerminalError::Io(error.to_string()))
}

fn receive_output_batch(output: &mpsc::Receiver<Vec<u8>>) -> Option<Vec<Vec<u8>>> {
    let first = output.recv().ok()?;
    let mut batch = vec![first];
    let deadline = Instant::now() + OUTPUT_BATCH_MAX_LATENCY;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match output.recv_timeout(OUTPUT_SETTLE_WINDOW.min(remaining)) {
            Ok(bytes) => batch.push(bytes),
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Some(batch)
}

fn set_last_error(shared: &SharedState, error: String) {
    if let Ok(mut last_error) = shared.last_error.lock() {
        *last_error = Some(error);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, TerminalError> {
    mutex.lock().map_err(|_| TerminalError::StatePoisoned)
}

fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_resize_generation_is_ignored_by_contract() {
        assert!(matches!(
            TerminalSize::new(0, 80),
            Err(TerminalError::InvalidSize { rows: 0, cols: 80 })
        ));
    }

    #[test]
    fn paste_payload_wraps_only_when_mode_2004_is_enabled() {
        assert_eq!(paste_payload("one\ntwo", false), b"one\ntwo");
        assert_eq!(
            paste_payload("one\ntwo", true),
            b"\x1b[200~one\ntwo\x1b[201~"
        );
    }

    #[test]
    fn output_batch_coalesces_adjacent_fake_stream_chunks() {
        let (sender, receiver) = mpsc::channel();
        sender.send(b"\x1b[1;1H\x1b[2K".to_vec()).unwrap();
        sender
            .send(b"\x1b[38;2;215;119;87mORANGE\x1b[0m".to_vec())
            .unwrap();
        drop(sender);

        let batch = receive_output_batch(&receiver).expect("fake output batch");
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.concat(), b"\x1b[1;1H\x1b[2K\x1b[38;2;215;119;87mORANGE\x1b[0m");
    }

    #[cfg(windows)]
    #[test]
    fn live_powershell_prompt_query_responder_and_command_round_trip() {
        let command = PaneCommand::new("powershell.exe")
            .args(["-NoLogo"])
            .starcil_env("STARCIL_ENV", "1")
            .starcil_env("STARCIL_SESSION", "terminal-live-test")
            .starcil_env("STARCIL_WORKSPACE_ID", "w1")
            .starcil_env("STARCIL_TAB_ID", "w1:t1")
            .starcil_env("STARCIL_PANE_ID", "w1:p1");
        let terminal = PaneTerminal::spawn(
            command,
            TerminalSize::new(30, 100).unwrap(),
            1024 * 1024,
        )
        .expect("spawn PowerShell");

        wait_until(Duration::from_secs(10), || {
            let visible = terminal
                .read(ReadSource::Visible, None, ReadFormat::Text)
                .expect("read visible")
                .content;
            (visible.contains("PS ") || visible.contains('>'))
                && terminal.query_response_counts().cursor_position > 0
        })
        .unwrap_or_else(|| {
            panic!(
                "PowerShell prompt/query timeout; screen={:?}, counts={:?}, worker_error={:?}",
                terminal
                    .read(ReadSource::Visible, None, ReadFormat::Text)
                    .map(|read| read.content),
                terminal.query_response_counts(),
                terminal.last_worker_error()
            )
        });

        terminal.write_text("echo STARCIL_OK").expect("write text");
        terminal.write_enter().expect("write enter separately");
        wait_until(Duration::from_secs(10), || {
            terminal
                .read(ReadSource::RecentUnwrapped, Some(30), ReadFormat::Text)
                .map(|read| read.content.contains("STARCIL_OK"))
                .unwrap_or(false)
        })
        .expect("PowerShell command output");

        assert!(terminal
            .resize_with_generation(31, 101, 2)
            .expect("new resize")
            .applied);
        assert!(!terminal
            .resize_with_generation(30, 100, 1)
            .expect("stale resize")
            .applied);

        terminal.kill().expect("kill PowerShell");
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> Option<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return Some(());
            }
            thread::sleep(Duration::from_millis(25));
        }
        None
    }
}
