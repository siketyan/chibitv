use std::net::{Ipv6Addr, SocketAddr};
use std::path::Path;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

use crate::channel::DeliverySystem;

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

/// A tuner, as `[[tuners]]` describes one.
#[derive(Clone, Debug, Deserialize)]
pub struct TunerConfig {
    #[serde(flatten)]
    pub kind: TunerKind,

    /// The broadcasts the tuner receives, which is what it is picked for.
    pub delivery_systems: Vec<DeliverySystem>,
}

/// How a tuner is driven, which `type` picks along with the keys of it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TunerKind {
    Stdin,

    #[cfg(all(feature = "dvb", target_os = "linux"))]
    Dvb {
        adapter_num: u8,
        frontend_num: u8,
    },

    /// A BonDriver DLL, which is how tuners are driven on Windows.
    #[cfg(all(feature = "bon", windows))]
    Bon {
        path: std::path::PathBuf,
    },

    /// A character device of px4_drv, the Linux driver of the PLEX and
    /// Digibest tuners.
    #[cfg(all(feature = "px4", target_os = "linux"))]
    Px4 {
        path: std::path::PathBuf,
        /// The voltage the tuner feeds the dish's converter with while a
        /// satellite channel is tuned: 0 for none, 11 or 15.
        #[serde(default)]
        lnb_voltage: u8,
    },

    /// A receiver of `DriverHost_PX4`, the Windows driver of px4_drv: the one
    /// named, or any free one receiving the tuner's broadcasts.
    #[cfg(all(feature = "px4", windows))]
    Px4 {
        #[serde(default)]
        receiver: Option<String>,
        /// The driver to start when it is not running, relative to the
        /// working directory.
        #[serde(default = "default_driver_host")]
        driver_host: std::path::PathBuf,
        /// The voltage the tuner feeds the dish's converter with while a
        /// satellite channel is tuned: 0 for none, 11 or 15.
        #[serde(default)]
        lnb_voltage: u8,
    },
}

#[cfg(all(feature = "px4", windows))]
fn default_driver_host() -> std::path::PathBuf {
    "DriverHost_PX4.exe".into()
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
    pub tuners: Vec<TunerConfig>,
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

    #[cfg(all(feature = "px4", target_os = "linux"))]
    #[test]
    fn leaves_the_lnb_of_a_px4_tuner_unpowered_unless_told() {
        #[derive(Deserialize)]
        struct Tuners {
            tuners: Vec<TunerConfig>,
        }

        let configured = toml::from_str::<Tuners>(
            r#"
                [[tuners]]
                type = "px4"
                path = "/dev/pxmlt5video0"
                delivery_systems = ["ISDB-T"]

                [[tuners]]
                type = "px4"
                path = "/dev/pxmlt5video1"
                lnb_voltage = 15
                delivery_systems = ["ISDB-S"]
            "#,
        )
        .unwrap();

        let [
            TunerConfig {
                kind:
                    TunerKind::Px4 {
                        path: first,
                        lnb_voltage: 0,
                    },
                ..
            },
            TunerConfig {
                kind:
                    TunerKind::Px4 {
                        path: second,
                        lnb_voltage: 15,
                    },
                ..
            },
        ] = configured.tuners.as_slice()
        else {
            panic!("{:?}", configured.tuners);
        };
        assert_eq!(first, std::path::Path::new("/dev/pxmlt5video0"));
        assert_eq!(second, std::path::Path::new("/dev/pxmlt5video1"));
    }

    #[test]
    fn requires_the_delivery_systems_of_a_tuner() {
        #[derive(Deserialize)]
        struct Tuners {
            tuners: Vec<TunerConfig>,
        }

        let configured = toml::from_str::<Tuners>(
            r#"
                [[tuners]]
                type = "stdin"
                delivery_systems = ["ISDB-S3"]
            "#,
        )
        .unwrap();

        assert!(matches!(configured.tuners[0].kind, TunerKind::Stdin));
        assert_eq!(
            configured.tuners[0].delivery_systems,
            vec![DeliverySystem::IsdbS3]
        );

        assert!(
            toml::from_str::<Tuners>(
                r#"
                    [[tuners]]
                    type = "stdin"
                "#
            )
            .is_err()
        );
        assert!(
            toml::from_str::<Tuners>(
                r#"
                    [[tuners]]
                    type = "stdin"
                    delivery_systems = ["ISDB-C"]
                "#
            )
            .is_err()
        );
    }
}
