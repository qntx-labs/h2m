//! # h2m-search
//!
//! Zero-config web search provider abstraction for
//! [`h2m`](https://crates.io/crates/h2m). The default providers
//! (`duckduckgo`, `wikipedia`) need **no API key** and **no environment
//! variables** — `SearchClient::from_env()` works out of the box.
//!
//! Provides a unified interface over multiple search backends. The design
//! mirrors the Firecrawl `/search` endpoint — default results carry only
//! `title`/`url`/`description`, while the companion CLI `--scrape` flag
//! funnels hits through the existing `h2m::scrape::Scraper` pipeline.
//!
//! ## Providers
//!
//! Each provider lives behind a Cargo feature so binaries can opt into exactly
//! the backends they need:
//!
//! | Feature       | Provider     | Requires              | Default |
//! | ------------- | ------------ | --------------------- | ------- |
//! | `duckduckgo`  | `DuckDuckGo` | — (zero-config)      | **yes** |
//! | `wikipedia`   | Wikipedia    | — (zero-config)      | **yes** |
//! | `searxng`     | `SearXNG`    | `H2M_SEARXNG_URL`     | no      |
//! | `brave`       | Brave        | `BRAVE_API_KEY`       | no      |
//! | `tavily`      | Tavily       | `TAVILY_API_KEY`      | no      |
//! | `all`         | —            | aggregates the above  | no      |
//!
//! ## Quick start
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use h2m_search::{SearchClient, SearchQuery};
//!
//! // Zero configuration: defaults to DuckDuckGo, no env vars needed.
//! let client = SearchClient::from_env()?;
//! let response = client
//!     .search(&SearchQuery::new("rust async trait").with_limit(5))
//!     .await?;
//!
//! for hit in &response.web {
//!     println!("{} — {}", hit.title, hit.url);
//! }
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

#[cfg(not(any(
    feature = "duckduckgo",
    feature = "wikipedia",
    feature = "searxng",
    feature = "brave",
    feature = "tavily"
)))]
compile_error!(
    "h2m-search requires at least one provider feature: 'duckduckgo' (default), 'wikipedia' (default), 'searxng', 'brave', or 'tavily'"
);

mod client;
mod error;
mod http;
mod providers;
mod query;
mod response;
mod retry;
mod secret;

#[cfg(feature = "brave")]
pub use client::ENV_BRAVE_API_KEY;
#[cfg(feature = "searxng")]
pub use client::ENV_SEARXNG_URL;
#[cfg(feature = "tavily")]
pub use client::ENV_TAVILY_API_KEY;
pub use client::{ENV_PROVIDER, ProviderId, SearchClient, SearchClientBuilder};
pub use error::SearchError;
pub use http::{DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, HttpConfig, USER_AGENT};
#[cfg(test)]
use insta as _;
#[cfg(test)]
use pretty_assertions as _;
#[cfg(feature = "brave")]
pub use providers::Brave;
#[cfg(feature = "duckduckgo")]
pub use providers::DuckDuckGo;
#[cfg(feature = "searxng")]
pub use providers::SearXNG;
#[cfg(feature = "tavily")]
pub use providers::Tavily;
#[cfg(feature = "wikipedia")]
pub use providers::Wikipedia;
pub use query::{SafeSearch, SearchQuery, SearchSource, TimeRange};
pub use response::{SearchHit, SearchResponse};
pub use retry::RetryPolicy;
pub use secret::SecretString;
#[cfg(test)]
use serde_json as _;
#[cfg(test)]
use wiremock as _;
