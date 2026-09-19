// Models related to server configurations

use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::{fs, io};
use url::{Host, Url};
use validator::Validate;

use super::persistence::write_atomic;

/// Result of a server mutation guarded by an exact loaded snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCas<T> {
    Applied(T),
    StateChanged,
}

fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        anyhow::bail!(
            "Invalid server name: '{}'. Use only alphanumeric, underscore, or hyphen.",
            name
        );
    }
    Ok(())
}

/// Represents the configuration for a single server connection.
/// This struct is serialized to/from `server.json` within the server's directory.
#[derive(Serialize, Deserialize, Debug, Validate, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The hostname or IP address of the server.
    #[validate(length(min = 1, message = "Host cannot be empty"))]
    pub host: String,
    /// The port number of the server.
    #[validate(range(min = 1, max = 65535, message = "Port must be between 1 and 65535"))]
    pub port: u16,
    /// Hosts the user has granted this MUD's OSC 8 hyperlinks permission to
    /// open in the browser without asking again (the "always allow links to
    /// <host>" opt-in. Legacy Unicode-domain and expanded-IPv6 entries are
    /// canonicalized when compared and on the next grant write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_link_hosts: Vec<String>,
    /// The "always trust links from this server" opt-in: every server-sent
    /// link — any URL host, and `send:` command links — activates without
    /// the confirm dialog.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_all_links: bool,
    /// The character encoding this server speaks, as an Encoding Standard label
    /// (`"big5"`, `"iso-8859-1"`, …). `None` = UTF-8. An unresolvable label is
    /// logged and treated as UTF-8.
    ///
    /// This is a preference, not a floor: it leads what CHARSET negotiation (RFC
    /// 2066) offers and wins over UTF-8 when a server offers both, but a server
    /// that cannot speak it still settles the connection somewhere else for the
    /// life of that connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// Whether inbound MCCP2 compression offers are accepted. On by default.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub compression: bool,
    /// Whether inbound MCCP4 compression offers are accepted. `None` is the legacy on-disk
    /// representation: inherit [`compression`](Self::compression), which preserves the old
    /// checkbox's behavior for configurations written before the two protocols were split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mccp4_compression: Option<bool>,
    /// Connect over TLS. Off by default (don't silently change existing plain servers).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tls: bool,
    /// When `tls`, whether to verify the server certificate against the OS trust store.
    /// On by default; off accepts any certificate (self-signed MUD ports — insecure).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub tls_verify: bool,
}

const fn default_true() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // the signature serde's skip_serializing_if wants
fn is_true(value: &bool) -> bool {
    *value
}

impl ServerConfig {
    /// A fresh config with no link-trust grants, speaking UTF-8, accepting compression.
    #[must_use]
    pub const fn new(host: String, port: u16) -> Self {
        Self {
            host,
            port,
            trusted_link_hosts: Vec::new(),
            trust_all_links: false,
            encoding: None,
            compression: true,
            mccp4_compression: None,
            tls: false,
            tls_verify: true,
        }
    }

    /// Whether MCCP4 is enabled after applying the legacy shared-compression fallback.
    #[must_use]
    pub fn accepts_mccp4_compression(&self) -> bool {
        self.mccp4_compression.unwrap_or(self.compression)
    }

    /// Whether a server-sent link needs no confirm dialog: `host` is `None`
    /// for a `send:` command link (covered only by the blanket grant) and
    /// the URL's host for a browser link.
    #[must_use]
    pub fn allows_server_link(&self, host: Option<&str>) -> bool {
        if self.trust_all_links {
            return true;
        }
        host.and_then(canonical_link_host).is_some_and(|host| {
            self.trusted_link_hosts
                .iter()
                .filter_map(|trusted| canonical_link_host(trusted))
                .any(|trusted| trusted.eq_ignore_ascii_case(&host))
        })
    }

    /// Persist a per-host grant in the current canonical form, cleaning up
    /// duplicate legacy spellings while preserving their authorization.
    pub fn grant_link_host(&mut self, host: &str) {
        let Some(host) = canonical_link_host(host) else {
            return;
        };
        let mut canonical = Vec::with_capacity(self.trusted_link_hosts.len() + 1);
        for trusted in self
            .trusted_link_hosts
            .drain(..)
            .filter_map(|trusted| canonical_link_host(&trusted))
            .chain(std::iter::once(host))
        {
            if !canonical
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&trusted))
            {
                canonical.push(trusted);
            }
        }
        self.trusted_link_hosts = canonical;
    }
}

fn canonical_link_host(host: &str) -> Option<String> {
    if let Ok(address) = host.parse::<Ipv6Addr>() {
        return Some(address.to_string());
    }
    match Host::parse(host).ok()? {
        Host::Domain(host) => Some(host),
        Host::Ipv4(host) => Some(host.to_string()),
        Host::Ipv6(host) => Some(host.to_string()),
    }
}

/// The canonical host component of a browser URL — the unit the per-host link
/// grant is keyed by. `None` when the scheme is unsupported, the written
/// authority is empty, or the URL has no valid host.
#[must_use]
pub fn link_url_host(url: &str) -> Option<String> {
    // WHATWG parsing removes ASCII tab/newline characters before parsing. Do
    // the same before checking the written authority so `https://\t/path`
    // cannot recover into a host and bypass the stricter empty-authority rule.
    let normalized = if url.contains(['\t', '\n', '\r']) {
        Cow::Owned(
            url.chars()
                .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
                .collect::<String>(),
        )
    } else {
        Cow::Borrowed(url)
    };
    // `url` correctly follows browser recovery rules, under which
    // `https:///path` becomes `https://path/`. Server-authored OSC links keep
    // the stricter existing contract: an authority must actually be written.
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

/// Represents a server, including its configuration and associated directory path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// The unique name of the server, derived from its directory name.
    pub name: String,
    /// The path to the server's directory within the smudgy home.
    pub path: PathBuf,
    /// The server's configuration details loaded from `server.json`.
    pub config: ServerConfig,
}

/// Whether `smudgy_dir` contains at least one directory that `list_servers`
/// would recognize as a server.
pub(crate) fn contains_valid_server(smudgy_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(smudgy_dir) else {
        return false;
    };

    entries.filter_map(std::result::Result::ok).any(|entry| {
        let path = entry.path();
        entry.file_type().is_ok_and(|kind| kind.is_dir())
            && path.file_name().and_then(|name| name.to_str()).is_some()
            && load_server_config(&path.join("server.json")).is_ok()
    })
}

/// Lists all valid servers found within the smudgy home directory.
///
/// A server is considered valid if it's a directory within the smudgy home
/// and contains a readable and valid `server.json` file.
///
/// # Errors
///
/// Returns an error if the smudgy home directory cannot be accessed or read.
/// Errors reading individual server directories or parsing `server.json` files
/// are logged as warnings, and those servers are skipped.
pub fn list_servers() -> Result<Vec<Server>> {
    let smudgy_dir = get_smudgy_home()?;
    let mut servers = Vec::new();

    match fs::read_dir(&smudgy_dir) {
        Ok(entries) => {
            for entry_result in entries {
                let entry = match entry_result {
                    Ok(entry) => entry,
                    Err(error) => {
                        eprintln!("Warning: Failed to read directory entry: {error}");
                        continue;
                    }
                };
                let path = entry.path();
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    eprintln!(
                        "Warning: Skipping directory with non-UTF8 name: {}",
                        path.display()
                    );
                    continue;
                };
                if validate_server_name(name).is_err() {
                    continue;
                }
                let config = match load_server_config(&path.join("server.json")) {
                    Ok(config) => config,
                    Err(error) => {
                        eprintln!(
                            "Warning: Skipping server '{name}'. Failed to load config: {error}"
                        );
                        continue;
                    }
                };
                servers.push(Server {
                    name: name.to_string(),
                    path,
                    config,
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("Warning: Smudgy home directory not found or accessible during scan: {e}");
        }
        Err(e) => {
            return Err(e).context(format!(
                "Failed to read smudgy directory entries at {}",
                smudgy_dir.to_string_lossy()
            ));
        }
    }

    Ok(servers)
}

/// Helper function to load and deserialize `ServerConfig` from a file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, read, or if the contents
/// cannot be deserialized into a `ServerConfig`.
fn load_server_config(path: &PathBuf) -> Result<ServerConfig> {
    let file_content = fs::read_to_string(path).context(format!(
        "Failed to read server config file: {}",
        path.display()
    ))?;
    let config: ServerConfig = serde_json::from_str(&file_content).context(format!(
        "Failed to parse server config file: {}",
        path.display()
    ))?;
    config.validate().context(format!(
        "Server config validation failed: {}",
        path.display()
    ))?;
    Ok(config)
}

fn load_server_for_cas_locked(name: &str) -> Result<Option<Server>> {
    let path = get_smudgy_home()?.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => load_server(name).map(Some),
        Ok(_) => anyhow::bail!("server path is not a real directory: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect server path {}", path.display())),
    }
}

/// Checks an exact server snapshot while the caller already holds its lifecycle lock.
pub(crate) fn server_unchanged_locked(expected: &Server) -> Result<bool> {
    Ok(load_server_for_cas_locked(&expected.name)?.as_ref() == Some(expected))
}

/// Runs a short operation only while an exact loaded server snapshot is still current.
///
/// The server lifecycle lock remains held for the operation, so this is intended for small,
/// name-keyed reads or side effects such as a UI cache namespace. Network requests and other
/// long-running work must finish before calling this helper.
///
/// # Errors
/// Returns an error for an invalid name, a server-state read failure, or a failed operation.
pub fn with_server_if_unchanged<T>(
    expected: &Server,
    operation: impl FnOnce(&Server) -> Result<T>,
) -> Result<ServerCas<T>> {
    validate_server_name(&expected.name)?;
    let _lifecycle_guard = super::profile::lifecycle_guard(&expected.name)?;
    let Some(current) = load_server_for_cas_locked(&expected.name)? else {
        return Ok(ServerCas::StateChanged);
    };
    if current != *expected {
        return Ok(ServerCas::StateChanged);
    }
    operation(&current).map(ServerCas::Applied)
}

/// Ensures the standard subdirectories exist within a given server directory.
///
/// Creates `profiles`, `aliases`, `hotkeys`, `triggers`, `modules`, and `maps`
/// directories if they don't already exist.
///
/// # Errors
///
/// Returns an error if any of the directories cannot be created.
pub fn ensure_server_subdirs(server_path: &Path) -> Result<()> {
    let subdirs = [
        "profiles", "aliases", "hotkeys", "triggers", "modules", "maps", "logs",
    ];

    for subdir in &subdirs {
        let dir_path = server_path.join(subdir);
        fs::create_dir_all(&dir_path).context(format!(
            "Failed to create subdirectory '{subdir}' in {}",
            server_path.display()
        ))?;
    }

    Ok(())
}

/// Creates a new server directory structure and configuration file.
///
/// # Errors
///
/// Returns an error if:
/// * The server name is invalid.
/// * The provided `config` is invalid.
/// * The smudgy home directory cannot be accessed.
/// * A server with the same name already exists.
/// * There are filesystem errors during directory or file creation.
pub fn create_server(name: &str, config: ServerConfig) -> Result<Server> {
    validate_server_name(name)?;
    let _profile_lifecycle_guard = super::profile::lifecycle_guard(name)?;

    config
        .validate()
        .context(format!("Invalid configuration for server '{name}'"))?;

    let server_path = get_smudgy_home()?.join(name);
    if server_path.exists() {
        return Err(anyhow::anyhow!(
            "Server '{}' already exists at {:?}",
            name,
            server_path
        ));
    }

    fs::create_dir(&server_path).context(format!(
        "Failed to create main directory for server '{name}' at {}",
        server_path.display()
    ))?;
    ensure_server_subdirs(&server_path)?;

    let config_path = server_path.join("server.json");
    let config_json = serde_json::to_string_pretty(&config)
        .context(format!("Failed to serialize config for server '{name}'"))?;
    write_atomic(&config_path, config_json.as_bytes()).context(format!(
        "Failed to write server.json for server '{name}' at {}",
        config_path.display()
    ))?;

    Ok(Server {
        name: name.to_string(),
        path: server_path,
        config,
    })
}

/// Loads a specific server by its name.
///
/// This function finds the server directory, ensures the standard subdirectories
/// exist (creating them if necessary), loads the `server.json` configuration,
/// and returns the `Server` struct.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home directory cannot be accessed.
/// * No directory with the given `name` exists within the smudgy home.
/// * The found path is not a directory.
/// * The `server.json` file is missing, cannot be read, or is invalid.
/// * Any required subdirectories cannot be created.
pub fn load_server(name: &str) -> Result<Server> {
    validate_server_name(name)?;
    let _profile_lifecycle_guard = super::profile::lifecycle_guard(name)?;
    let server_path = get_smudgy_home()?.join(name);

    if !server_path.exists() {
        return Err(anyhow::anyhow!("Server '{}' not found", name))
            .with_context(|| format!("Looked in directory: {}", server_path.display()));
    }
    if !fs::symlink_metadata(&server_path)?.file_type().is_dir() {
        return Err(anyhow::anyhow!(
            "Path for server '{}' exists but is not a directory: {:?}",
            name,
            server_path
        ));
    }

    ensure_server_subdirs(&server_path).context(format!(
        "Failed to ensure subdirectories for server '{name}'"
    ))?;

    let config_path = server_path.join("server.json");
    let config = load_server_config(&config_path)
        .context(format!("Failed to load config for server '{name}'"))?;

    Ok(Server {
        name: name.to_string(),
        path: server_path,
        config,
    })
}

/// Updates the configuration of an existing server.
///
/// # Errors
///
/// Returns an error if:
/// * The server with the given `name` cannot be found.
/// * The path found is not a directory.
/// * The `new_config` fails validation.
/// * The `server.json` file cannot be written.
pub fn update_server(name: &str, new_config: ServerConfig) -> Result<Server> {
    validate_server_name(name)?;
    new_config.validate().context(format!(
        "Invalid new configuration provided for server '{name}'"
    ))?;
    let _profile_lifecycle_guard = super::profile::lifecycle_guard(name)?;

    let server_path = get_smudgy_home()?.join(name);
    if !server_path.exists() {
        return Err(anyhow::anyhow!("Server '{}' not found for update", name))
            .with_context(|| format!("Looked for directory: {}", server_path.display()));
    }
    if !fs::symlink_metadata(&server_path)?.file_type().is_dir() {
        return Err(anyhow::anyhow!(
            "Path for server '{}' exists but is not a directory: {:?}",
            name,
            server_path
        ));
    }

    let config_path = server_path.join("server.json");
    let config_json = serde_json::to_string_pretty(&new_config).context(format!(
        "Failed to serialize updated config for server '{name}'"
    ))?;
    write_atomic(&config_path, config_json.as_bytes()).context(format!(
        "Failed to write updated server.json for server '{name}' at {}",
        config_path.display()
    ))?;

    Ok(Server {
        name: name.to_string(),
        path: server_path,
        config: new_config,
    })
}

/// Updates a server only while its complete loaded snapshot is still current.
///
/// A missing or edited server returns [`ServerCas::StateChanged`] and is not modified.
///
/// # Errors
/// Returns an error for an invalid name or a server-state I/O failure.
pub fn update_server_if_unchanged(
    expected: &Server,
    new_config: ServerConfig,
) -> Result<ServerCas<Server>> {
    validate_server_name(&expected.name)?;
    let _lifecycle_guard = super::profile::lifecycle_guard(&expected.name)?;
    let Some(current) = load_server_for_cas_locked(&expected.name)? else {
        return Ok(ServerCas::StateChanged);
    };
    if current != *expected {
        return Ok(ServerCas::StateChanged);
    }
    update_server(&expected.name, new_config).map(ServerCas::Applied)
}

/// Deletes a server and all its associated data.
///
/// Credentials that live outside the server directory (profile passwords and package secrets)
/// are removed first, then the directory is removed recursively. If the server directory does
/// not exist, the function succeeds silently.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home directory cannot be accessed.
/// * A file exists with the server name (instead of a directory).
/// * Credentials or the directory cannot be removed.
pub fn delete_server(name: &str) -> Result<()> {
    validate_server_name(name)?;
    let _profile_lifecycle_guard = super::profile::lifecycle_guard(name)?;
    if let Some(current) = load_server_for_cas_locked(name)? {
        remove_server_locked(&current)?;
    }
    Ok(())
}

fn remove_server_locked(current: &Server) -> Result<()> {
    let _package_guard = super::shared_packages::guard(&current.name);
    // Deterministic credentials live outside the server directory. They must be gone before the
    // directory is removed; otherwise removing the local index would make an orphaned keyring
    // entry impossible to identify safely.
    super::profile::prepare_server_deletion(&current.name)
        .context("Failed to clear profile credentials before deleting the server")?;
    super::shared_packages::clear_server_param_secrets(&current.name)
        .context("Failed to clear package credentials before deleting the server")?;
    fs::remove_dir_all(&current.path).context(format!(
        "Failed to delete directory for server '{}' at {}",
        current.name,
        current.path.display()
    ))
}

/// Deletes only the exact loaded server snapshot in `expected`.
///
/// # Errors
/// Returns an error for an invalid name or a deletion failure.
pub fn delete_server_if_unchanged(expected: &Server) -> Result<ServerCas<()>> {
    delete_server_if_unchanged_and_then(expected, || {})
}

/// Deletes only the exact loaded server snapshot in `expected`, then runs `after_delete` before
/// releasing the server lifecycle lock.
///
/// This is intended for infallible, name-keyed cleanup owned by an embedding layer, such as a UI
/// cache namespace. The callback runs only after deletion and must be short.
///
/// # Errors
/// Returns an error for an invalid name or a deletion failure.
pub fn delete_server_if_unchanged_and_then(
    expected: &Server,
    after_delete: impl FnOnce(),
) -> Result<ServerCas<()>> {
    validate_server_name(&expected.name)?;
    let _lifecycle_guard = super::profile::lifecycle_guard(&expected.name)?;
    match load_server_for_cas_locked(&expected.name)? {
        Some(current) if current != *expected => return Ok(ServerCas::StateChanged),
        Some(current) => remove_server_locked(&current)?,
        None => return Ok(ServerCas::StateChanged),
    }
    after_delete();
    Ok(ServerCas::Applied(()))
}

#[cfg(test)]
mod link_trust_tests {
    use super::{ServerConfig, link_url_host};

    fn config(hosts: &[&str], all: bool) -> ServerConfig {
        ServerConfig {
            trusted_link_hosts: hosts.iter().map(ToString::to_string).collect(),
            trust_all_links: all,
            ..ServerConfig::new("mud.example.org".to_string(), 4000)
        }
    }

    #[test]
    fn blanket_trust_covers_urls_and_commands() {
        let c = config(&[], true);
        assert!(c.allows_server_link(Some("anything.example")));
        assert!(
            c.allows_server_link(None),
            "send: links ride the blanket grant"
        );
    }

    #[test]
    fn host_grants_match_case_insensitively_and_only_urls() {
        let c = config(&["Wiki.Example.ORG"], false);
        assert!(c.allows_server_link(Some("wiki.example.org")));
        assert!(!c.allows_server_link(Some("evil.example.org")));
        assert!(
            !c.allows_server_link(None),
            "a host grant never covers send: links"
        );
    }

    #[test]
    fn legacy_host_grants_survive_canonicalization_changes() {
        let c = config(&["\u{4f8b}\u{3048}.test", "0:0:0:0:0:0:0:1"], false);
        assert!(c.allows_server_link(Some("xn--r8jz45g.test")));
        assert!(c.allows_server_link(Some("::1")));
    }

    #[test]
    fn granting_a_host_migrates_and_deduplicates_legacy_entries() {
        let mut c = config(
            &[
                "\u{4f8b}\u{3048}.test",
                "xn--r8jz45g.test",
                "0:0:0:0:0:0:0:1",
            ],
            false,
        );
        c.grant_link_host("[::1]");
        assert_eq!(
            c.trusted_link_hosts,
            ["xn--r8jz45g.test".to_string(), "::1".to_string()]
        );
    }

    #[test]
    fn ungranted_config_allows_nothing() {
        let c = config(&[], false);
        assert!(!c.allows_server_link(Some("wiki.example.org")));
        assert!(!c.allows_server_link(None));
    }

    #[test]
    fn url_host_extraction() {
        assert_eq!(
            link_url_host("https://Wiki.Example.org/page?x=1"),
            Some("wiki.example.org".to_string())
        );
        assert_eq!(
            link_url_host("http://example.org:8080/p"),
            Some("example.org".to_string())
        );
        assert_eq!(
            link_url_host("https://user:pw@example.org/x"),
            Some("example.org".to_string())
        );
        assert_eq!(
            link_url_host("https://[::1]:8080/x"),
            Some("::1".to_string())
        );
        assert_eq!(link_url_host("https:///nohost"), None);
        assert_eq!(link_url_host("https://\t/path"), None);
        assert_eq!(link_url_host("https://\r\n/path"), None);
        assert_eq!(link_url_host("nonsense"), None);
        assert_eq!(link_url_host("ssh://example.org/path"), None);
    }

    #[test]
    fn url_host_extraction_matches_browser_backslash_and_idna_semantics() {
        assert_eq!(
            link_url_host(r"https://evil.example\@trusted.example/"),
            Some("evil.example".to_string())
        );
        assert_eq!(
            link_url_host("https://例え.test/path"),
            Some("xn--r8jz45g.test".to_string())
        );
    }

    #[test]
    fn trust_fields_round_trip_and_default_clean() {
        let c = config(&["a.example"], false);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<ServerConfig>(&json).unwrap(), c);
        // Old-format files (no trust fields) still deserialize.
        let old: ServerConfig = serde_json::from_str(r#"{"host":"h","port":1}"#).unwrap();
        assert!(old.trusted_link_hosts.is_empty());
        assert!(!old.trust_all_links);
    }

    #[test]
    fn legacy_compression_setting_also_controls_mccp4() {
        let enabled: ServerConfig = serde_json::from_str(r#"{"host":"h","port":1}"#).unwrap();
        assert!(enabled.compression);
        assert!(enabled.accepts_mccp4_compression());

        let disabled: ServerConfig =
            serde_json::from_str(r#"{"host":"h","port":1,"compression":false}"#).unwrap();
        assert!(!disabled.compression);
        assert!(!disabled.accepts_mccp4_compression());
    }

    #[test]
    fn explicit_mccp4_setting_is_independent_and_round_trips() {
        let mut config = ServerConfig::new("h".to_string(), 1);
        config.compression = false;
        config.mccp4_compression = Some(true);

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""mccp4_compression":true"#));
        let decoded: ServerConfig = serde_json::from_str(&json).unwrap();
        assert!(!decoded.compression);
        assert!(decoded.accepts_mccp4_compression());
    }
}
