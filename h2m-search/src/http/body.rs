//! Bounded response collection shared by HTML and JSON search providers.

use super::HttpConfig;
use crate::error::SearchError;
use crate::providers::common::classify_transport;

impl HttpConfig {
    async fn read_body(
        &self,
        provider: &'static str,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, SearchError> {
        let too_large = || SearchError::ResponseTooLarge {
            provider,
            limit_bytes: self.max_response_bytes,
        };
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| classify_transport(provider, &error))?
        {
            // Check decoded chunks before accumulation; length headers may be absent.
            if chunk.len() > self.max_response_bytes.saturating_sub(body.len()) {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    #[cfg(feature = "duckduckgo")]
    pub(crate) async fn read_text(
        &self,
        provider: &'static str,
        response: reqwest::Response,
    ) -> Result<String, SearchError> {
        let body = self.read_body(provider, response).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    #[cfg(any(
        feature = "wikipedia",
        feature = "searxng",
        feature = "brave",
        feature = "tavily"
    ))]
    pub(crate) async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        provider: &'static str,
        response: reqwest::Response,
    ) -> Result<T, SearchError> {
        let body = self.read_body(provider, response).await?;
        serde_json::from_slice(&body).map_err(|error| SearchError::InvalidResponse {
            provider,
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests;
