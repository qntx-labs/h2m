//! End-to-end integration tests exercising the full public API surface
//! (client, retry, secret, error) against a local wiremock server.
//!
//! These tests complement the per-provider unit tests by verifying the
//! crate behaves correctly as a black box, including retry semantics and
//! the unified [`SearchError`] taxonomy.

#![allow(
    clippy::indexing_slicing,
    reason = "integration tests should panic on wrong shape"
)]
#![allow(
    clippy::tests_outside_test_module,
    reason = "integration tests are already in a test-only crate"
)]

// dev-deps of the host crate get linked into integration tests by default;
// silence the `unused-crate-dependencies` lint by declaring them explicitly.
#[cfg(any(feature = "brave", feature = "searxng"))]
use std::time::Duration;

#[cfg(feature = "brave")]
use h2m_search::Brave;
#[cfg(any(feature = "brave", feature = "searxng"))]
use h2m_search::RetryPolicy;
#[cfg(feature = "searxng")]
use h2m_search::SearXNG;
use h2m_search::{HttpConfig, SearchClient, SearchError, SearchQuery, SecretString};
use insta as _;
#[cfg(feature = "wikipedia")]
use percent_encoding as _;
use pretty_assertions as _;
use reqwest as _;
#[cfg(feature = "duckduckgo")]
use scraper as _;
use serde as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use url as _;
#[cfg(all(feature = "brave", feature = "searxng"))]
use wiremock::matchers::header;
use wiremock::matchers::path;
#[cfg(any(feature = "brave", feature = "searxng"))]
use wiremock::matchers::{any, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An oversized HTML result fails without falling back or retrying.
#[cfg(feature = "duckduckgo")]
#[tokio::test]
async fn duckduckgo_respects_host_response_limit() {
    let server = MockServer::start().await;
    Mock::given(path("/html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(1024)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/lite"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let http = HttpConfig::new().unwrap().with_max_response_bytes(512);
    let provider = h2m_search::DuckDuckGo::builder()
        .http(http)
        .html_endpoint(format!("{}/html", server.uri()))
        .lite_endpoint(format!("{}/lite", server.uri()))
        .build()
        .unwrap();
    let error = provider
        .search(&SearchQuery::new("rust"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SearchError::ResponseTooLarge {
            provider: "duckduckgo",
            limit_bytes: 512
        }
    ));
}

/// JSON providers use the same host limit through the public client builder.
#[cfg(feature = "searxng")]
#[tokio::test]
async fn json_search_respects_host_response_limit_without_retry() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(" ".repeat(1024)))
        .expect(1)
        .mount(&server)
        .await;
    let http = HttpConfig::new().unwrap().with_max_response_bytes(512);
    let client = SearchClient::builder()
        .provider("searxng")
        .searxng_url(server.uri())
        .http(http)
        .build()
        .unwrap();
    let error = client.search(&SearchQuery::new("rust")).await.unwrap_err();
    assert!(matches!(
        error,
        SearchError::ResponseTooLarge {
            provider: "searxng",
            limit_bytes: 512
        }
    ));
}

/// `SearchClient::builder()` resolves an unknown provider to a dedicated
/// [`SearchError::ProviderUnavailable`] variant instead of a transport error.
#[test]
fn unknown_provider_is_unavailable() {
    let err = SearchClient::builder()
        .provider("yahoo")
        .build()
        .unwrap_err();
    assert!(matches!(err, SearchError::ProviderUnavailable { .. }));
}

/// `SearXNG` happy path through the public [`SearchClient`].
#[tokio::test]
#[cfg(feature = "searxng")]
async fn searxng_via_client_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "url": "https://r.io", "title": "Rust", "content": "lang", "category": "general" }
            ]
        })))
        .mount(&server)
        .await;

    let client = SearchClient::builder()
        .provider("searxng")
        .searxng_url(server.uri())
        .build()
        .unwrap();
    let response = client.search(&SearchQuery::new("rust")).await.unwrap();

    assert_eq!(response.provider, "searxng");
    assert_eq!(response.web.len(), 1);
    assert_eq!(response.web[0].url, "https://r.io");
}

/// Retries a transient 503 and succeeds on the retry attempt.
#[tokio::test]
#[cfg(feature = "searxng")]
async fn retry_policy_recovers_from_transient_5xx() {
    let server = MockServer::start().await;

    // First attempt: 503. Limit to 1 so the second call routes to the
    // next mock.
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "url": "https://ok", "title": "OK", "category": "general" }
            ]
        })))
        .mount(&server)
        .await;

    let client = SearchClient::builder()
        .provider("searxng")
        .searxng_url(server.uri())
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
        .build()
        .unwrap();
    let response = client.search(&SearchQuery::new("q")).await.unwrap();

    assert_eq!(response.web.len(), 1);
    assert_eq!(response.web[0].url, "https://ok");
}

/// A 401 aborts immediately (no retry) and is classified as `AuthFailed`.
#[tokio::test]
#[cfg(feature = "brave")]
async fn brave_auth_failure_short_circuits_retries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(any())
        .respond_with(ResponseTemplate::new(401))
        .expect(1) // retry must NOT trigger
        .mount(&server)
        .await;

    let client = SearchClient::builder()
        .provider("brave")
        .brave_api_key(SecretString::new("KEY"))
        .retry(RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
        .build()
        .unwrap();

    // Trick the client into hitting our mock: re-wire by using the Brave
    // provider directly through the builder. (For this test we use
    // Brave::with_base_url to bypass the canonical URL.)
    let provider = Brave::builder("KEY")
        .base_url(server.uri())
        .retry(RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
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
    drop(client);
}

/// A 429 with `Retry-After` surfaces the parsed duration.
#[tokio::test]
#[cfg(feature = "searxng")]
async fn rate_limited_with_retry_after_is_exposed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3"))
        .mount(&server)
        .await;

    let provider = SearXNG::builder(server.uri())
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let err = provider.search(&SearchQuery::new("q")).await.unwrap_err();

    assert!(matches!(
        err,
        SearchError::RateLimited {
            provider: "searxng",
            retry_after: Some(d),
        } if d == Duration::from_secs(3)
    ));
}

/// Two providers sharing a single [`HttpConfig`] verify the connection pool
/// is effectively shared (we can't easily assert the pool identity from
/// outside, but we can verify both providers function correctly).
#[tokio::test]
#[cfg(all(feature = "searxng", feature = "brave"))]
async fn shared_http_config_works_across_providers() {
    let searxng_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": []
        })))
        .mount(&searxng_server)
        .await;

    let brave_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(header("X-Subscription-Token", "KEY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "web": { "results": [] },
            "news": { "results": [] }
        })))
        .mount(&brave_server)
        .await;

    let http = HttpConfig::new().unwrap();
    let searxng = SearXNG::builder(searxng_server.uri())
        .http(http.clone())
        .build()
        .unwrap();
    let brave = Brave::builder("KEY")
        .base_url(brave_server.uri())
        .http(http)
        .build()
        .unwrap();

    let _ = searxng.search(&SearchQuery::new("a")).await.unwrap();
    let _ = brave.search(&SearchQuery::new("b")).await.unwrap();
}

/// `SearchError` serialises with the stable JSON contract used by the CLI.
#[test]
fn error_json_contract() {
    let err = SearchError::RateLimited {
        provider: "tavily",
        retry_after: None,
    };
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(json["kind"], "rateLimited");
    assert_eq!(json["provider"], "tavily");
    assert_eq!(json["status"], 429);
}

/// `SecretString` redacts in Debug and Display.
#[test]
fn secret_string_is_never_leaked() {
    let s = SecretString::new("hunter2");
    assert!(!format!("{s:?}").contains("hunter2"));
    assert!(!format!("{s}").contains("hunter2"));
    assert_eq!(s.expose(), "hunter2");
}
