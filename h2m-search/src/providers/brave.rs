//! Brave Search API provider.
//!
//! Uses the official Brave Web Search API backed by Brave's independent
//! index. Requires a subscription token supplied via the `BRAVE_API_KEY`
//! environment variable.
//!
//! Brave caps `count` at 20 per page and `offset` at 9, giving a hard
//! ceiling of 200 results. This provider transparently paginates via
//! `offset` when [`SearchQuery::limit`] exceeds a single page.
//!
//! Enable the `brave` feature to use this provider.

use std::time::Instant;

use serde::Deserialize;
use tracing::instrument;

use super::common::{classify_status, classify_transport};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::query::{SafeSearch, SearchQuery, SearchSource, TimeRange};
use crate::response::{SearchHit, SearchResponse};
use crate::retry::{RetryPolicy, retry};
use crate::secret::SecretString;

const PROVIDER_ID: &str = "brave";
/// Canonical Brave Search API root URL.
const DEFAULT_BASE_URL: &str = "https://api.search.brave.com";
const SEARCH_PATH: &str = "res/v1/web/search";
/// Brave caps `count` at 20 per documentation.
const MAX_COUNT: usize = 20;
/// Brave caps `offset` at 9 (0-indexed), giving 10 pages / 200 results max.
const MAX_OFFSET: u32 = 9;

/// Brave Search API provider.
#[derive(Clone)]
pub struct Brave {
    http: HttpConfig,
    base_url: url::Url,
    api_key: SecretString,
    retry_policy: RetryPolicy,
}

impl std::fmt::Debug for Brave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Brave")
            .field("base_url", &self.base_url.as_str())
            .field("api_key", &self.api_key)
            .field("retry_policy", &self.retry_policy)
            .finish_non_exhaustive()
    }
}

impl Brave {
    /// Creates a provider pointing at the canonical Brave API endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if the HTTP client cannot be built.
    pub fn new(api_key: impl Into<SecretString>) -> Result<Self, SearchError> {
        Self::builder(api_key).build()
    }

    /// Convenience constructor for tests and ad-hoc base URLs.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if `base_url` is invalid or the HTTP
    /// client cannot be built.
    pub fn with_base_url(
        api_key: impl Into<SecretString>,
        base_url: impl AsRef<str>,
    ) -> Result<Self, SearchError> {
        Self::builder(api_key).base_url(base_url.as_ref()).build()
    }

    /// Starts a typed builder for fine-grained configuration.
    #[must_use]
    pub fn builder(api_key: impl Into<SecretString>) -> BraveBuilder {
        BraveBuilder {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            http: None,
            retry: None,
        }
    }

    /// Executes a search against the Brave API.
    ///
    /// Transparently paginates via `offset` up to Brave's internal cap of 9
    /// (10 pages / 200 results) to satisfy [`SearchQuery::limit`]. Each page
    /// request honours retries on 429/5xx responses.
    ///
    /// # Errors
    ///
    /// Returns the specific [`SearchError`] produced by the underlying
    /// HTTP layer (see [`SearchError::is_retryable`]).
    #[instrument(level = "debug", skip(self), fields(provider = PROVIDER_ID))]
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResponse, SearchError> {
        if !query.is_valid() {
            return Err(SearchError::Config {
                message: "search query is empty".into(),
            });
        }
        if self.api_key.is_empty() {
            return Err(SearchError::MissingApiKey {
                provider: PROVIDER_ID,
                env_var: crate::client::ENV_BRAVE_API_KEY,
            });
        }

        let start = Instant::now();
        let mut result = SearchResponse::new(&query.query, PROVIDER_ID);
        let wants_web = query.sources.contains(&SearchSource::Web);
        let wants_news = query.sources.contains(&SearchSource::News);

        let mut offset: u32 = 0;
        loop {
            if result.total() >= query.limit || offset > MAX_OFFSET {
                break;
            }
            let body = self.fetch_page(query, offset).await?;
            let web_len = body.web.as_ref().map_or(0, |s| s.results.len());
            let news_len = body.news.as_ref().map_or(0, |s| s.results.len());
            if web_len == 0 && news_len == 0 {
                break;
            }
            if wants_web {
                push_section(&mut result, SearchSource::Web, body.web, query.limit);
            }
            if wants_news {
                push_section(&mut result, SearchSource::News, body.news, query.limit);
            }
            if web_len < MAX_COUNT && news_len < MAX_COUNT {
                break;
            }
            offset = offset.saturating_add(1);
        }

        result.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(result)
    }

    async fn fetch_page(
        &self,
        query: &SearchQuery,
        offset: u32,
    ) -> Result<BraveResponse, SearchError> {
        let endpoint = self.build_endpoint(query, offset)?;
        retry(&self.retry_policy, || self.request(endpoint.clone())).await
    }

    async fn request(&self, endpoint: url::Url) -> Result<BraveResponse, SearchError> {
        let response = self
            .http
            .client()
            .get(endpoint)
            .header("X-Subscription-Token", self.api_key.expose())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| classify_transport(PROVIDER_ID, &e))?;
        if !response.status().is_success() {
            return Err(classify_status(PROVIDER_ID, &response));
        }
        self.http.read_json(PROVIDER_ID, response).await
    }

    fn build_endpoint(&self, query: &SearchQuery, offset: u32) -> Result<url::Url, SearchError> {
        let mut endpoint = self
            .base_url
            .join(SEARCH_PATH)
            .map_err(|e| SearchError::Config {
                message: format!("cannot build Brave endpoint: {e}"),
            })?;
        let offset_string = offset.to_string();
        {
            let mut pairs = endpoint.query_pairs_mut();
            pairs.append_pair("q", &query.query);
            pairs.append_pair("count", &MAX_COUNT.to_string());
            pairs.append_pair("offset", &offset_string);
            pairs.append_pair("safesearch", safesearch_label(query.safesearch));
            if let Some(country) = &query.country {
                pairs.append_pair("country", country);
            }
            if let Some(lang) = &query.language {
                pairs.append_pair("search_lang", lang);
            }
            if let Some(tr) = query.time_range {
                pairs.append_pair("freshness", freshness_label(tr));
            }
        }
        Ok(endpoint)
    }
}

/// Typed builder for [`Brave`].
#[derive(Debug)]
pub struct BraveBuilder {
    api_key: SecretString,
    base_url: String,
    http: Option<HttpConfig>,
    retry: Option<RetryPolicy>,
}

impl BraveBuilder {
    /// Overrides the API base URL (defaults to [`DEFAULT_BASE_URL`]).
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Supplies a shared [`HttpConfig`].
    #[must_use]
    pub fn http(mut self, http: HttpConfig) -> Self {
        self.http = Some(http);
        self
    }

    /// Overrides the retry policy.
    #[must_use]
    pub const fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// Builds the provider.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if the base URL is invalid or the
    /// default HTTP client cannot be constructed.
    pub fn build(self) -> Result<Brave, SearchError> {
        let base_url = url::Url::parse(&self.base_url).map_err(|e| SearchError::Config {
            message: format!("invalid Brave base URL: {e}"),
        })?;
        Ok(Brave {
            http: super::common::resolve_http(self.http)?,
            base_url,
            api_key: self.api_key,
            retry_policy: super::common::resolve_retry(self.retry),
        })
    }
}

fn push_section(
    response: &mut SearchResponse,
    source: SearchSource,
    section: Option<BraveSection>,
    limit: usize,
) {
    let Some(section) = section else { return };
    for raw in section.results {
        if response.total() >= limit {
            break;
        }
        response.push(source, raw.into_hit());
    }
}

const fn safesearch_label(level: SafeSearch) -> &'static str {
    match level {
        SafeSearch::Off => "off",
        SafeSearch::Moderate => "moderate",
        SafeSearch::Strict => "strict",
    }
}

const fn freshness_label(range: TimeRange) -> &'static str {
    match range {
        TimeRange::Day => "pd",
        TimeRange::Week => "pw",
        TimeRange::Month => "pm",
        TimeRange::Year => "py",
    }
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveSection>,
    #[serde(default)]
    news: Option<BraveSection>,
}

#[derive(Debug, Deserialize)]
struct BraveSection {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    age: Option<String>,
}

impl BraveResult {
    fn into_hit(self) -> SearchHit {
        SearchHit {
            title: self.title,
            url: self.url,
            description: self.description.filter(|s| !s.is_empty()),
            published_at: self.age,
            engine: None,
            score: None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test assertions should panic on wrong shape"
)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn maps_web_and_news_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(query_param("q", "rust"))
            .and(header("X-Subscription-Token", "KEY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "web": {
                    "results": [
                        { "title": "Rust", "url": "https://r.io", "description": "lang", "age": "2025-09-01" }
                    ]
                },
                "news": {
                    "results": [
                        { "title": "News", "url": "https://n.io", "description": "hi" }
                    ]
                }
            })))
            .mount(&server)
            .await;

        let provider = Brave::with_base_url("KEY", server.uri()).unwrap();
        let query =
            SearchQuery::new("rust").with_sources(vec![SearchSource::Web, SearchSource::News]);
        let response = provider.search(&query).await.unwrap();

        assert_eq!(response.provider, "brave");
        assert_eq!(response.web.len(), 1);
        assert_eq!(response.web[0].url, "https://r.io");
        assert_eq!(response.web[0].published_at.as_deref(), Some("2025-09-01"));
        assert_eq!(response.news.len(), 1);
    }

    #[tokio::test]
    async fn respects_source_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "web": { "results": [{ "title": "W", "url": "https://w" }] },
                "news": { "results": [{ "title": "N", "url": "https://n" }] }
            })))
            .mount(&server)
            .await;

        let provider = Brave::with_base_url("KEY", server.uri()).unwrap();
        let query = SearchQuery::new("q").with_sources(vec![SearchSource::Web]);
        let response = provider.search(&query).await.unwrap();

        assert_eq!(response.web.len(), 1);
        assert_eq!(response.news.len(), 0);
    }

    #[tokio::test]
    async fn auth_failed_is_classified() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let provider = Brave::builder("KEY")
            .base_url(server.uri())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let err = provider.search(&SearchQuery::new("q")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::AuthFailed {
                provider: "brave",
                status: 401
            }
        ));
    }

    #[tokio::test]
    async fn empty_api_key_short_circuits() {
        let provider = Brave::new("").unwrap();
        let err = provider.search(&SearchQuery::new("q")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::MissingApiKey {
                provider: "brave",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let provider = Brave::new("KEY").unwrap();
        let err = provider.search(&SearchQuery::new("")).await.unwrap_err();
        assert!(matches!(err, SearchError::Config { .. }));
    }

    #[tokio::test]
    async fn auto_paginates_when_limit_exceeds_page_size() {
        let server = MockServer::start().await;

        let page_results = (0..MAX_COUNT)
            .map(|i| json!({ "title": format!("T{i}"), "url": format!("https://p0/{i}") }))
            .collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "web": { "results": page_results }
            })))
            .mount(&server)
            .await;

        let page2_results = (0..5)
            .map(|i| json!({ "title": format!("T2-{i}"), "url": format!("https://p1/{i}") }))
            .collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(query_param("offset", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "web": { "results": page2_results }
            })))
            .mount(&server)
            .await;

        let provider = Brave::with_base_url("KEY", server.uri()).unwrap();
        let query = SearchQuery::new("rust").with_limit(25);
        let response = provider.search(&query).await.unwrap();

        assert_eq!(response.web.len(), 25);
        assert_eq!(response.web[0].url, "https://p0/0");
        assert_eq!(response.web[20].url, "https://p1/0");
    }

    #[tokio::test]
    async fn short_page_stops_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "web": { "results": [
                    { "title": "Only", "url": "https://only.io" }
                ]}
            })))
            .mount(&server)
            .await;

        let provider = Brave::with_base_url("KEY", server.uri()).unwrap();
        let query = SearchQuery::new("rust").with_limit(100);
        let response = provider.search(&query).await.unwrap();

        assert_eq!(response.web.len(), 1);
    }

    #[tokio::test]
    async fn debug_does_not_leak_api_key() {
        let provider = Brave::new("super-secret-brave-key").unwrap();
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("super-secret-brave-key"),
            "Debug leaked key: {debug}"
        );
    }
}
