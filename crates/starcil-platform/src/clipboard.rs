use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard operation failed: {0}")]
    Backend(String),
}

pub trait Clipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError>;
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError>;
    fn has_image(&mut self) -> Result<bool, ClipboardError>;
}

pub struct ArboardClipboard {
    inner: arboard::Clipboard,
}

impl ArboardClipboard {
    pub fn new() -> Result<Self, ClipboardError> {
        arboard::Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(|error| ClipboardError::Backend(error.to_string()))
    }
}

impl Clipboard for ArboardClipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError> {
        self.inner
            .get_text()
            .map_err(|error| ClipboardError::Backend(error.to_string()))
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.inner
            .set_text(text.to_owned())
            .map_err(|error| ClipboardError::Backend(error.to_string()))
    }

    fn has_image(&mut self) -> Result<bool, ClipboardError> {
        match self.inner.get_image() {
            Ok(_) => Ok(true),
            Err(arboard::Error::ContentNotAvailable) => Ok(false),
            Err(error) => Err(ClipboardError::Backend(error.to_string())),
        }
    }
}
