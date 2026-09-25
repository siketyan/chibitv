use std::net::{Ipv6Addr, SocketAddr};
use std::path::Path;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

#[derive(Copy, Clone, Debug)]
pub struct CasMasterKey([u8; 32]);

impl From<CasMasterKey> for [u8; 32] {
    fn from(value: CasMasterKey) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for CasMasterKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_string = String::deserialize(deserializer)?;
        let bytes = hex::decode(hex_string).map_err(Error::custom)?;

        Ok(Self(bytes.as_slice().try_into().map_err(Error::custom)?))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CasConfig {
    pub master_key: CasMasterKey,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub address: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            address: SocketAddr::from((Ipv6Addr::LOCALHOST, 3001)),
        }
    }
}

/// Where the server keeps what has to survive a restart.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// The database to keep it in, as a URL whose scheme picks the backend.
    pub url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://chibitv.db".to_string(),
        }
    }
}

/// Where recordings are kept.
///
/// Only a directory of the file system is stored into for now; the tag is what
/// a remote store, such as an S3 bucket, would be picked with.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorageConfig {
    Directory {
        #[serde(default = "default_storage_path")]
        path: std::path::PathBuf,
    },
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self::Directory {
            path: default_storage_path(),
        }
    }
}

fn default_storage_path() -> std::path::PathBuf {
    std::path::PathBuf::from("./recordings")
}

/// How tunelithd, which the tuners are all had from, is reached.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TunelithConfig {
    /// The socket of tunelithd; that of the user's if there is one, or else
    /// the system's.
    #[serde(default)]
    pub socket: Option<std::path::PathBuf>,
    /// Whether the tuner powers the dish's converter while a satellite
    /// channel is tuned.
    #[serde(default)]
    pub lnb: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub cas: CasConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub database: DatabaseConfig,

    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub tunelith: TunelithConfig,
}

impl Config {
    pub fn load_from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = std::fs::read_to_string(path)?;
        let config = toml::from_str(&file)?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[test]
    fn stores_recordings_in_the_configured_directory() {
        #[derive(Deserialize)]
        struct Storage {
            #[serde(default)]
            storage: StorageConfig,
        }

        let configured = toml::from_str::<Storage>(
            r#"
                [storage]
                type = "directory"
                path = "/srv/recordings"
            "#,
        )
        .unwrap();
        let StorageConfig::Directory { path } = configured.storage;
        assert_eq!(path, std::path::Path::new("/srv/recordings"));

        let StorageConfig::Directory { path } = toml::from_str::<Storage>("").unwrap().storage;
        assert_eq!(path, std::path::Path::new("./recordings"));
    }
}
