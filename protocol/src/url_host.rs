use std::borrow::Cow;

use url::{Host, Url};

/// Canonical host of a browser URL accepted for a server-authored OSC link.
/// A written, nonempty authority is required even when WHATWG recovery would
/// otherwise infer one.
#[must_use]
pub fn link_url_host(url: &str) -> Option<String> {
    let normalized = if url.contains(['\t', '\n', '\r']) {
        Cow::Owned(
            url.chars()
                .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
                .collect::<String>(),
        )
    } else {
        Cow::Borrowed(url)
    };
    let (_, raw_authority) = normalized.split_once("://")?;
    if raw_authority.starts_with(['/', '\\', '?', '#']) {
        return None;
    }
    let parsed = Url::parse(&normalized).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ftp") {
        return None;
    }
    match parsed.host()? {
        Host::Domain(host) => Some(host.to_string()),
        Host::Ipv4(host) => Some(host.to_string()),
        Host::Ipv6(host) => Some(host.to_string()),
    }
}
