//! Process-wide shared `reqwest::Client`s for sampling.
//!
//! Sharing is safe because the builders take no per-request input (auth, extra
//! headers, base URL, and User-Agent are applied per-request in `SamplingClient::post`).
//! The only process-wide input is the egress proxy, latched once at startup via
//! [`set_egress_proxy`] before the first client is built.
//! Stale connections are bounded by h2 keepalive (15s ping, 5s timeout), 90s idle-pool eviction, and the pool-less HTTP/1.1 first-retry rebuild.
//! Connections whose per-session runtime died are discarded by hyper's checkout ready-check, with the retry loop covering the rest.
//!
//! Wire behavior is pinned by the `shared_http_wire` and `shared_http_kill_switch` binaries.
//! `GROK_EXTRA_CA_BUNDLE` adds extra CA roots.

use std::sync::OnceLock;
use std::time::Duration;

static SHARED_H2: OnceLock<reqwest::Client> = OnceLock::new();
static SHARED_HTTP1: OnceLock<reqwest::Client> = OnceLock::new();

/// Egress proxy URL for sampling traffic, latched once at startup.
/// `None` (default) means direct connections.
static EGRESS_PROXY: OnceLock<Option<String>> = OnceLock::new();

/// Supported egress proxy schemes (`reqwest::Proxy::all` handles both;
/// the `socks` reqwest feature is enabled workspace-wide).
const EGRESS_PROXY_SCHEMES: &[&str] = &["http://", "https://", "socks5://", "socks5h://"];

/// Validate + normalize a proxy URL candidate. Accepts `http(s)://` and
/// `socks5(h)://` (credentials may be embedded as `user:pass@`).
/// Returns `None` for empty/unsupported values. Pure: unit-tested below.
// FORK: added in this fork for `[proxy] url` / `GROK_PROXY_URL` support.
fn normalize_egress_proxy(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Schemes are case-insensitive (RFC 3986 §3.1); lowercase the scheme so
    // downstream URL parsing never trips on `SOCKS5H://…`. Remainder (with
    // possible credentials) is preserved verbatim.
    let lower = trimmed.to_ascii_lowercase();
    let scheme_len = EGRESS_PROXY_SCHEMES
        .iter()
        .find(|scheme| lower.starts_with(*scheme))
        .map(|scheme| scheme.len());
    let Some(scheme_len) = scheme_len else {
        // Never log the raw value: it may embed `user:pass@` credentials.
        let safe_proxy = trimmed.rsplit('@').next().unwrap_or(trimmed);
        tracing::warn!(
            proxy = %safe_proxy,
            "ignoring egress proxy: scheme must be http(s):// or socks5(h)://"
        );
        return None;
    };
    let mut normalized = lower[..scheme_len].to_string();
    normalized.push_str(&trimmed[scheme_len..]);
    Some(normalized)
}

/// Latch the egress proxy for all sampling clients built afterwards.
/// Called once from agent startup (`init_process`, every agent mode passes
/// through it) with the resolved `[proxy] url` / `GROK_PROXY_URL` value.
/// First call wins; later calls (e.g. subagent re-bootstrap in one process)
/// are ignored, matching the other read-once knobs in this file.
/// A `None`/invalid value latches direct connections.
// FORK: added in this fork for `[proxy] url` / `GROK_PROXY_URL` support.
pub fn set_egress_proxy(url: Option<String>) {
    let normalized = url.as_deref().and_then(normalize_egress_proxy);
    let _ = EGRESS_PROXY.set(normalized);
}

/// The latched egress proxy, if any.
fn egress_proxy() -> Option<String> {
    EGRESS_PROXY.get().cloned().flatten()
}

/// Apply the latched egress proxy to a client builder, if one is set.
/// An unparseable URL warns and falls back to direct (a bad proxy setting
/// must not brick the tool; validation already happened in [`set_egress_proxy`]).
fn apply_egress_proxy(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let Some(proxy_url) = egress_proxy() else {
        return builder;
    };
    match reqwest::Proxy::all(&proxy_url) {
        Ok(proxy) => builder.proxy(proxy),
        Err(e) => {
            tracing::warn!(error = %e, "egress proxy unusable, connecting directly");
            builder
        }
    }
}

/// Kill switch: `GROK_SAMPLER_SHARED_CLIENT=0` (or `false`, any case) builds a fresh `reqwest::Client` per `SamplingClient` instead.
/// Resolved once per process: the environment cannot change externally after spawn.
/// Latching keeps the rollback consistent with the pool knobs, which are also read only once.
fn sharing_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        let disabled = match std::env::var("GROK_SAMPLER_SHARED_CLIENT") {
            Ok(v) => v == "0" || v.eq_ignore_ascii_case("false"),
            Err(_) => false,
        };
        if disabled {
            tracing::info!("sampler HTTP client sharing disabled via GROK_SAMPLER_SHARED_CLIENT");
        }
        disabled
    })
}

/// Clone the shared client out of `cell`, building it on first use.
/// Build failures are not cached: on `Err` the cell stays empty and the next call retries.
/// A racing loser's freshly built client is dropped.
fn shared(
    cell: &OnceLock<reqwest::Client>,
    build: fn() -> Result<reqwest::Client, reqwest::Error>,
    disabled: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    if disabled {
        return build();
    }
    if let Some(client) = cell.get() {
        return Ok(client.clone());
    }
    let built = build()?;
    Ok(cell.get_or_init(|| built).clone())
}

/// Shared HTTP/2 sampling client (connection pooling and h2 keepalive).
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    shared(&SHARED_H2, build_http_client, sharing_disabled())
}

pub(crate) enum PooledClient {
    SharingDisabled,
    Unavailable(reqwest::Error),
    Ready(reqwest::Client),
}

/// The pooled client worth prewarming, or why there is none.
pub(crate) fn pooled_client() -> PooledClient {
    if sharing_disabled() {
        return PooledClient::SharingDisabled;
    }
    match client() {
        Ok(client) => PooledClient::Ready(client),
        Err(error) => PooledClient::Unavailable(error),
    }
}

/// Idle timeout the shared pool evicts after; read once so prewarm's re-warm window stays in step.
pub(crate) fn pool_idle_timeout() -> Duration {
    static SECS: OnceLock<u64> = OnceLock::new();
    Duration::from_secs(*SECS.get_or_init(|| {
        std::env::var("GROK_POOL_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90)
    }))
}

/// Shared HTTP/1.1 fallback client.
/// It has no connection pool, so sharing it behaves the same as building a fresh one.
pub(crate) fn client_http1() -> Result<reqwest::Client, reqwest::Error> {
    shared(&SHARED_HTTP1, build_http_client_http1, sharing_disabled())
}

/// Build a `reqwest::Client` for sampling with HTTP/2 and connection pooling.
/// Env knobs are read once, when the shared client is first built.
fn build_http_client() -> Result<reqwest::Client, reqwest::Error> {
    let pool_max_idle: usize = std::env::var("GROK_POOL_MAX_IDLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let connect_timeout_secs: u64 = std::env::var("GROK_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    xai_grok_extra_ca::build_reqwest_client(|builder| {
        // FORK: route sampling traffic through the latched egress proxy, if set.
        let builder = apply_egress_proxy(builder);
        builder
            .pool_max_idle_per_host(pool_max_idle)
            .pool_idle_timeout(pool_idle_timeout())
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(15))
            .http2_keep_alive_timeout(Duration::from_secs(5))
            .http2_keep_alive_while_idle(true)
    })
}

/// Build a `reqwest::Client` constrained to HTTP/1.1 with pooling disabled.
/// Used as a fallback after HTTP/2 transport failures.
fn build_http_client_http1() -> Result<reqwest::Client, reqwest::Error> {
    let connect_timeout_secs: u64 = std::env::var("GROK_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    xai_grok_extra_ca::build_reqwest_client(|builder| {
        // FORK: route sampling traffic through the latched egress proxy, if set.
        let builder = apply_egress_proxy(builder);
        builder
            .http1_only()
            .pool_max_idle_per_host(0)
            .pool_idle_timeout(Duration::from_secs(0))
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .tcp_nodelay(true)
    })
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{normalize_egress_proxy, shared};

    // FORK: `[proxy] url` validation. Pure function tests only — the process
    // latch (`set_egress_proxy`) is first-wins global state and must not be
    // touched by unit tests.
    #[test]
    fn normalize_egress_proxy_accepts_http_and_socks() {
        assert_eq!(
            normalize_egress_proxy("http://proxy.corp:8080"),
            Some("http://proxy.corp:8080".to_string())
        );
        assert_eq!(
            normalize_egress_proxy("https://proxy.corp:8443"),
            Some("https://proxy.corp:8443".to_string())
        );
        assert_eq!(
            normalize_egress_proxy("socks5://127.0.0.1:1080"),
            Some("socks5://127.0.0.1:1080".to_string())
        );
        assert_eq!(
            normalize_egress_proxy("socks5h://127.0.0.1:1080"),
            Some("socks5h://127.0.0.1:1080".to_string())
        );
        // Credentials embedded in the URL are preserved verbatim.
        assert_eq!(
            normalize_egress_proxy("http://user:p%40ss@proxy.corp:8080"),
            Some("http://user:p%40ss@proxy.corp:8080".to_string())
        );
        // Case-insensitive scheme (normalized to lowercase), surrounding
        // whitespace trimmed.
        assert_eq!(
            normalize_egress_proxy("  SOCKS5H://proxy.corp:1080  "),
            Some("socks5h://proxy.corp:1080".to_string())
        );
    }

    #[test]
    fn normalize_egress_proxy_rejects_empty_and_unknown_schemes() {
        assert_eq!(normalize_egress_proxy(""), None);
        assert_eq!(normalize_egress_proxy("   "), None);
        assert_eq!(normalize_egress_proxy("proxy.corp:8080"), None);
        assert_eq!(normalize_egress_proxy("ftp://proxy.corp:21"), None);
        assert_eq!(normalize_egress_proxy("socks4://127.0.0.1:1080"), None);
    }

    static BUILD_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Fails on the first call (a real `reqwest::Error`, no I/O), then builds.
    fn flaky_build() -> Result<reqwest::Client, reqwest::Error> {
        if BUILD_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(reqwest::Proxy::all("not a proxy url").unwrap_err());
        }
        reqwest::Client::builder().build()
    }

    #[test]
    fn shared_does_not_cache_build_failures() {
        static CELL: OnceLock<reqwest::Client> = OnceLock::new();
        assert!(shared(&CELL, flaky_build, false).is_err());
        assert!(CELL.get().is_none(), "failure must leave the cell empty");
        assert!(shared(&CELL, flaky_build, false).is_ok());
        assert!(CELL.get().is_some(), "success must populate the cell");
        assert!(shared(&CELL, flaky_build, false).is_ok());
        assert_eq!(
            BUILD_CALLS.load(Ordering::SeqCst),
            2,
            "third call must reuse the cached client, not rebuild"
        );
    }

    #[test]
    fn shared_disabled_bypasses_cell() {
        static CELL: OnceLock<reqwest::Client> = OnceLock::new();
        assert!(shared(&CELL, || reqwest::Client::builder().build(), true).is_ok());
        assert!(
            CELL.get().is_none(),
            "disabled mode must never touch the cell"
        );
    }
}
