//! Low-level HTTP plumbing for the `DuckDuckGo` provider.
//!
//! Owns the rotating user-agent pool, the form-encoded POST requests against
//! the HTML and lite endpoints, and the CAPTCHA-aware body finalisation.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Response;

use super::PROVIDER_ID;
use super::captcha::looks_like_captcha;
use super::params::{
    accept_header, accept_language, region_code, safesearch_token, time_range_token,
};
use crate::error::SearchError;
use crate::http::HttpConfig;
use crate::providers::common::{classify_status, classify_transport};
use crate::query::SearchQuery;

/// `DuckDuckGo` returns roughly 25–30 results per page.
pub(super) const RESULTS_PER_PAGE: u32 = 25;

/// Rotating user-agent pool (Chrome 121+, Firefox 123+, Safari 17+, Edge 121+).
///
/// A diverse, realistic pool dramatically reduces anomaly-detection hits on
/// `DuckDuckGo`'s HTML endpoint compared to a single hard-coded UA.
const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:123.0) Gecko/20100101 Firefox/123.0",
    "Mozilla/5.0 (X11; Linux x86_64; rv:122.0) Gecko/20100101 Firefox/122.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.2; rv:123.0) Gecko/20100101 Firefox/123.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36 Edg/121.0.0.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1",
];

const _: () = assert!(!USER_AGENTS.is_empty(), "USER_AGENTS must be non-empty");

/// Picks a user agent from the rotation pool using wall-clock nanoseconds.
///
/// Nano-resolution is sufficient for cheap UA rotation without pulling in a
/// random-number dependency. The `.is_empty()` invariant is asserted at
/// compile time, so the indexing is infallible.
#[allow(
    clippy::indexing_slicing,
    reason = "USER_AGENTS non-empty invariant enforced by const assert; idx bounded by modulo"
)]
fn pick_user_agent() -> &'static str {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    let idx = nanos % USER_AGENTS.len();
    USER_AGENTS[idx]
}

/// POSTs a search request to the html.duckduckgo.com endpoint.
pub(super) async fn post_html(
    http: &HttpConfig,
    endpoint: &str,
    query: &SearchQuery,
    offset: u32,
) -> Result<String, SearchError> {
    let offset_str = offset.to_string();
    let kl = region_code(query);
    let safe = safesearch_token(query.safesearch);
    let df = query.time_range.map_or("", time_range_token);

    let mut form: Vec<(&str, &str)> = vec![
        ("q", query.query.as_str()),
        ("b", ""),
        ("kl", &kl),
        ("safe", safe),
    ];
    if offset > 0 {
        form.push(("s", &offset_str));
        form.push(("dc", &offset_str));
        form.push(("v", "l"));
        form.push(("o", "json"));
        form.push(("api", "d.js"));
    }
    if !df.is_empty() {
        form.push(("df", df));
    }

    let response = http
        .client()
        .post(endpoint)
        .header(reqwest::header::USER_AGENT, pick_user_agent())
        .header(reqwest::header::ACCEPT, accept_header())
        .header(reqwest::header::ACCEPT_LANGUAGE, accept_language(query))
        .header(reqwest::header::REFERER, "https://duckduckgo.com/")
        .header("DNT", "1")
        .header("Upgrade-Insecure-Requests", "1")
        .form(&form)
        .send()
        .await
        .map_err(|e| classify_transport(PROVIDER_ID, &e))?;

    finalize_body(http, response).await
}

/// POSTs a search request to the lite.duckduckgo.com endpoint.
pub(super) async fn post_lite(
    http: &HttpConfig,
    endpoint: &str,
    query: &SearchQuery,
) -> Result<String, SearchError> {
    let kl = region_code(query);
    let safe = safesearch_token(query.safesearch);
    let df = query.time_range.map_or("", time_range_token);

    let mut form: Vec<(&str, &str)> =
        vec![("q", query.query.as_str()), ("kl", &kl), ("safe", safe)];
    if !df.is_empty() {
        form.push(("df", df));
    }

    let response = http
        .client()
        .post(endpoint)
        .header(reqwest::header::USER_AGENT, pick_user_agent())
        .header(reqwest::header::ACCEPT, accept_header())
        .header(reqwest::header::ACCEPT_LANGUAGE, accept_language(query))
        .header(reqwest::header::REFERER, "https://lite.duckduckgo.com/")
        .form(&form)
        .send()
        .await
        .map_err(|e| classify_transport(PROVIDER_ID, &e))?;

    finalize_body(http, response).await
}

/// Reads the response body, classifying non-success statuses, 202-soft-blocks,
/// and CAPTCHA challenge pages into structured errors.
async fn finalize_body(http: &HttpConfig, response: Response) -> Result<String, SearchError> {
    // DuckDuckGo uses a successful HTTP status for challenges, regardless of body shape.
    if response.status() == reqwest::StatusCode::ACCEPTED {
        return Err(SearchError::CaptchaDetected {
            provider: PROVIDER_ID,
        });
    }
    if !response.status().is_success() {
        return Err(classify_status(PROVIDER_ID, &response));
    }
    let body = http.read_text(PROVIDER_ID, response).await?;
    if looks_like_captcha(&body) {
        return Err(SearchError::CaptchaDetected {
            provider: PROVIDER_ID,
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_pool_is_populated() {
        for ua in USER_AGENTS {
            assert!(ua.starts_with("Mozilla/5.0"));
        }
    }

    #[test]
    fn pick_user_agent_returns_pool_entry() {
        let ua = pick_user_agent();
        assert!(USER_AGENTS.contains(&ua));
    }
}
