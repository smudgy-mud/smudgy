//! Validation of the WSS address shared by desktop and browser Connect flows.

/// Parse a binary-Telnet-over-WSS URL without discarding its path or query.
/// The returned host and port are only for connection metadata; transports
/// must still use the original URL.
///
/// # Errors
/// Rejects malformed or non-WSS URLs, credentials, fragments, and invalid ports.
pub fn parse_address(endpoint: &str) -> Result<(String, u16), &'static str> {
    let url = url::Url::parse(endpoint).map_err(|_| "Invalid WSS URL")?;
    let host = url.host_str().ok_or("WSS URL must have a host")?;
    if url.scheme() != "wss"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("WSS URL must have a host, no credentials, and no fragment");
    }
    let port = url
        .port_or_known_default()
        .ok_or("WSS URL has no valid port")?;
    if port == 0 {
        return Err("WSS URL port must be between 1 and 65535");
    }
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_full_wss_url_and_rejects_unsafe_authorities() {
        assert_eq!(
            parse_address("wss://last-outpost.com/ws/telnet/?profile=one"),
            Ok(("last-outpost.com".into(), 443))
        );
        for invalid in [
            "ws://example.org/ws",
            "wss://user:secret@example.org/ws",
            "wss://example.org/ws#fragment",
            "wss://example.org:0/ws",
        ] {
            assert!(parse_address(invalid).is_err(), "{invalid}");
        }
    }
}
