//! Error types for search operations.
//!
//! Variants are intentionally fine-grained so callers can implement
//! provider-specific remediation (retry, rotate keys, surface UI messages)
//! without parsing strings. Retry semantics are exposed via
//! [`SearchError::is_retryable`].

use std::time::Duration;

use serde::Serialize;

/// Errors returned by [`SearchClient`](crate::SearchClient) and its providers.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Transport / DNS / TLS / timeout failure — never reached the server.
    #[error("transport error from {provider}: {message}")]
    Transport {
        /// Provider identifier.
        provider: &'static str,
        /// Human-readable description.
        message: String,
    },

    /// Server responded with a 5xx status.
    #[error("{provider} returned HTTP {status}")]
    ServerError {
        /// Provider identifier.
        provider: &'static str,
        /// HTTP status code.
        status: u16,
    },

    /// Server applied rate limiting (HTTP 429).
    #[error("{provider} rate-limited this request")]
    RateLimited {
        /// Provider identifier.
        provider: &'static str,
        /// Optional `Retry-After` hint reported by the server.
        retry_after: Option<Duration>,
    },

    /// Authentication or authorisation failed (401/403).
    #[error("{provider} rejected credentials (HTTP {status})")]
    AuthFailed {
        /// Provider identifier.
        provider: &'static str,
        /// HTTP status code.
        status: u16,
    },

    /// Required API key is missing from the environment.
    #[error("missing API key for {provider}: set {env_var}")]
    MissingApiKey {
        /// Provider identifier.
        provider: &'static str,
        /// Environment variable the user must set.
        env_var: &'static str,
    },

    /// `SearXNG` base URL was not configured.
    #[error("missing SearXNG base URL: set H2M_SEARXNG_URL or pass --searxng-url")]
    MissingSearxngUrl,

    /// Provider returned a malformed or unexpected payload.
    #[error("invalid response from {provider}: {message}")]
    InvalidResponse {
        /// Provider identifier.
        provider: &'static str,
        /// Human-readable description.
        message: String,
    },

    /// Response exceeded the configured decoded body size limit. Not retryable.
    #[error("{provider} response exceeds {limit_bytes} bytes")]
    ResponseTooLarge {
        /// Provider identifier.
        provider: &'static str,
        /// Maximum permitted response size in bytes.
        limit_bytes: usize,
    },

    /// HTML scraping detected an anti-bot challenge (e.g. `DuckDuckGo` CAPTCHA).
    ///
    /// Retryable: callers can back off and try again with rotated UA or a
    /// fallback endpoint.
    #[error("{provider} served an anti-bot challenge; try again or switch provider")]
    CaptchaDetected {
        /// Provider identifier.
        provider: &'static str,
    },

    /// HTML/JSON parsing failed — upstream changed its layout or returned
    /// unexpected content. Not retryable: the selectors need updating.
    #[error("failed to parse {provider} response: {message}")]
    ParseFailed {
        /// Provider identifier.
        provider: &'static str,
        /// Human-readable description.
        message: String,
    },

    /// Requested provider is not compiled into this build.
    #[error("provider '{name}' is not available (missing feature flag)")]
    ProviderUnavailable {
        /// Requested provider name.
        name: String,
    },

    /// Invalid configuration or query parameters.
    #[error("configuration error: {message}")]
    Config {
        /// Human-readable description.
        message: String,
    },
}

impl SearchError {
    /// Returns the provider identifier associated with this error, if any.
    #[must_use]
    pub const fn provider(&self) -> Option<&'static str> {
        match self {
            Self::Transport { provider, .. }
            | Self::ServerError { provider, .. }
            | Self::RateLimited { provider, .. }
            | Self::AuthFailed { provider, .. }
            | Self::MissingApiKey { provider, .. }
            | Self::InvalidResponse { provider, .. }
            | Self::ResponseTooLarge { provider, .. }
            | Self::CaptchaDetected { provider }
            | Self::ParseFailed { provider, .. } => Some(provider),
            Self::MissingSearxngUrl | Self::ProviderUnavailable { .. } | Self::Config { .. } => {
                None
            }
        }
    }

    /// Returns the HTTP status associated with this error, if any.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::ServerError { status, .. } | Self::AuthFailed { status, .. } => Some(*status),
            Self::RateLimited { .. } => Some(429),
            _ => None,
        }
    }

    /// Returns the machine-readable kind tag used in JSON serialization.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "transport",
            Self::ServerError { .. } => "serverError",
            Self::RateLimited { .. } => "rateLimited",
            Self::AuthFailed { .. } => "authFailed",
            Self::MissingApiKey { .. } => "missingApiKey",
            Self::MissingSearxngUrl => "missingSearxngUrl",
            Self::InvalidResponse { .. } => "invalidResponse",
            Self::ResponseTooLarge { .. } => "responseTooLarge",
            Self::CaptchaDetected { .. } => "captchaDetected",
            Self::ParseFailed { .. } => "parseFailed",
            Self::ProviderUnavailable { .. } => "providerUnavailable",
            Self::Config { .. } => "config",
        }
    }

    /// Returns `true` if the error is worth retrying.
    ///
    /// Transient transport failures, 5xx, and 429 are retryable. Auth,
    /// config, and validation errors are not.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport { .. }
                | Self::ServerError { .. }
                | Self::RateLimited { .. }
                | Self::CaptchaDetected { .. }
        )
    }
}

impl Serialize for SearchError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("error", &self.to_string())?;
        map.serialize_entry("kind", self.kind())?;
        if let Some(p) = self.provider() {
            map.serialize_entry("provider", p)?;
        }
        if let Some(s) = self.status() {
            map.serialize_entry("status", &s)?;
        }
        if let Self::ResponseTooLarge { limit_bytes, .. } = self {
            map.serialize_entry("limitBytes", limit_bytes)?;
        }
        map.end()
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test assertions should panic on wrong shape"
)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn retryable_variants_are_correctly_identified() {
        assert!(
            SearchError::Transport {
                provider: "x",
                message: String::new()
            }
            .is_retryable()
        );
        assert!(
            SearchError::ServerError {
                provider: "x",
                status: 503,
            }
            .is_retryable()
        );
        assert!(
            SearchError::RateLimited {
                provider: "x",
                retry_after: None
            }
            .is_retryable()
        );
        assert!(
            !SearchError::AuthFailed {
                provider: "x",
                status: 401,
            }
            .is_retryable()
        );
        assert!(
            !SearchError::InvalidResponse {
                provider: "x",
                message: String::new()
            }
            .is_retryable()
        );
    }

    #[test]
    fn serializes_with_kind_tag_and_status() {
        let err = SearchError::RateLimited {
            provider: "brave",
            retry_after: None,
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "rateLimited");
        assert_eq!(json["provider"], "brave");
        assert_eq!(json["status"], 429);
    }

    #[test]
    fn config_errors_omit_provider_and_status() {
        let err = SearchError::Config {
            message: "x".into(),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "config");
        assert!(json.get("provider").is_none());
        assert!(json.get("status").is_none());
    }
}
