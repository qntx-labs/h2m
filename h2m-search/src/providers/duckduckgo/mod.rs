//! `DuckDuckGo` provider.
//!
//! Talks to `DuckDuckGo`'s unauthenticated HTML endpoints — the same interface
//! the `ddgs` (5M+ monthly downloads), `LangChain`, and Haystack libraries use.
//! No API key, no registration, no environment variables: this is the
//! default zero-config provider for `h2m`.
//!
//! # Robustness layers
//!
//! The provider is engineered against rate limiting and anti-bot challenges
//! via four complementary strategies:
//!
//! 1. **User-Agent rotation** — every request picks from a pool of 10
//!    current (Chrome 121+, Firefox 123+, Safari 17+) browser UAs.
//! 2. **POST + form-data** — the HTML endpoint is reached via POST, which
//!    empirically survives rate limiting better than GET + query string.
//! 3. **Exponential backoff** — 429s and detected CAPTCHA pages retry with
//!    backoff via the shared [`RetryPolicy`].
//! 4. **Endpoint fallback** — when the HTML endpoint returns a CAPTCHA or
//!    parse-failure, the provider automatically falls back to the lighter
//!    `lite.duckduckgo.com/lite/` endpoint, whose HTML is table-based and
//!    much harder to break.
//!
//! # Module layout
//!
//! - [`captcha`] — anomaly detection and fallback-eligibility predicates.
//! - [`http`] — UA rotation + form-encoded POST wrappers for the two endpoints.
//! - [`params`] — query-parameter encoders (`kl`, `df`, `safe`, headers).
//! - [`parse`] — HTML → [`SearchHit`] extraction for both endpoints.
//!
//! Enable the `duckduckgo` feature (on by default) to use this provider.

mod captcha;
mod http;
mod params;
mod parse;

use std::time::Instant;

use tracing::{instrument, warn};

use self::captcha::is_recoverable_via_lite;
use self::http::{RESULTS_PER_PAGE, post_html, post_lite};
use self::parse::{parse_html_results, parse_lite_results};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::query::{SearchQuery, SearchSource};
use crate::response::{SearchHit, SearchResponse};
use crate::retry::{RetryPolicy, retry};

/// Stable identifier sent with every telemetry / error payload.
pub(super) const PROVIDER_ID: &str = "duckduckgo";

const DEFAULT_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const DEFAULT_LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
/// Hard cap on pagination — deeper pages usually trip anomaly detection.
const MAX_PAGES: u32 = 4;

/// `DuckDuckGo` search provider.
#[derive(Debug, Clone)]
pub struct DuckDuckGo {
    http: HttpConfig,
    html_endpoint: String,
    lite_endpoint: String,
    retry_policy: RetryPolicy,
    enable_fallback: bool,
}

impl DuckDuckGo {
    /// Creates a new provider with default settings.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if the default HTTP client cannot be
    /// constructed.
    pub fn new() -> Result<Self, SearchError> {
        Self::builder().build()
    }

    /// Starts a typed builder for fine-grained configuration.
    #[must_use]
    pub fn builder() -> DuckDuckGoBuilder {
        DuckDuckGoBuilder::default()
    }

    /// Executes a search against `DuckDuckGo`.
    ///
    /// The primary HTML endpoint is tried first with the configured retry
    /// policy. On persistent CAPTCHA / parse failures the provider
    /// automatically falls back to the lite endpoint (unless disabled).
    ///
    /// # Errors
    ///
    /// Returns the specific [`SearchError`] from the underlying HTTP layer.
    /// Parse failures surface as [`SearchError::ParseFailed`]; anti-bot
    /// pages surface as [`SearchError::CaptchaDetected`].
    #[instrument(level = "debug", skip(self), fields(provider = PROVIDER_ID))]
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResponse, SearchError> {
        if !query.is_valid() {
            return Err(SearchError::Config {
                message: "search query is empty".into(),
            });
        }

        let start = Instant::now();
        let hits = match self.fetch_html(query).await {
            Ok(h) => h,
            Err(e) if self.enable_fallback && is_recoverable_via_lite(&e) => {
                warn!(primary_error = %e, "falling back to lite.duckduckgo.com");
                self.fetch_lite(query).await?
            }
            Err(e) => return Err(e),
        };

        let mut response = SearchResponse::new(&query.query, PROVIDER_ID);
        for hit in hits.into_iter().take(query.limit) {
            response.push(SearchSource::Web, hit);
        }
        response.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(response)
    }

    async fn fetch_html(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, SearchError> {
        let mut collected: Vec<SearchHit> = Vec::with_capacity(query.limit);
        let mut offset: u32 = 0;
        let mut pages: u32 = 0;
        while collected.len() < query.limit && pages < MAX_PAGES {
            let body = retry(&self.retry_policy, || {
                post_html(
                    &self.http,
                    &self.html_endpoint,
                    query,
                    offset.saturating_mul(RESULTS_PER_PAGE),
                )
            })
            .await?;
            let page = parse_html_results(&body)?;
            let page_len = page.len();
            collected.extend(page);
            if page_len < RESULTS_PER_PAGE as usize || collected.len() >= query.limit {
                break;
            }
            offset = offset.saturating_add(1);
            pages = pages.saturating_add(1);
        }
        Ok(collected)
    }

    async fn fetch_lite(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, SearchError> {
        let body = retry(&self.retry_policy, || {
            post_lite(&self.http, &self.lite_endpoint, query)
        })
        .await?;
        let hits = parse_lite_results(&body)?;
        Ok(hits.into_iter().take(query.limit).collect())
    }
}

/// Typed builder for [`DuckDuckGo`].
#[derive(Debug, Default)]
pub struct DuckDuckGoBuilder {
    http: Option<HttpConfig>,
    html_endpoint: Option<String>,
    lite_endpoint: Option<String>,
    retry: Option<RetryPolicy>,
    enable_fallback: Option<bool>,
}

impl DuckDuckGoBuilder {
    /// Supplies a shared [`HttpConfig`].
    #[must_use]
    pub fn http(mut self, http: HttpConfig) -> Self {
        self.http = Some(http);
        self
    }

    /// Overrides the primary HTML endpoint (useful for tests / mirrors).
    #[must_use]
    pub fn html_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.html_endpoint = Some(endpoint.into());
        self
    }

    /// Overrides the lite fallback endpoint.
    #[must_use]
    pub fn lite_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.lite_endpoint = Some(endpoint.into());
        self
    }

    /// Overrides the retry policy.
    #[must_use]
    pub const fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// Disables the automatic html → lite fallback.
    ///
    /// Fallback is enabled by default. Disable when you want to surface
    /// anomaly/CAPTCHA errors to the caller untouched.
    #[must_use]
    pub const fn fallback(mut self, enabled: bool) -> Self {
        self.enable_fallback = Some(enabled);
        self
    }

    /// Builds the provider.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if the default HTTP client cannot be
    /// constructed.
    pub fn build(self) -> Result<DuckDuckGo, SearchError> {
        Ok(DuckDuckGo {
            http: super::common::resolve_http(self.http)?,
            html_endpoint: self
                .html_endpoint
                .unwrap_or_else(|| DEFAULT_HTML_ENDPOINT.into()),
            lite_endpoint: self
                .lite_endpoint
                .unwrap_or_else(|| DEFAULT_LITE_ENDPOINT.into()),
            retry_policy: super::common::resolve_retry(self.retry),
            enable_fallback: self.enable_fallback.unwrap_or(true),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test assertions should panic on wrong shape"
)]
mod tests {
    use pretty_assertions::assert_eq;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const SAMPLE_HTML: &str = include_str!("../testdata/duckduckgo_html_results.html");
    const SAMPLE_LITE: &str = include_str!("../testdata/duckduckgo_lite_results.html");
    const SAMPLE_CAPTCHA: &str = r#"<!DOCTYPE html><html><body>
<div class="anomaly-modal">
    <p>If this error persists, please reach out to us.</p>
</div>
<script src="/assets/common/error.js"></script>
</body></html>"#;

    fn build_provider(html: &str, lite: &str) -> DuckDuckGo {
        DuckDuckGo::builder()
            .html_endpoint(html)
            .lite_endpoint(lite)
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn html_happy_path_extracts_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_string_contains("q=rust"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_HTML))
            .mount(&server)
            .await;

        let provider = build_provider(&format!("{}/", server.uri()), "http://unused");
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();
        assert_eq!(response.provider, "duckduckgo");
        assert_eq!(response.web.len(), 3);
        assert_eq!(response.web[0].title, "Rust Programming Language");
        assert_eq!(response.web[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            response.web[0].description.as_deref(),
            Some("A language empowering everyone to build reliable and efficient software.")
        );
        assert_eq!(response.web[0].engine.as_deref(), Some("duckduckgo"));
    }

    #[tokio::test]
    async fn captcha_triggers_lite_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_CAPTCHA))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/lite"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_LITE))
            .mount(&server)
            .await;

        let provider = build_provider(
            &format!("{}/html", server.uri()),
            &format!("{}/lite", server.uri()),
        );
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();
        assert_eq!(response.web.len(), 2);
        assert_eq!(response.web[0].title, "Rust Lang");
        assert!(response.web[0].url.starts_with("https://"));
    }

    #[tokio::test]
    async fn http_202_classifies_as_captcha_before_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/html"))
            .respond_with(ResponseTemplate::new(202).set_body_string(SAMPLE_HTML))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/lite"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_LITE))
            .mount(&server)
            .await;

        let provider = build_provider(
            &format!("{}/html", server.uri()),
            &format!("{}/lite", server.uri()),
        );
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();
        assert_eq!(response.web.len(), 2);
    }

    #[tokio::test]
    async fn fallback_disabled_surfaces_captcha() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(202).set_body_string(SAMPLE_HTML))
            .mount(&server)
            .await;

        let provider = DuckDuckGo::builder()
            .html_endpoint(format!("{}/", server.uri()))
            .lite_endpoint("http://unused")
            .retry(RetryPolicy::NONE)
            .fallback(false)
            .build()
            .unwrap();
        let err = provider
            .search(&SearchQuery::new("rust"))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::CaptchaDetected { .. }));
    }

    #[tokio::test]
    async fn limit_truncates_across_pages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_HTML))
            .mount(&server)
            .await;

        let provider = build_provider(&format!("{}/", server.uri()), "http://unused");
        let response = provider
            .search(&SearchQuery::new("rust").with_limit(2))
            .await
            .unwrap();
        assert_eq!(response.web.len(), 2);
    }

    #[tokio::test]
    async fn parse_failure_on_unrecognised_layout() {
        let server = MockServer::start().await;
        // Large body (> MIN_HTML_BODY_BYTES) with no results, no markers,
        // no parseable structure — selectors find nothing.
        let filler = "<p>lorem ipsum dolor sit amet</p>".repeat(60);
        let body = format!("<html><body>{filler}</body></html>");
        Mock::given(method("POST"))
            .and(path("/html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/lite"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let provider = build_provider(
            &format!("{}/html", server.uri()),
            &format!("{}/lite", server.uri()),
        );
        let err = provider
            .search(&SearchQuery::new("rust"))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::ParseFailed { .. }));
    }

    #[tokio::test]
    async fn rate_limited_propagates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/html"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3"))
            .mount(&server)
            .await;
        // Lite also rate-limits — no recovery path.
        Mock::given(method("POST"))
            .and(path("/lite"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let provider = build_provider(
            &format!("{}/html", server.uri()),
            &format!("{}/lite", server.uri()),
        );
        let err = provider
            .search(&SearchQuery::new("rust"))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let provider = DuckDuckGo::new().unwrap();
        let err = provider.search(&SearchQuery::new("")).await.unwrap_err();
        assert!(matches!(err, SearchError::Config { .. }));
    }
}
