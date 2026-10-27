//! `SearXNG` provider.
//!
//! `SearXNG` is a free, open-source metasearch engine that aggregates results
//! from Google, Bing, `DuckDuckGo`, and dozens of other backends. It requires
//! no API key, but the caller must supply an instance URL — self-hosted
//! deployments are recommended for reliability.
//!
//! `SearXNG` returns ~30 results per page. This provider transparently
//! paginates when [`SearchQuery::limit`] exceeds that.
//!
//! Enable the `searxng` feature (on by default) to use this provider.

use std::time::Instant;

use serde::Deserialize;
use tracing::instrument;

use super::common::{classify_status, classify_transport};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::query::{SearchQuery, SearchSource};
use crate::response::{SearchHit, SearchResponse};
use crate::retry::{RetryPolicy, retry};

const PROVIDER_ID: &str = "searxng";
/// Upstream default results per page (instance-dependent; 30 is a safe floor).
const RESULTS_PER_PAGE: usize = 30;
/// Safety cap to avoid runaway pagination against mis-configured instances.
const MAX_PAGES: u32 = 5;

/// `SearXNG` search provider.
#[derive(Debug, Clone)]
pub struct SearXNG {
    http: HttpConfig,
    base_url: url::Url,
    retry_policy: RetryPolicy,
}

impl SearXNG {
    /// Creates a new provider using the given `base_url` and the default
    /// shared HTTP client.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Config`] if `base_url` is not a valid URL or
    /// the default HTTP client cannot be built.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, SearchError> {
        Self::builder(base_url).build()
    }

    /// Starts a typed builder for fine-grained configuration.
    #[must_use]
    pub fn builder(base_url: impl AsRef<str>) -> SearXNGBuilder {
        SearXNGBuilder {
            base_url: base_url.as_ref().to_owned(),
            http: None,
            retry: None,
        }
    }

    /// Executes a search against the `SearXNG` instance.
    ///
    /// Transparently paginates to satisfy [`SearchQuery::limit`] up to
    /// `MAX_PAGES` pages, honouring retries on 429/5xx responses.
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

        let start = Instant::now();
        let mut result = SearchResponse::new(&query.query, PROVIDER_ID);

        let mut page: u32 = 1;
        while result.total() < query.limit && page <= MAX_PAGES {
            let body = self.fetch_page(query, page).await?;
            let page_size = body.results.len();
            if page_size == 0 {
                break;
            }
            for raw in body.results {
                if result.total() >= query.limit {
                    break;
                }
                let source = classify_source(raw.category.as_deref());
                result.push(source, raw.into_hit());
            }
            // A short page signals the instance has no more results.
            if page_size < RESULTS_PER_PAGE {
                break;
            }
            page = page.saturating_add(1);
        }

        result.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(result)
    }

    async fn fetch_page(
        &self,
        query: &SearchQuery,
        pageno: u32,
    ) -> Result<SearxngResponse, SearchError> {
        let endpoint = self.build_endpoint(query, pageno)?;
        retry(&self.retry_policy, || self.request(endpoint.clone())).await
    }

    async fn request(&self, endpoint: url::Url) -> Result<SearxngResponse, SearchError> {
        let response = self
            .http
            .client()
            .get(endpoint)
            .send()
            .await
            .map_err(|e| classify_transport(PROVIDER_ID, &e))?;
        if !response.status().is_success() {
            return Err(classify_status(PROVIDER_ID, &response));
        }
        self.http.read_json(PROVIDER_ID, response).await
    }

    fn build_endpoint(&self, query: &SearchQuery, pageno: u32) -> Result<url::Url, SearchError> {
        let mut endpoint = self
            .base_url
            .join("search")
            .map_err(|e| SearchError::Config {
                message: format!("cannot build /search endpoint: {e}"),
            })?;
        let categories = build_categories(&query.sources);
        let safesearch = query.safesearch.as_u8().to_string();
        let page_string = pageno.to_string();
        {
            let mut pairs = endpoint.query_pairs_mut();
            pairs.append_pair("q", &query.query);
            pairs.append_pair("format", "json");
            pairs.append_pair("pageno", &page_string);
            pairs.append_pair("safesearch", &safesearch);
            if !categories.is_empty() {
                pairs.append_pair("categories", &categories);
            }
            if let Some(tr) = query.time_range {
                pairs.append_pair("time_range", tr.as_searxng());
            }
            if let Some(lang) = &query.language {
                pairs.append_pair("language", lang);
            }
        }
        Ok(endpoint)
    }
}

/// Typed builder for [`SearXNG`].
#[derive(Debug)]
pub struct SearXNGBuilder {
    base_url: String,
    http: Option<HttpConfig>,
    retry: Option<RetryPolicy>,
}

impl SearXNGBuilder {
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
    pub fn build(self) -> Result<SearXNG, SearchError> {
        let base_url = url::Url::parse(&self.base_url).map_err(|e| SearchError::Config {
            message: format!("invalid SearXNG base URL: {e}"),
        })?;
        Ok(SearXNG {
            http: super::common::resolve_http(self.http)?,
            base_url,
            retry_policy: super::common::resolve_retry(self.retry),
        })
    }
}

fn build_categories(sources: &[SearchSource]) -> String {
    let mut out = Vec::with_capacity(sources.len());
    for src in sources {
        let cat = match src {
            SearchSource::Web => "general",
            SearchSource::News => "news",
            SearchSource::Images => "images",
        };
        if !out.contains(&cat) {
            out.push(cat);
        }
    }
    out.join(",")
}

fn classify_source(category: Option<&str>) -> SearchSource {
    match category {
        Some("news") => SearchSource::News,
        Some("images") => SearchSource::Images,
        _ => SearchSource::Web,
    }
}

#[derive(Debug, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Debug, Deserialize)]
struct SearxngResult {
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
}

impl SearxngResult {
    fn into_hit(self) -> SearchHit {
        SearchHit {
            title: self.title,
            url: self.url,
            description: self.content.filter(|s| !s.is_empty()),
            published_at: self.published_date,
            engine: self.engine,
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn happy_path_maps_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "rust"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    {
                        "url": "https://rust-lang.org",
                        "title": "Rust",
                        "content": "A language empowering everyone.",
                        "engine": "google",
                        "category": "general"
                    },
                    {
                        "url": "https://news.example.com/rust",
                        "title": "Rust news",
                        "content": "Latest",
                        "engine": "bing",
                        "category": "news"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let provider = SearXNG::new(server.uri()).unwrap();
        let response = provider.search(&SearchQuery::new("rust")).await.unwrap();

        assert_eq!(response.provider, "searxng");
        assert_eq!(response.web.len(), 1);
        assert_eq!(response.news.len(), 1);
        assert_eq!(response.web[0].title, "Rust");
        assert_eq!(
            response.web[0].description.as_deref(),
            Some("A language empowering everyone.")
        );
        assert_eq!(response.web[0].engine.as_deref(), Some("google"));
    }

    #[tokio::test]
    async fn limit_truncates_results() {
        let server = MockServer::start().await;
        let results: Vec<_> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "url": format!("https://example.com/{i}"),
                    "title": format!("Result {i}"),
                    "content": "snippet",
                    "category": "general"
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": results })),
            )
            .mount(&server)
            .await;

        let provider = SearXNG::new(server.uri()).unwrap();
        let response = provider
            .search(&SearchQuery::new("x").with_limit(5))
            .await
            .unwrap();
        assert_eq!(response.total(), 5);
    }

    #[tokio::test]
    async fn server_error_is_classified_not_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let provider = SearXNG::builder(server.uri())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let err = provider.search(&SearchQuery::new("x")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::ServerError {
                provider: "searxng",
                status: 503
            }
        ));
    }

    #[tokio::test]
    async fn rate_limited_surfaces_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "2"))
            .mount(&server)
            .await;

        let provider = SearXNG::builder(server.uri())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let err = provider.search(&SearchQuery::new("x")).await.unwrap_err();
        assert!(matches!(
            err,
            SearchError::RateLimited {
                provider: "searxng",
                retry_after: Some(d),
            } if d == std::time::Duration::from_secs(2)
        ));
    }

    #[tokio::test]
    async fn paginates_when_limit_exceeds_first_page() {
        let server = MockServer::start().await;
        let page1: Vec<_> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "url": format!("https://example.com/{i}"),
                    "title": format!("R{i}"),
                    "category": "general"
                })
            })
            .collect();
        let page2: Vec<_> = (30..45)
            .map(|i| {
                serde_json::json!({
                    "url": format!("https://example.com/{i}"),
                    "title": format!("R{i}"),
                    "category": "general"
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("pageno", "1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": page1 })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("pageno", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": page2 })),
            )
            .mount(&server)
            .await;

        let provider = SearXNG::new(server.uri()).unwrap();
        let response = provider
            .search(&SearchQuery::new("x").with_limit(45))
            .await
            .unwrap();
        assert_eq!(response.total(), 45);
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let provider = SearXNG::new("https://searx.example.org").unwrap();
        let err = provider.search(&SearchQuery::new("")).await.unwrap_err();
        assert!(matches!(err, SearchError::Config { .. }));
    }

    #[test]
    fn bad_base_url_is_config_error() {
        let err = SearXNG::new("not a url").unwrap_err();
        assert!(matches!(err, SearchError::Config { .. }));
    }
}
