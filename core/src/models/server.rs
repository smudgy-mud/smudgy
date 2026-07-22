// Models related to server configurations

use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, io};
use validator::Validate;

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
    /// <host>" opt-in; compared case-insensitively).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_link_hosts: Vec<String>,
    /// The "always trust links from this server" opt-in: every server-sent
    /// link — any URL host, and `send:` command links — activates without
    /// the confirm dialog.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_all_links: bool,
    /// The character encoding this server speaks, as an Encoding Standard label
    /// (`"big5"`, `"iso-8859-1"`, …). `None` = UTF-8. CHARSET negotiation (RFC
    /// 2066), when the server offers it, overrides this for the life of the
    /// connection. An unresolvable label is logged and treated as UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// Whether inbound compression offers (MCCP2) are accepted. On by default;
    /// off declines every compression option with `DONT`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub compression: bool,
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
            tls: false,
            tls_verify: true,
        }
    }

    /// Whether a server-sent link needs no confirm dialog: `host` is `None`
    /// for a `send:` command link (covered only by the blanket grant) and
    /// the URL's host for a browser link.
    #[must_use]
    pub fn allows_server_link(&self, host: Option<&str>) -> bool {
        if self.trust_all_links {
            return true;
        }
        host.is_some_and(|host| {
            self.trusted_link_hosts
                .iter()
                .any(|trusted| trusted.eq_ignore_ascii_case(host))
        })
    }
}

/// The host component of an http(s) URL, lowercased — the unit the per-host
/// link grant is keyed by. `None` when the URL has no recognizable host.
#[must_use]
pub fn link_url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Strip userinfo, then the port — mind IPv6 bracket forms.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']').map_or(host, |(h, _)| &h[1..])
    } else {
        host.rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(host, |(h, _)| h)
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
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
                match entry_result {
                    Ok(entry) => {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                let config_path = path.join("server.json");
                                match load_server_config(&config_path) {
                                    Ok(config) => {
                                        servers.push(Server {
                                            name: name.to_string(),
                                            path: path.clone(),
                                            config,
                                        });
                                    }
                                    Err(e) => {
                                        // Log warning: Failed to load server config
                                        eprintln!(
                                            "Warning: Skipping server '{name}'. Failed to load config: {e}"
                                        );
                                    }
                                }
                            } else {
                                // Log warning: Invalid directory name (not UTF-8)
                                eprintln!(
                                    "Warning: Skipping directory with non-UTF8 name: {}",
                                    path.display()
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // Log warning: Failed to read directory entry
                        eprintln!("Warning: Failed to read directory entry: {e}");
                    }
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // If the smudgy dir itself doesn't exist yet (first run?), return empty list.
            // get_smudgy_home() already created it, but read_dir might race?
            // Or maybe permissions issue. Log warning.
            eprintln!("Warning: Smudgy home directory not found or accessible during scan: {e}");
            // Returning empty list is fine here.
        }
        Err(e) => {
            // Other errors reading the main smudgy dir are propagated.
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

/// Ensures the standard subdirectories exist within a given server directory.
///
/// Creates `profiles`, `aliases`, `hotkeys`, `triggers`, `modules`, and `maps`
/// directories if they don't already exist.
///
/// # Arguments
///
/// * `server_path` - The path to the server's root directory.
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
/// # Arguments
///
/// * `name` - The name for the new server. Must be a valid directory name.
/// * `config` - The initial `ServerConfig` for the server.
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
    // Validate server name (basic validation for now)
    if name.is_empty() || name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        return Err(anyhow::anyhow!(
            "Invalid server name: '{}'. Use only alphanumeric, underscore, or hyphen.",
            name
        ));
    }

    // Validate the provided configuration
    config
        .validate()
        .context(format!("Invalid configuration for server '{name}'"))?;

    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(name);

    // Check if server directory already exists
    if server_path.exists() {
        return Err(anyhow::anyhow!(
            "Server '{}' already exists at {:?}",
            name,
            server_path
        ));
    }

    // Create the main server directory
    fs::create_dir(&server_path).context(format!(
        "Failed to create main directory for server '{name}' at {}",
        server_path.display()
    ))?;

    // Ensure standard subdirectories are created
    ensure_server_subdirs(&server_path)?;

    // Write the server.json file
    let config_path = server_path.join("server.json");
    let config_json = serde_json::to_string_pretty(&config)
        .context(format!("Failed to serialize config for server '{name}'"))?;

    fs::write(&config_path, config_json).context(format!(
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
/// # Arguments
///
/// * `name` - The name of the server to load.
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
    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(name);

    if !server_path.exists() {
        return Err(anyhow::anyhow!("Server '{}' not found", name))
            .with_context(|| format!("Looked in directory: {}", server_path.display()));
    }

    if !server_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Path for server '{}' exists but is not a directory: {:?}",
            name,
            server_path
        ));
    }

    // Ensure standard subdirectories exist
    ensure_server_subdirs(&server_path).context(format!(
        "Failed to ensure subdirectories for server '{name}'"
    ))?;

    // Load the configuration
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
/// Finds the server by name, validates the new configuration, and overwrites
/// the existing `server.json` file.
///
/// # Arguments
///
/// * `name` - The name of the server to update.
/// * `new_config` - The `ServerConfig` containing the updated settings.
///
/// # Errors
///
/// Returns an error if:
/// * The server with the given `name` cannot be found.
/// * The path found is not a directory.
/// * The `new_config` fails validation.
/// * The `server.json` file cannot be written.
pub fn update_server(name: &str, new_config: ServerConfig) -> Result<Server> {
    // Validate the new configuration first
    new_config.validate().context(format!(
        "Invalid new configuration provided for server '{name}'"
    ))?;

    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(name);

    // Ensure the server directory exists and is a directory
    if !server_path.exists() {
        return Err(anyhow::anyhow!("Server '{}' not found for update", name))
            .with_context(|| format!("Looked for directory: {}", server_path.display()));
    }
    if !server_path.is_dir() {
        return Err(anyhow::anyhow!(
            "Path for server '{}' exists but is not a directory: {:?}",
            name,
            server_path
        ));
    }

    // Construct path to server.json
    let config_path = server_path.join("server.json");

    // Serialize the new config
    let config_json = serde_json::to_string_pretty(&new_config).context(format!(
        "Failed to serialize updated config for server '{name}'"
    ))?;

    // Write the new config, overwriting the old one
    fs::write(&config_path, config_json).context(format!(
        "Failed to write updated server.json for server '{name}' at {}",
        config_path.display()
    ))?;

    // Return the server representation with the new config
    Ok(Server {
        name: name.to_string(),
        path: server_path,
        config: new_config, // Use the validated new_config
    })
}

/// Deletes a server and all its associated data.
///
/// Finds the server directory by name and removes it recursively.
/// If the server directory does not exist, the function succeeds silently.
///
/// # Arguments
///
/// * `name` - The name of the server to delete.
///
/// # Errors
///
/// Returns an error if:
/// * The smudgy home directory cannot be accessed.
/// * A file exists with the server name (instead of a directory).
/// * The directory or its contents cannot be removed due to permissions or other I/O issues.
pub fn delete_server(name: &str) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let server_path = smudgy_dir.join(name);

    if server_path.exists() {
        // Check if it's actually a directory before attempting recursive delete
        if !server_path.is_dir() {
            return Err(anyhow::anyhow!(
                "Cannot delete server '{}': Path exists but is not a directory: {:?}",
                name,
                server_path
            ));
        }

        // Recursively remove the directory
        fs::remove_dir_all(&server_path).context(format!(
            "Failed to delete directory for server '{name}' at {}",
            server_path.display()
        ))?;
    } else {
        // Optionally log that the server didn't exist? For now, silent success.
        println!("Info: Server '{name}' not found for deletion.");
    }

    Ok(())
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
        assert_eq!(link_url_host("nonsense"), None);
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
}
