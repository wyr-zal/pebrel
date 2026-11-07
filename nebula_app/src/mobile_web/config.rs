use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProjectGrant {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) shell_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) relay_url: String,
    pub(crate) device_name: String,
    pub(crate) projects: Vec<ProjectGrant>,
}

impl Default for Preferences {
    fn default() -> Self {
        let device_name = std::env::var("COMPUTERNAME")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "Pebrel Computer".to_owned());
        Self { enabled: false, relay_url: String::new(), device_name, projects: Vec::new() }
    }
}

impl Preferences {
    pub(crate) fn project(&self, id: &str) -> Option<&ProjectGrant> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        validate_relay_url(&self.relay_url)?;
        if self.device_name.trim().is_empty()
            || self.device_name.len() > 80
            || self.device_name.chars().any(char::is_control)
        {
            return Err("device name must contain 1..80 visible characters".to_owned());
        }
        if self.projects.len() > 32 {
            return Err("at most 32 projects may be authorized".to_owned());
        }
        for project in &self.projects {
            validate_project(project)?;
        }
        Ok(())
    }

    pub(crate) fn upsert_project(
        &mut self,
        path: &str,
        shell_id: Option<String>,
    ) -> Result<(), String> {
        let grant = ProjectGrant::from_input(path, shell_id)?;
        if let Some(existing) = self.projects.iter_mut().find(|item| item.id == grant.id) {
            *existing = grant;
        } else {
            self.projects.push(grant);
        }
        self.projects.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    pub(crate) fn remove_project(&mut self, id: &str) {
        self.projects.retain(|project| project.id != id);
    }
}

impl ProjectGrant {
    pub(super) fn from_input(path: &str, shell_id: Option<String>) -> Result<Self, String> {
        let requested = PathBuf::from(path.trim());
        if !requested.is_absolute() {
            return Err("project path must be absolute".to_owned());
        }
        let canonical = requested
            .canonicalize()
            .map_err(|error| format!("project path is unavailable: {error}"))?;
        let shell_id = Some(project_shell_id(shell_id.as_deref())?);
        let identity = canonical_identity(&canonical, shell_id.as_deref());
        let digest = Sha256::digest(identity.as_bytes());
        let id = digest[..12].iter().map(|byte| format!("{byte:02x}")).collect();
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Project")
            .to_owned();
        let grant = Self { id, name, path: canonical.to_string_lossy().into_owned(), shell_id };
        validate_project(&grant)?;
        Ok(grant)
    }

    pub(crate) fn runtime_path(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }

    pub(crate) fn contains_cwd(&self, cwd: &str) -> bool {
        let root = normalized_path(Path::new(&self.path));
        if self
            .shell_id
            .as_deref()
            .is_some_and(|shell| shell.to_ascii_lowercase().starts_with("wsl"))
        {
            let host = normalized_path(Path::new(&self.path));
            let guest = crate::ssh::wsl_path(Path::new(&host));
            return path_is_within(&normalized_path(Path::new(cwd)), &guest);
        }
        #[cfg(windows)]
        {
            return path_is_within(
                &normalized_path(Path::new(cwd)).to_lowercase(),
                &root.to_lowercase(),
            );
        }
        #[cfg(not(windows))]
        {
            path_is_within(&normalized_path(Path::new(cwd)), &root)
        }
    }

    pub(crate) fn wsl_distribution(&self) -> Option<&str> {
        let shell = self.shell_id.as_deref()?;
        shell.get(..4).filter(|prefix| prefix.eq_ignore_ascii_case("wsl:"))?;
        shell.get(4..).filter(|distro| !distro.is_empty())
    }
}

fn project_shell_id(requested: Option<&str>) -> Result<String, String> {
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    if let Some(shell) = requested {
        if shell.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("wsl:")) {
            let distro = shell[4..].trim();
            if !distro.is_empty() {
                return Ok(format!("wsl:{distro}"));
            }
        }
    }
    // Freeze a concrete host shell. The user's default may later change to WSL.
    crate::shell_detect::detect_shells()
        .into_iter()
        .find(|shell| {
            !shell.id.to_ascii_lowercase().starts_with("wsl")
                && requested.is_none_or(|id| shell.id.eq_ignore_ascii_case(id))
        })
        .map(|shell| shell.id)
        .ok_or_else(|| String::from("Choose an installed host shell or wsl:<distribution>"))
}

fn path_is_within(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_project(project: &ProjectGrant) -> Result<(), String> {
    if project.id.len() != 24 || !project.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid project id".to_owned());
    }
    if project.name.trim().is_empty()
        || project.name.len() > 120
        || project.name.chars().any(char::is_control)
    {
        return Err("invalid project name".to_owned());
    }
    if project.path.is_empty()
        || project.path.len() > 32 * 1024
        || project.path.chars().any(char::is_control)
        || !Path::new(&project.path).is_absolute()
    {
        return Err("invalid project path".to_owned());
    }
    if project.shell_id.as_ref().is_none_or(|shell| {
        shell.is_empty()
            || shell.len() > 256
            || shell.chars().any(char::is_control)
            || (shell.to_ascii_lowercase().starts_with("wsl")
                && project.wsl_distribution().is_none())
    }) {
        return Err("invalid project shell".to_owned());
    }
    Ok(())
}

pub(crate) fn validate_relay_url(value: &str) -> Result<(), String> {
    normalized_relay_url(value).map(|_| ())
}

pub(crate) fn normalized_relay_url(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > 2048
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.chars().any(char::is_control)
        || trimmed.contains('#')
        || trimmed.contains('?')
        || trimmed.contains('@')
    {
        return Err("invalid Relay URL".to_owned());
    }
    let uri: ureq::http::Uri = trimmed.parse().map_err(|_| String::from("invalid Relay URL"))?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let authority = uri.authority().ok_or_else(|| String::from("Relay host is required"))?;
    let host = authority.host().trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !(scheme == "https" || (scheme == "http" && loopback)) || uri.path() != "/" {
        return Err(String::from("Relay must be an HTTPS origin (HTTP only on loopback)"));
    }
    Ok(format!("{scheme}://{}", authority.as_str().to_ascii_lowercase()))
}

fn canonical_identity(path: &Path, shell_id: Option<&str>) -> String {
    let mut path = normalized_path(path);
    #[cfg(windows)]
    {
        path = path.to_lowercase();
    }
    format!("{}|{path}", shell_id.unwrap_or_default())
}

fn normalized_path(path: &Path) -> String {
    let value = normalize_slashes(&path.to_string_lossy());
    let value = if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        value.clone()
    };
    value.trim_end_matches('/').to_owned()
}

fn normalize_slashes(value: &str) -> String {
    value.replace('\\', "/")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeviceCredentials {
    pub(crate) device_id: String,
    pub(crate) device_secret: String,
    pub(crate) device_token: String,
    pub(crate) e2ee_master_key: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoredConfig {
    pub(crate) version: u32,
    pub(crate) preferences: Preferences,
    pub(crate) device: Option<DeviceCredentials>,
}

impl StoredConfig {
    pub(crate) fn load() -> Result<Self, String> {
        let path = config_path();
        if !path.try_exists().map_err(|error| error.to_string())? {
            return Ok(Self { version: 1, ..Self::default() });
        }
        let key = load_secret(CONFIG_KEY)?
            .ok_or_else(|| String::from("remote configuration key unavailable"))?;
        let bytes = super::storage::read(&path, &key, scoped_key(CONFIG_KEY).as_bytes())?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "invalid web remote config".to_owned())?;
        if config.version != 1 {
            return Err("unsupported web remote config version".to_owned());
        }
        config.preferences.validate()?;
        Ok(config)
    }

    pub(crate) fn store(&self) -> Result<(), String> {
        self.preferences.validate()?;
        if self.version != 1 {
            return Err("unsupported web remote config version".to_owned());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        let key = match load_secret(CONFIG_KEY)? {
            Some(key) => key,
            None => {
                let mut key = vec![0_u8; 32];
                getrandom::fill(&mut key)
                    .map_err(|_| String::from("configuration randomness unavailable"))?;
                store_secret(CONFIG_KEY, &key)?;
                key
            },
        };
        super::storage::write(&config_path(), &key, scoped_key(CONFIG_KEY).as_bytes(), &bytes)
    }
}

const CONFIG_KEY: &str = "web-configuration-key-v1";

fn config_path() -> PathBuf {
    state_dir().join("config-v1.enc")
}

pub(super) fn state_dir() -> PathBuf {
    let scope =
        if crate::platform::elevation::requires_isolation() { "elevated" } else { "standard" };
    crate::display::nebula_data_dir().join("mobile-web").join(scope)
}

fn scoped_key(name: &str) -> String {
    let path = crate::display::nebula_data_dir();
    let identity =
        format!("{}:{}", path.display(), crate::platform::elevation::requires_isolation());
    let digest = Sha256::digest(identity.as_bytes());
    let scope: String = digest[..12].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("Pebrel/Mobile/Web/V1/{scope}/{name}")
}

fn load_secret(name: &str) -> Result<Option<Vec<u8>>, String> {
    crate::platform::credentials::load(&scoped_key(name)).map_err(|error| error.to_string())
}

fn store_secret(name: &str, bytes: &[u8]) -> Result<(), String> {
    crate::platform::credentials::store(&scoped_key(name), bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_scope_requires_a_path_boundary() {
        let root = std::env::temp_dir();
        let root_text =
            root.to_string_lossy().trim_end_matches(|ch| ch == '\\' || ch == '/').to_owned();
        let grant = ProjectGrant::from_input(&root_text, None).unwrap();
        assert!(grant.contains_cwd(&root_text));
        assert!(!grant.contains_cwd(&format!("{root_text}x")));
    }

    #[test]
    fn project_scope_does_not_mix_native_and_wsl_paths() {
        let root = std::env::temp_dir();
        let host = root.to_string_lossy();
        let guest = crate::ssh::wsl_path(&root);
        let native = ProjectGrant::from_input(&host, None).unwrap();
        assert!(native.contains_cwd(&host));
        assert!(!native.contains_cwd(&guest));

        let wsl = ProjectGrant::from_input(&host, Some(String::from("wsl:Ubuntu"))).unwrap();
        assert!(wsl.contains_cwd(&guest));
        assert!(!wsl.contains_cwd(&host));
    }

    #[test]
    fn relay_rejects_plaintext_non_loopback_origins() {
        assert!(validate_relay_url("https://relay.example.com").is_ok());
        assert!(validate_relay_url("http://127.0.0.1:8787").is_ok());
        assert!(validate_relay_url("http://relay.example.com").is_err());
        assert!(validate_relay_url("http://localhost:80@relay.example.com").is_err());
        assert!(validate_relay_url("https://user@relay.example.com").is_err());
        assert!(validate_relay_url("https://relay.example.com/path").is_err());
        assert!(validate_relay_url("http://[::1]:8787/").is_ok());
        assert_eq!(
            normalized_relay_url("https://Relay.Example.com/").unwrap(),
            "https://relay.example.com"
        );
    }
}
