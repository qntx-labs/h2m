//! Tavily Search API provider.
//!
//! AI-oriented search with built-in LLM-friendly snippets. Requires an API
//! token supplied via the `TAVILY_API_KEY` environment variable.
//!
//! Enable the `tavily` feature to use this provider.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::instrument;

use super::common::{classify_status, classify_transport};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::query::{SearchQuery, SearchSource, TimeRange};
use crate::response::{SearchHit, SearchResponse};
use crate::retry::{RetryPolicy, retry};
use crate::secret::SecretString;

const PROVIDER_ID: &str = "tavily";
/// Canonical Tavily API root URL.
const DEFAULT_BASE_URL: &str = "https://api.tavily.com";
const SEARCH_PATH: &str = "search";
/// Tavily caps `max_results` at 20 per request.
const MAX_RESULTS: usize = 20;

/// Search topic Tavily should use; maps to [`SearchSource`] semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TavilyTopic {
    /// General web results (default).
    General,
    /// News-focused results.
    News,
    /// Finance-focused results (latest Tavily addition).
    Finance,
}

impl TavilyTopic {
    const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::News => "news",
            Self::Finance => "finance",
        }
    }
}

/// Tavily Search API provider.
#[derive(Clone)]
pub struct Tavily {
    http: HttpConfig,
    base_url: url::Url,
    api_key: SecretString,
    retry_policy: RetryPolicy,
    topic_override: Option<TavilyTopic>,
    include_answer: bool,
}

impl std::fmt::Debug for Tavily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tavily")
            .field("base_url", &self.base_url.as_str())
            .field("api_key", &self.api_key)
            .field("retry_policy", &self.retry_policy)
            .field("topic_override", &self.topic_override)
            .field("include_answer", &self.include_answer)
            .finish_non_exhaustive()
    }
}

impl Tavily {
    /// Creates a provider pointing at the canonical Tavily endpoint.
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
    pub fn builder(api_key: impl Into<SecretString>) -> TavilyBuilder {
        TavilyBuilder {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            http: None,
            retry: None,
            topic: None,
            include_answer: false,
        }
    }

    /// Executes a search against the Tavily API.
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
                env_var: crate::client::ENV_TAVILY_API_KEY,
            });
        }

        let start = Instant::now();
        let endpoint = self
            .base_url
            .join(SEARCH_PATH)
            .map_err(|e| SearchError::Config {
                message: format!("cannot build Tavily endpoint: {e}"),
            })?;

        let topic = self
            .topic_override
            .unwrap_or_else(|| topic_for(&query.sources));
        let payload = TavilyRequest {
            query: &query.query,
            max_results: query.limit.min(MAX_RESULTS),
            search_depth: "basic",
            include_domains: None,
            include_answer: self.include_answer,
            time_range: query.time_range.map(time_range_label),
            topic: topic.as_str(),
            country: query.country.as_deref(),
        };

        let parsed: TavilyResponse = retry(&self.retry_policy, || {
            self.request(endpoint.clone(), &payload)
        })
        .await?;

        let mut result = SearchResponse::new(&query.query, PROVIDER_ID);
        result.answer = parsed.answer.filter(|s| !s.is_empty());
        let target_source = match topic {
            TavilyTopic::News => SearchSource::News,
            TavilyTopic::General | TavilyTopic::Finance => SearchSource::Web,
        };
        for raw in parsed.results {
            result.push(target_source, raw.into_hit());
        }
        result.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(result)
    }
}

impl Tavily {
    async fn request(
        &self,
        endpoint: url::Url,
        payload: &TavilyRequest<'_>,
    ) -> Result<TavilyResponse, SearchError> {
        let response = self
            .http
            .client()
            .post(endpoint)
            .bearer_auth(self.api_key.expose())
            .header("Content-Type", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|e| classify_transport(PROVIDER_ID, &e))?;
        if !response.status().is_success() {
            return Err(classify_status(PROVIDER_ID, &response));
        }
        self.http.read_json(PROVIDER_ID, response).await
    }
}

/// Typed builder for [`Tavily`].
#[derive(Debug)]
pub struct TavilyBuilder {
    api_key: SecretString,
    base_url: String,
    http: Option<HttpConfig>,
    retry: Option<RetryPolicy>,
    topic: Option<TavilyTopic>,
    include_answer: bool,
}

impl TavilyBuilder {
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

    /// Pins the topic explicitly; otherwise it is inferred from the
    /// query's source set.
    #[must_use]
    pub const fn topic(mut self, topic: TavilyTopic) -> Self {
        self.topic = Some(topic);
        self
    }

    /// Enables Tavily's LLM-generated `answer` in the response.
    ///
    /// Defaults to `false` because it consumes additional Tavily credits.
    /// When enabled, the synthesised answer is returned via
    /// [`SearchResponse::answer`](crate::SearchResponse::answer).
    #[must_use]
    pub const fn include_answer(mut self, include: bool) -> Self {
        self.include_answer = include;
        self
    }

    /// Builds the provider.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if the base URL is invalid or the
    /// default HTTP client cannot be constructed.
    pub fn build(self) -> Result<Tavily, SearchError> {
        let base_url = url::Url::parse(&self.base_url).map_err(|e| SearchError::Config {
            message: format!("invalid Tavily base URL: {e}"),
        })?;
        Ok(Tavily {
            http: super::common::resolve_http(self.http)?,
            base_url,
            api_key: self.api_key,
            retry_policy: super::common::resolve_retry(self.retry),
            topic_override: self.topic,
            include_answer: self.include_answer,
        })
    }
}

const fn time_range_label(range: TimeRange) -> &'static str {
    match range {
        TimeRange::Day => "day",
        TimeRange::Week => "week",
        TimeRange::Month => "month",
        TimeRange::Year => "year",
    }
}

const fn topic_for(sources: &[SearchSource]) -> TavilyTopic {
    let mut i = 0;
    while i < sources.len() {
        if matches!(sources[i], SearchSource::News) {
            return TavilyTopic::News;
        }
        i += 1;
    }
    TavilyTopic::General
}

#[derive(Debug, Serialize)]
struct TavilyRequest<'a> {
    query: &'a str,
    max_results: usize,
    search_depth: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_domains: Option<Vec<String>>,
    include_answer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_range: Option<&'static str>,
    topic: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
    #[serde(default)]
    answer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    published_date: Option<String>,
    #[serde(default)]
    score: Option<f64>,
}

impl TavilyResult {
    fn into_hit(self) -> SearchHit {
        SearchHit {
            title: self.title,
            url: self.url,
            description: self.content.filter(|s| !s.is_empty()),
            published_at: self.published_date,
            engine: None,
            score: self.score,
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
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn maps_results_to_web_with_bearer_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("Authorization", "Bearer tvly-KEY"))
            .and(body_partial_json(serde_json::json!({
                "query": "rust",
                "topic": "general",
                "search_depth": "basic"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "title": "Rust", "url": "https://r.io", "content": "lang", "published_date": "2025-10-01" }
                ]
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-KEY", server.uri()).unwrap();
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();

        assert_eq!(response.provider, "tavily");
        assert_eq!(response.web.len(), 1);
        assert_eq!(response.web[0].published_at.as_deref(), Some("2025-10-01"));
    }

    #[tokio::test]
    async fn does_not_leak_api_key_in_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-SECRET", server.uri()).unwrap();
        let _ = provider.search(&SearchQuery::new("q")).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = std::str::from_utf8(&requests[0].body).unwrap();
        assert!(
            !body.contains("tvly-SECRET") && !body.contains("api_key"),
            "API key must only live in the Bearer header, got body: {body}"
        );
    }

    #[tokio::test]
    async fn clamps_limit_to_api_maximum() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(serde_json::json!({ "max_results": 20 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-KEY", server.uri()).unwrap();
        let query = SearchQuery::new("q").with_limit(50);
        provider.search(&query).await.unwrap();
    }

    #[tokio::test]
    async fn forwards_country_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(
                serde_json::json!({ "country": "united states" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-KEY", server.uri()).unwrap();
        let query = SearchQuery::new("q").with_country("united states");
        provider.search(&query).await.unwrap();
    }

    #[tokio::test]
    async fn news_source_routes_to_news_bucket() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(serde_json::json!({ "topic": "news" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "title": "N", "url": "https://n" }]
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("KEY", server.uri()).unwrap();
        let query = SearchQuery::new("q").with_sources(vec![SearchSource::News]);
        let response = provider.search(&query).await.unwrap();
        assert_eq!(response.news.len(), 1);
        assert_eq!(response.web.len(), 0);
    }

    #[tokio::test]
    async fn auth_failed_is_classified() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let provider = Tavily::builder("tvly-KEY")
            .base_url(server.uri())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let err = provider.search(&SearchQuery::new("q")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::AuthFailed {
                provider: "tavily",
                status: 403
            }
        ));
    }

    #[tokio::test]
    async fn include_answer_defaults_to_false() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(
                serde_json::json!({ "include_answer": false }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-KEY", server.uri()).unwrap();
        let response = provider.search(&SearchQuery::new("q")).await.unwrap();
        assert!(response.answer.is_none());
    }

    #[tokio::test]
    async fn include_answer_round_trip_populates_response_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(
                serde_json::json!({ "include_answer": true }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "answer": "Rust is a systems programming language.",
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::builder("tvly-KEY")
            .base_url(server.uri())
            .include_answer(true)
            .build()
            .unwrap();
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();
        assert_eq!(
            response.answer.as_deref(),
            Some("Rust is a systems programming language.")
        );
    }

    #[tokio::test]
    async fn score_field_propagates_to_search_hit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "title": "Rust", "url": "https://r.io", "score": 0.87 }
                ]
            })))
            .mount(&server)
            .await;

        let provider = Tavily::with_base_url("tvly-KEY", server.uri()).unwrap();
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();
        assert_eq!(response.web[0].score, Some(0.87));
    }

    #[tokio::test]
    async fn finance_topic_can_be_pinned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(body_partial_json(serde_json::json!({ "topic": "finance" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let provider = Tavily::builder("tvly-KEY")
            .base_url(server.uri())
            .topic(TavilyTopic::Finance)
            .build()
            .unwrap();
        provider.search(&SearchQuery::new("q")).await.unwrap();
    }

    #[tokio::test]
    async fn empty_api_key_short_circuits() {
        let provider = Tavily::new("").unwrap();
        let err = provider.search(&SearchQuery::new("q")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::MissingApiKey {
                provider: "tavily",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn debug_does_not_leak_api_key() {
        let provider = Tavily::new("tvly-super-secret").unwrap();
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("tvly-super-secret"),
            "Debug leaked key: {debug}"
        );
    }
}
