use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use starcil_platform::{Transport, TransportError};
use thiserror::Error;

pub const IMAGE_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_ID_BYTES: usize = 256;
static NEXT_IMAGE_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImageBridgeFrame {
    Paste { image_paste: ImagePaste },
    Chunk { image_chunk: ImageChunk },
    End { image_end: ImageEnd },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePaste {
    pub id: String,
    pub format: ImageFormat,
    pub total_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageChunk {
    pub id: String,
    pub index: u32,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEnd {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
}

/// Sends one PNG as a declaration, ordered 256 KiB chunks, and an end frame.
pub async fn send_image<T>(
    transport: &mut T,
    id: &str,
    png_bytes: &[u8],
) -> Result<(), ImageBridgeError>
where
    T: Transport + ?Sized,
{
    validate_image_id(id)?;
    if png_bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageBridgeError::Oversize {
            declared: png_bytes.len() as u64,
            max: MAX_IMAGE_BYTES,
        });
    }
    let digest = sha256_hex(png_bytes);
    send_frame(
        transport,
        ImageBridgeFrame::Paste {
            image_paste: ImagePaste {
                id: id.to_owned(),
                format: ImageFormat::Png,
                total_bytes: png_bytes.len() as u64,
                sha256: digest,
            },
        },
    )
    .await?;
    for (index, chunk) in png_bytes.chunks(IMAGE_CHUNK_BYTES).enumerate() {
        send_frame(
            transport,
            ImageBridgeFrame::Chunk {
                image_chunk: ImageChunk {
                    id: id.to_owned(),
                    index: u32::try_from(index).expect("32 MiB has fewer than u32 chunks"),
                    data_base64: encode_base64(chunk),
                },
            },
        )
        .await?;
    }
    send_frame(
        transport,
        ImageBridgeFrame::End {
            image_end: ImageEnd { id: id.to_owned() },
        },
    )
    .await
}

pub async fn send_image_file<T>(
    transport: &mut T,
    id: &str,
    png_path: &Path,
) -> Result<(), ImageBridgeError>
where
    T: Transport + ?Sized,
{
    let metadata = tokio::fs::metadata(png_path)
        .await
        .map_err(|source| ImageBridgeError::Io {
            path: png_path.to_owned(),
            source,
        })?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(ImageBridgeError::Oversize {
            declared: metadata.len(),
            max: MAX_IMAGE_BYTES,
        });
    }
    let bytes = tokio::fs::read(png_path)
        .await
        .map_err(|source| ImageBridgeError::Io {
            path: png_path.to_owned(),
            source,
        })?;
    send_image(transport, id, &bytes).await
}

async fn send_frame<T>(
    transport: &mut T,
    frame: ImageBridgeFrame,
) -> Result<(), ImageBridgeError>
where
    T: Transport + ?Sized,
{
    transport.send(serde_json::to_value(frame)?).await?;
    Ok(())
}

pub struct ImageReceiver {
    runtime_dir: PathBuf,
    active: Option<ActiveImage>,
}

impl ImageReceiver {
    pub fn new(runtime_dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: runtime_dir.into(),
            active: None,
        }
    }

    pub fn receive_value(&mut self, value: Value) -> Result<Option<PathBuf>, ImageBridgeError> {
        self.receive(serde_json::from_value(value)?)
    }

    pub fn receive(
        &mut self,
        frame: ImageBridgeFrame,
    ) -> Result<Option<PathBuf>, ImageBridgeError> {
        match frame {
            ImageBridgeFrame::Paste { image_paste } => {
                self.begin(image_paste)?;
                Ok(None)
            }
            ImageBridgeFrame::Chunk { image_chunk } => {
                self.chunk(image_chunk)?;
                Ok(None)
            }
            ImageBridgeFrame::End { image_end } => self.finish(image_end),
        }
    }

    fn begin(&mut self, paste: ImagePaste) -> Result<(), ImageBridgeError> {
        self.abort_active();
        validate_image_id(&paste.id)?;
        if paste.total_bytes > MAX_IMAGE_BYTES {
            return Err(ImageBridgeError::Oversize {
                declared: paste.total_bytes,
                max: MAX_IMAGE_BYTES,
            });
        }
        let expected_sha256 = normalize_sha256(&paste.sha256)?;
        fs::create_dir_all(&self.runtime_dir).map_err(|source| ImageBridgeError::Io {
            path: self.runtime_dir.clone(),
            source,
        })?;
        let (path, file) = create_temp_image(&self.runtime_dir)?;
        self.active = Some(ActiveImage {
            id: paste.id,
            expected_bytes: paste.total_bytes,
            expected_sha256,
            received_bytes: 0,
            next_index: 0,
            path,
            file,
            hasher: Sha256::new(),
        });
        Ok(())
    }

    fn chunk(&mut self, chunk: ImageChunk) -> Result<(), ImageBridgeError> {
        let Some(active) = self.active.as_ref() else {
            return Err(ImageBridgeError::UnexpectedFrame("image_chunk"));
        };
        if active.id != chunk.id {
            let expected = active.id.clone();
            self.abort_active();
            return Err(ImageBridgeError::IdMismatch {
                expected,
                actual: chunk.id,
            });
        }
        if active.next_index != chunk.index {
            let expected = active.next_index;
            self.abort_active();
            return Err(ImageBridgeError::OutOfOrder {
                expected,
                actual: chunk.index,
            });
        }
        if chunk.data_base64.len() > encoded_size(IMAGE_CHUNK_BYTES) {
            self.abort_active();
            return Err(ImageBridgeError::ChunkTooLarge {
                max: IMAGE_CHUNK_BYTES,
            });
        }
        let decoded = match decode_base64(&chunk.data_base64) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.abort_active();
                return Err(error);
            }
        };
        if decoded.len() > IMAGE_CHUNK_BYTES {
            self.abort_active();
            return Err(ImageBridgeError::ChunkTooLarge {
                max: IMAGE_CHUNK_BYTES,
            });
        }

        let active = self.active.as_ref().expect("active image was checked");
        let next_total = match active.received_bytes.checked_add(decoded.len() as u64) {
            Some(total) => total,
            None => {
                let max = active.expected_bytes.min(MAX_IMAGE_BYTES);
                self.abort_active();
                return Err(ImageBridgeError::Oversize {
                    declared: u64::MAX,
                    max,
                });
            }
        };
        if next_total > active.expected_bytes || next_total > MAX_IMAGE_BYTES {
            let max = active.expected_bytes.min(MAX_IMAGE_BYTES);
            self.abort_active();
            return Err(ImageBridgeError::Oversize {
                declared: next_total,
                max,
            });
        }
        let write_error = {
            let active = self.active.as_mut().expect("active image was checked");
            active
                .file
                .write_all(&decoded)
                .err()
                .map(|source| (active.path.clone(), source))
        };
        if let Some((path, source)) = write_error {
            self.abort_active();
            return Err(ImageBridgeError::Io { path, source });
        }
        let active = self.active.as_mut().expect("active image was checked");
        active.hasher.update(&decoded);
        active.received_bytes = next_total;
        active.next_index += 1;
        Ok(())
    }

    fn finish(&mut self, end: ImageEnd) -> Result<Option<PathBuf>, ImageBridgeError> {
        let Some(active) = self.active.as_ref() else {
            return Err(ImageBridgeError::UnexpectedFrame("image_end"));
        };
        if active.id != end.id {
            let expected = active.id.clone();
            self.abort_active();
            return Err(ImageBridgeError::IdMismatch {
                expected,
                actual: end.id,
            });
        }

        let mut active = self.active.take().expect("active image was checked");
        if active.received_bytes != active.expected_bytes {
            let error = ImageBridgeError::SizeMismatch {
                expected: active.expected_bytes,
                actual: active.received_bytes,
            };
            cleanup_active(active);
            return Err(error);
        }
        if let Err(source) = active.file.flush() {
            let path = active.path.clone();
            cleanup_active(active);
            return Err(ImageBridgeError::Io { path, source });
        }
        let digest = active.hasher.clone().finalize();
        let actual = digest_hex(&digest);
        if actual != active.expected_sha256 {
            let expected = active.expected_sha256.clone();
            cleanup_active(active);
            return Err(ImageBridgeError::HashMismatch { expected, actual });
        }
        drop(active.file);
        Ok(Some(active.path))
    }

    fn abort_active(&mut self) {
        if let Some(active) = self.active.take() {
            cleanup_active(active);
        }
    }
}

impl Drop for ImageReceiver {
    fn drop(&mut self) {
        self.abort_active();
    }
}

struct ActiveImage {
    id: String,
    expected_bytes: u64,
    expected_sha256: String,
    received_bytes: u64,
    next_index: u32,
    path: PathBuf,
    file: File,
    hasher: Sha256,
}

fn cleanup_active(active: ActiveImage) {
    drop(active.file);
    let _ = fs::remove_file(active.path);
}

fn create_temp_image(runtime_dir: &Path) -> Result<(PathBuf, File), ImageBridgeError> {
    for _ in 0..32 {
        let id = NEXT_IMAGE_FILE.fetch_add(1, Ordering::Relaxed);
        let path = runtime_dir.join(format!("clipboard-{}-{id}.png", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(ImageBridgeError::Io { path, source }),
        }
    }
    Err(ImageBridgeError::NoUniquePath(runtime_dir.to_owned()))
}

fn validate_image_id(id: &str) -> Result<(), ImageBridgeError> {
    if id.is_empty() || id.len() > MAX_IMAGE_ID_BYTES || id.contains('\0') {
        Err(ImageBridgeError::InvalidId)
    } else {
        Ok(())
    }
}

fn normalize_sha256(value: &str) -> Result<String, ImageBridgeError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ImageBridgeError::InvalidSha256(value.to_owned()));
    }
    Ok(value.to_ascii_lowercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest_hex(&digest)
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

const BASE64_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encoded_size(bytes: usize) -> usize {
    bytes.saturating_add(2) / 3 * 4
}

fn encode_base64(input: &[u8]) -> String {
    let mut output = String::with_capacity(encoded_size(input.len()));
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(BASE64_TABLE[(first >> 2) as usize] as char);
        output.push(BASE64_TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(BASE64_TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(BASE64_TABLE[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ImageBridgeError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(ImageBridgeError::InvalidBase64);
    }
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    for (quartet_index, quartet) in bytes.chunks_exact(4).enumerate() {
        let last = quartet_index + 1 == bytes.len() / 4;
        let a = base64_value(quartet[0]).ok_or(ImageBridgeError::InvalidBase64)?;
        let b = base64_value(quartet[1]).ok_or(ImageBridgeError::InvalidBase64)?;
        let c_padding = quartet[2] == b'=';
        let d_padding = quartet[3] == b'=';
        if !last && (c_padding || d_padding) || c_padding && !d_padding {
            return Err(ImageBridgeError::InvalidBase64);
        }
        let c = if c_padding {
            0
        } else {
            base64_value(quartet[2]).ok_or(ImageBridgeError::InvalidBase64)?
        };
        let d = if d_padding {
            0
        } else {
            base64_value(quartet[3]).ok_or(ImageBridgeError::InvalidBase64)?
        };
        if c_padding && b & 0x0f != 0 || d_padding && !c_padding && c & 0x03 != 0 {
            return Err(ImageBridgeError::InvalidBase64);
        }
        output.push((a << 2) | (b >> 4));
        if !c_padding {
            output.push((b << 4) | (c >> 2));
        }
        if !d_padding {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[derive(Debug, Error)]
pub enum ImageBridgeError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("clipboard image id is empty, too long, or contains a NUL byte")]
    InvalidId,
    #[error("clipboard image declares {declared} bytes, above the {max} byte limit")]
    Oversize { declared: u64, max: u64 },
    #[error("clipboard image chunk exceeds the {max} byte decoded limit")]
    ChunkTooLarge { max: usize },
    #[error("clipboard image SHA-256 is invalid: `{0}`")]
    InvalidSha256(String),
    #[error("clipboard image chunk is not strict standard Base64")]
    InvalidBase64,
    #[error("received {0} without an active image_paste")]
    UnexpectedFrame(&'static str),
    #[error("clipboard image id mismatch: expected `{expected}`, received `{actual}`")]
    IdMismatch { expected: String, actual: String },
    #[error("clipboard image chunk index is out of order: expected {expected}, received {actual}")]
    OutOfOrder { expected: u32, actual: u32 },
    #[error("clipboard image size mismatch: expected {expected} bytes, received {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("clipboard image hash mismatch: expected {expected}, received {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("could not access clipboard image path `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not allocate a unique clipboard image below `{0}`")]
    NoUniquePath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    use starcil_platform::InMemoryTransport;

    #[tokio::test]
    async fn happy_path_reassembles_verified_png_under_runtime_dir() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("runtime");
        let bytes = (0..(IMAGE_CHUNK_BYTES + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let expected = bytes.clone();
        let (mut sender, mut receiver_transport) = InMemoryTransport::pair(1024 * 1024);
        let send = tokio::spawn(async move {
            send_image(&mut sender, "paste-1", &bytes).await.unwrap();
        });
        let mut receiver = ImageReceiver::new(&runtime);
        let completed = loop {
            let value = receiver_transport.recv().await.unwrap().unwrap();
            if let Some(path) = receiver.receive_value(value).unwrap() {
                break path;
            }
        };
        send.await.unwrap();
        assert!(completed.starts_with(&runtime));
        assert_eq!(fs::read(&completed).unwrap(), expected);
    }

    #[test]
    fn hash_mismatch_removes_the_partial_file() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"not actually a png";
        let mut receiver = ImageReceiver::new(temp.path());
        receiver
            .receive(ImageBridgeFrame::Paste {
                image_paste: ImagePaste {
                    id: "paste-2".into(),
                    format: ImageFormat::Png,
                    total_bytes: bytes.len() as u64,
                    sha256: "00".repeat(32),
                },
            })
            .unwrap();
        receiver
            .receive(ImageBridgeFrame::Chunk {
                image_chunk: ImageChunk {
                    id: "paste-2".into(),
                    index: 0,
                    data_base64: encode_base64(bytes),
                },
            })
            .unwrap();
        let error = receiver
            .receive(ImageBridgeFrame::End {
                image_end: ImageEnd { id: "paste-2".into() },
            })
            .unwrap_err();
        assert!(matches!(error, ImageBridgeError::HashMismatch { .. }));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn out_of_order_chunk_is_rejected_and_aborted() {
        let temp = tempfile::tempdir().unwrap();
        let mut receiver = ImageReceiver::new(temp.path());
        receiver
            .receive(ImageBridgeFrame::Paste {
                image_paste: ImagePaste {
                    id: "paste-3".into(),
                    format: ImageFormat::Png,
                    total_bytes: 1,
                    sha256: sha256_hex(&[1]),
                },
            })
            .unwrap();
        let error = receiver
            .receive(ImageBridgeFrame::Chunk {
                image_chunk: ImageChunk {
                    id: "paste-3".into(),
                    index: 1,
                    data_base64: encode_base64(&[1]),
                },
            })
            .unwrap_err();
        assert!(matches!(error, ImageBridgeError::OutOfOrder { expected: 0, actual: 1 }));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn oversize_declaration_is_rejected_before_file_creation() {
        let temp = tempfile::tempdir().unwrap();
        let mut receiver = ImageReceiver::new(temp.path());
        let error = receiver
            .receive(ImageBridgeFrame::Paste {
                image_paste: ImagePaste {
                    id: "paste-4".into(),
                    format: ImageFormat::Png,
                    total_bytes: MAX_IMAGE_BYTES + 1,
                    sha256: "00".repeat(32),
                },
            })
            .unwrap_err();
        assert!(matches!(error, ImageBridgeError::Oversize { .. }));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn strict_base64_round_trips_padding_boundaries() {
        for bytes in [b"".as_slice(), b"a", b"ab", b"abc", &[0, 255, 1, 2, 3]] {
            assert_eq!(decode_base64(&encode_base64(bytes)).unwrap(), bytes);
        }
        assert!(decode_base64("Zh==").is_err());
        assert!(decode_base64("Zg=A").is_err());
    }
}
