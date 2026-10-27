//! Wikipedia provider.
//!
//! Uses the public [MediaWiki Search API][api] — free, unauthenticated,
//! unrate-limited for reasonable usage, and officially supported.
//!
//! Unlike scraping-based providers, this talks to a stable JSON endpoint
//! maintained by the Wikimedia Foundation. Ideal for academic / encyclopedic
//! queries and as a fallback when general-web providers fail.
//!
//! # Language
//!
//! The provider is language-aware: set [`WikipediaBuilder::language`] to
//! any language code Wikipedia supports (`en`, `zh`, `es`, `ja`, …). The
//! default is `en`.
//!
//! [api]: https://www.mediawiki.org/wiki/API:Search
//!
//! Enable the `wikipedia` feature (on by default) to use this provider.

use std::time::Instant;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::Deserialize;
use tracing::instrument;

use super::common::{classify_status, classify_transport};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::query::{SearchQuery, SearchSource};
use crate::response::{SearchHit, SearchResponse};
use crate::retry::{RetryPolicy, retry};

const PROVIDER_ID: &str = "wikipedia";
const DEFAULT_LANGUAGE: &str = "en";
const MAX_SRLIMIT: usize = 50;

/// Characters that must be percent-encoded in a Wikipedia article path.
///
/// We intentionally preserve `(`, `)`, `,`, `'` etc. because Wikipedia URLs
/// carry them unescaped (`/wiki/Rust_(programming_language)`). Only the
/// URL-reserved trio `#`, `?`, `%` plus whitespace need escaping.
const WIKI_PATH_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'#')
    .add(b'?')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'"')
    .add(b'`')
    .add(b'^')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\');

/// Wikipedia search provider.
#[derive(Debug, Clone)]
pub struct Wikipedia {
    http: HttpConfig,
    language: String,
    retry_policy: RetryPolicy,
}

impl Wikipedia {
    /// Creates a new provider with default settings (English Wikipedia,
    /// production retry policy, shared HTTP client).
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
    pub fn builder() -> WikipediaBuilder {
        WikipediaBuilder::default()
    }

    /// Executes a Wikipedia search.
    ///
    /// All hits are classified as [`SearchSource::Web`] — Wikipedia does not
    /// expose a news/images category via this API.
    ///
    /// # Errors
    ///
    /// Returns the specific [`SearchError`] produced by the HTTP layer or
    /// [`SearchError::ParseFailed`] if the JSON shape is unexpected.
    #[instrument(level = "debug", skip(self), fields(provider = PROVIDER_ID))]
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResponse, SearchError> {
        if !query.is_valid() {
            return Err(SearchError::Config {
                message: "search query is empty".into(),
            });
        }

        let start = Instant::now();
        let effective_lang = query.language.as_deref().unwrap_or(&self.language);
        let endpoint = build_endpoint(effective_lang)?;
        let srlimit = query.limit.min(MAX_SRLIMIT).to_string();
        let body = retry(&self.retry_policy, || {
            self.fetch(endpoint.clone(), &query.query, &srlimit)
        })
        .await?;

        let mut response = SearchResponse::new(&query.query, PROVIDER_ID);
        let mut hits: Vec<_> = body
            .query
            .search
            .into_iter()
            .map(|r| r.into_hit(effective_lang))
            .collect();
        hits.truncate(query.limit);
        for hit in hits {
            response.push(SearchSource::Web, hit);
        }
        response.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(response)
    }

    async fn fetch(
        &self,
        endpoint: url::Url,
        srsearch: &str,
        srlimit: &str,
    ) -> Result<ApiResponse, SearchError> {
        let response = self
            .http
            .client()
            .get(endpoint)
            .query(&[
                ("action", "query"),
                ("list", "search"),
                ("srsearch", srsearch),
                ("srlimit", srlimit),
                ("srprop", "snippet|timestamp|wordcount"),
                ("format", "json"),
                ("formatversion", "2"),
                ("utf8", "1"),
                ("origin", "*"),
            ])
            .send()
            .await
            .map_err(|e| classify_transport(PROVIDER_ID, &e))?;

        if !response.status().is_success() {
            return Err(classify_status(PROVIDER_ID, &response));
        }
        self.http.read_json(PROVIDER_ID, response).await
    }
}

/// Typed builder for [`Wikipedia`].
#[derive(Debug, Default)]
pub struct WikipediaBuilder {
    language: Option<String>,
    http: Option<HttpConfig>,
    retry: Option<RetryPolicy>,
}

impl WikipediaBuilder {
    /// Sets the Wikipedia language edition (e.g. `"en"`, `"zh"`, `"ja"`).
    ///
    /// Empty or whitespace-only strings are ignored. Per-query
    /// [`SearchQuery::language`] takes precedence over this default.
    #[must_use]
    pub fn language(mut self, lang: impl Into<String>) -> Self {
        let value = lang.into();
        if !value.trim().is_empty() {
            self.language = Some(value);
        }
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
    /// Returns [`SearchError::Config`] if the default HTTP client cannot be
    /// constructed.
    pub fn build(self) -> Result<Wikipedia, SearchError> {
        Ok(Wikipedia {
            http: super::common::resolve_http(self.http)?,
            language: self.language.unwrap_or_else(|| DEFAULT_LANGUAGE.into()),
            retry_policy: super::common::resolve_retry(self.retry),
        })
    }
}

fn build_endpoint(language: &str) -> Result<url::Url, SearchError> {
    let trimmed = language.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(SearchError::Config {
            message: format!("invalid Wikipedia language code: {language:?}"),
        });
    }
    url::Url::parse(&format!("https://{trimmed}.wikipedia.org/w/api.php")).map_err(|e| {
        SearchError::Config {
            message: format!("cannot build Wikipedia endpoint: {e}"),
        }
    })
}

fn article_url(language: &str, title: &str) -> String {
    let with_underscores = title.replace(' ', "_");
    let encoded = utf8_percent_encode(&with_underscores, WIKI_PATH_SET);
    format!("https://{language}.wikipedia.org/wiki/{encoded}")
}

/// Strips Wikipedia's `<span class="searchmatch">` highlight tags and
/// decodes the handful of HTML entities the search snippets actually use.
fn clean_snippet(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    query: QueryBlock,
}

#[derive(Debug, Deserialize)]
struct QueryBlock {
    #[serde(default)]
    search: Vec<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    title: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    timestamp: Option<String>,
}

impl SearchResult {
    fn into_hit(self, language: &str) -> SearchHit {
        let url = article_url(language, &self.title);
        let description = clean_snippet(&self.snippet);
        SearchHit {
            title: self.title,
            url,
            description: (!description.is_empty()).then_some(description),
            published_at: self.timestamp,
            engine: Some(PROVIDER_ID.into()),
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

    /// Drives `fetch` against a wiremock server, then maps results using
    /// the same logic as [`Wikipedia::search`] but with a fixed endpoint.
    async fn run_against_mock(
        server: &MockServer,
        query: &SearchQuery,
    ) -> Result<SearchResponse, SearchError> {
        let provider = Wikipedia::builder()
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let endpoint = url::Url::parse(&format!("{}/w/api.php", server.uri())).unwrap();
        let srlimit = query.limit.min(MAX_SRLIMIT).to_string();
        let body = provider.fetch(endpoint, &query.query, &srlimit).await?;
        let mut response = SearchResponse::new(&query.query, PROVIDER_ID);
        let mut hits: Vec<_> = body
            .query
            .search
            .into_iter()
            .map(|r| r.into_hit("en"))
            .collect();
        hits.truncate(query.limit);
        for hit in hits {
            response.push(SearchSource::Web, hit);
        }
        Ok(response)
    }

    #[tokio::test]
    async fn happy_path_maps_fields_and_builds_article_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/w/api.php"))
            .and(query_param("action", "query"))
            .and(query_param("list", "search"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "query": {
                    "search": [
                        {
                            "title": "Rust (programming language)",
                            "snippet": "<span class=\"searchmatch\">Rust</span> is a systems language.",
                            "timestamp": "2026-04-15T12:00:00Z",
                            "wordcount": 15000
                        },
                        {
                            "title": "Turing machine",
                            "snippet": "A model of computation.",
                            "timestamp": "2026-03-01T00:00:00Z"
                        }
                    ]
                }
            })))
            .mount(&server)
            .await;

        let response = run_against_mock(&server, &SearchQuery::new("rust"))
            .await
            .unwrap();

        assert_eq!(response.provider, "wikipedia");
        assert_eq!(response.web.len(), 2);
        assert_eq!(response.web[0].title, "Rust (programming language)");
        assert_eq!(
            response.web[0].url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert_eq!(
            response.web[0].description.as_deref(),
            Some("Rust is a systems language.")
        );
        assert_eq!(
            response.web[0].published_at.as_deref(),
            Some("2026-04-15T12:00:00Z")
        );
        assert_eq!(response.web[0].engine.as_deref(), Some("wikipedia"));
    }

    #[tokio::test]
    async fn limit_truncates_results() {
        let server = MockServer::start().await;
        let search: Vec<_> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "title": format!("Article {i}"),
                    "snippet": "",
                    "timestamp": "2026-01-01T00:00:00Z"
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/w/api.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "query": { "search": search }
            })))
            .mount(&server)
            .await;

        let response = run_against_mock(&server, &SearchQuery::new("x").with_limit(5))
            .await
            .unwrap();
        assert_eq!(response.web.len(), 5);
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let provider = Wikipedia::new().unwrap();
        let err = provider.search(&SearchQuery::new("")).await.unwrap_err();
        assert!(matches!(err, SearchError::Config { .. }));
    }

    #[test]
    fn article_url_escapes_special_chars_but_preserves_parens() {
        assert_eq!(
            article_url("en", "Rust (programming language)"),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert_eq!(
            article_url("zh", "Rust语言"),
            "https://zh.wikipedia.org/wiki/Rust%E8%AF%AD%E8%A8%80"
        );
        // `#` and `?` must be escaped — they would otherwise terminate the path.
        assert_eq!(
            article_url("en", "C# (programming language)"),
            "https://en.wikipedia.org/wiki/C%23_(programming_language)"
        );
    }

    #[test]
    fn clean_snippet_strips_highlight_tags_and_decodes_entities() {
        assert_eq!(
            clean_snippet("<span class=\"searchmatch\">Rust</span> &amp; Go"),
            "Rust & Go"
        );
        assert_eq!(
            clean_snippet("Nested <b><i>tags</i></b> removed"),
            "Nested tags removed"
        );
        assert_eq!(clean_snippet("Plain text"), "Plain text");
        assert_eq!(clean_snippet("  trimmed  "), "trimmed");
    }

    #[test]
    fn build_endpoint_rejects_invalid_language() {
        assert!(build_endpoint("").is_err());
        assert!(build_endpoint("en/../evil").is_err());
        assert!(build_endpoint(" ").is_err());
        assert!(build_endpoint("en").is_ok());
        assert!(build_endpoint("zh-tw").is_ok());
    }

    #[tokio::test]
    async fn server_error_is_classified() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/w/api.php"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = run_against_mock(&server, &SearchQuery::new("x"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SearchError::ServerError {
                provider: "wikipedia",
                status: 503,
            }
        ));
    }
}
