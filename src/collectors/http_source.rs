use futures::StreamExt;
use reqwest::{
    Client,
    header::{HeaderName, HeaderValue},
};
use url::Url;

use crate::{collector::CollectorError, config::BearerToken};

const MAX_SOURCE_BYTES: usize = 1_048_576;

pub async fn fetch(
    client: &Client,
    url: &Url,
    auth: Option<&SourceAuth>,
) -> Result<Vec<u8>, CollectorError> {
    let mut request = client.get(url.clone());
    match auth {
        Some(SourceAuth::Bearer(token)) => request = request.bearer_auth(token.expose()),
        Some(SourceAuth::Header { name, value }) => {
            request = request.header(name, value);
        }
        None => {}
    }
    let response = request
        .send()
        .await
        .map_err(|error| CollectorError::new("http_request", error.to_string()))?
        .error_for_status()
        .map_err(|error| CollectorError::new("http_status", error.to_string()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SOURCE_BYTES as u64)
    {
        return Err(CollectorError::new(
            "http_body_too_large",
            "source response exceeds the configured safety limit",
        ));
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| CollectorError::new("http_body", error.to_string()))?;
        let new_length = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| CollectorError::new("http_body_too_large", "source size overflow"))?;
        if new_length > MAX_SOURCE_BYTES {
            return Err(CollectorError::new(
                "http_body_too_large",
                "source response exceeds the configured safety limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Clone)]
pub enum SourceAuth {
    Bearer(BearerToken),
    Header {
        name: HeaderName,
        value: HeaderValue,
    },
}

impl std::fmt::Debug for SourceAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => formatter.write_str("SourceAuth::Bearer([REDACTED])"),
            Self::Header { name, .. } => formatter
                .debug_struct("SourceAuth::Header")
                .field("name", name)
                .field("value", &"[REDACTED]")
                .finish(),
        }
    }
}
