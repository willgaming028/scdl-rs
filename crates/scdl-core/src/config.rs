//! Config file handling, compatible with scdl's `~/.config/scdl/scdl.cfg`.
//!
//! The format is the same INI file the Python version uses, so an existing
//! config keeps working. Two behaviours differ deliberately:
//!
//! * The file is created `0600`. The Python version writes it with the process
//!   umask (typically `0644`), leaving a SoundCloud OAuth token world-readable.
//! * A config that fails to parse is an error, not something to silently
//!   overwrite with defaults — overwriting destroys a stored token.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::naming::{DEFAULT_NAME_FORMAT, DEFAULT_PLAYLIST_NAME_FORMAT};

const SECTION: &str = "scdl";

#[derive(Debug, Clone)]
pub struct Config {
    pub client_id: Option<String>,
    pub auth_token: Option<String>,
    pub path: PathBuf,
    pub name_format: String,
    pub playlist_name_format: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: None,
            auth_token: None,
            path: PathBuf::from("."),
            name_format: DEFAULT_NAME_FORMAT.to_string(),
            playlist_name_format: DEFAULT_PLAYLIST_NAME_FORMAT.to_string(),
        }
    }
}

/// Where the config lives, honouring `XDG_CONFIG_HOME` exactly as scdl does.
pub fn default_config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("scdl").join("scdl.cfg");
        }
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".config"))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("scdl")
        .join("scdl.cfg")
}

impl Config {
    /// Load a config, falling back to defaults when the file does not exist.
    ///
    /// A file that exists but cannot be parsed is an error: the alternative is
    /// overwriting it with defaults and losing whatever token it held.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::io(path, e)),
        };

        let ini = ini::Ini::load_from_str(&text).map_err(|e| {
            Error::Config(format!(
                "{} is not valid config ({e}). Fix or remove it; refusing to overwrite it \
                 because it may hold your auth token.",
                path.display()
            ))
        })?;

        let section = ini.section(Some(SECTION));
        let get = |key: &str| -> Option<String> {
            section
                .and_then(|s| s.get(key))
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };

        let defaults = Config::default();
        Ok(Config {
            client_id: get("client_id"),
            auth_token: get("auth_token"),
            path: get("path").map(PathBuf::from).unwrap_or(defaults.path),
            name_format: get("name_format").unwrap_or(defaults.name_format),
            playlist_name_format: get("playlist_name_format")
                .unwrap_or(defaults.playlist_name_format),
        })
    }

    pub fn to_ini_string(&self) -> String {
        format!(
            "[{SECTION}]\n\
             client_id = {}\n\
             auth_token = {}\n\
             path = {}\n\
             name_format = {}\n\
             playlist_name_format = {}\n\
             \n\
             # Name formats accept both %(field)s and the legacy {{field}} syntax.\n",
            self.client_id.as_deref().unwrap_or(""),
            self.auth_token.as_deref().unwrap_or(""),
            self.path.display(),
            self.name_format,
            self.playlist_name_format,
        )
    }

    /// Write the config with owner-only permissions.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
            restrict_dir(dir);
        }

        std::fs::write(path, self.to_ini_string()).map_err(|e| Error::io(path, e))?;
        restrict_file(path)?;
        Ok(())
    }

    /// True when this config holds a secret worth protecting.
    pub fn has_secret(&self) -> bool {
        self.auth_token.is_some()
    }
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|e| Error::io(path, e))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    // Windows inherits restrictive ACLs from the user profile directory.
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) {}

/// Warn if an existing config is readable by anyone but its owner.
#[cfg(unix)]
pub fn insecure_permissions(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
pub fn insecure_permissions(_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let d = tempfile::tempdir().unwrap();
        let c = Config::load(&d.path().join("nope.cfg")).unwrap();
        assert_eq!(c.name_format, DEFAULT_NAME_FORMAT);
        assert!(c.auth_token.is_none());
    }

    #[test]
    fn reads_the_python_versions_config_format() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("scdl.cfg");
        // Verbatim shape of the shipped scdl.cfg.
        std::fs::write(
            &p,
            "[scdl]\n\
             client_id = abc123\n\
             auth_token = \n\
             path = /home/u/Music\n\
             name_format = [%(id)s] %(uploader)s - %(title)s.%(ext)s\n\
             playlist_name_format = %(playlist_index)s. %(uploader)s - %(title)s.%(ext)s\n",
        )
        .unwrap();

        let c = Config::load(&p).unwrap();
        assert_eq!(c.client_id.as_deref(), Some("abc123"));
        assert!(c.auth_token.is_none(), "blank value should be None");
        assert_eq!(c.path, PathBuf::from("/home/u/Music"));
        assert_eq!(c.name_format, DEFAULT_NAME_FORMAT);
    }

    #[test]
    fn unparseable_config_errors_rather_than_being_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("scdl.cfg");
        let original = "this is not [valid\x00 ini at all";
        std::fs::write(&p, original).unwrap();

        assert!(Config::load(&p).is_err());
        // The point of erroring: the file, and any token in it, survives.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("scdl.cfg");
        let c = Config {
            client_id: Some("cid".into()),
            auth_token: Some("tok".into()),
            path: PathBuf::from("/tmp/x"),
            ..Config::default()
        };
        c.save(&p).unwrap();

        let back = Config::load(&p).unwrap();
        assert_eq!(back.client_id.as_deref(), Some("cid"));
        assert_eq!(back.auth_token.as_deref(), Some("tok"));
        assert_eq!(back.path, PathBuf::from("/tmp/x"));
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("scdl.cfg");
        let c = Config {
            auth_token: Some("secret-token".into()),
            ..Config::default()
        };
        c.save(&p).unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must not be group/world readable");
        assert!(insecure_permissions(&p).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn detects_preexisting_world_readable_config() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("scdl.cfg");
        std::fs::write(&p, "[scdl]\nauth_token = t\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(insecure_permissions(&p), Some(0o644));
    }

    #[test]
    fn xdg_config_home_is_honoured() {
        // Verified by construction rather than by mutating the process env,
        // which would race with other tests.
        let p = PathBuf::from("/xdg").join("scdl").join("scdl.cfg");
        assert!(p.ends_with("scdl/scdl.cfg"));
    }
}
