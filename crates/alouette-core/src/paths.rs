//! Where Alouette keeps its files.
//!
//! By default these are the platform directories (XDG on Linux, `~/Library` on macOS,
//! `%APPDATA%` on Windows). Setting `ALOUETTE_HOME` puts everything under one folder,
//! which tests and SSH serve mode use to give each user an isolated home.

use std::path::{Path, PathBuf};

/// Every directory and well-known file Alouette uses.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl Paths {
    /// Resolve paths from `ALOUETTE_HOME` or the platform defaults.
    pub fn discover() -> Self {
        if let Some(home) = std::env::var_os("ALOUETTE_HOME") {
            return Self::under(Path::new(&home));
        }
        match directories::ProjectDirs::from("org", "Alouette", "alouette") {
            Some(dirs) => Self {
                config_dir: dirs.config_dir().to_path_buf(),
                data_dir: dirs.data_dir().to_path_buf(),
                cache_dir: dirs.cache_dir().to_path_buf(),
                log_dir: dirs.data_local_dir().join("logs"),
            },
            None => Self::under(Path::new(".alouette")),
        }
    }

    /// Put everything under one root folder.
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("alouette.toml")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    pub fn state_db(&self) -> PathBuf {
        self.data_dir.join("state.sqlite3")
    }

    /// Create every directory that does not exist yet.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.config_dir, &self.data_dir, &self.cache_dir, &self.log_dir] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
