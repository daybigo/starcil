use std::{fmt, io::Read, time::Duration};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait HttpClient {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct HttpError {
    message: String,
}

impl HttpError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UreqHttpClient;

impl HttpClient for UreqHttpClient {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(request.timeout)
            .timeout_read(request.timeout)
            .timeout_write(request.timeout)
            .build();
        let response = match agent
            .get(&request.url)
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "starcil-update")
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(HttpError::new(error.to_string())),
        };
        let status = response.status();
        let mut body = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut body)
            .map_err(|error| HttpError::new(error.to_string()))?;
        Ok(HttpResponse { status, body })
    }
}

impl fmt::Display for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.url)
    }
}
