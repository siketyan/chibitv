use std::net::{Ipv6Addr, SocketAddr};
use std::path::Path;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TunerConfig {
    Stdin,
    Dvb { adapter_num: u8, frontend_num: u8 },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "delivery_system")]
pub enum ChannelConfigInner {
    #[serde(rename = "ISDB-S")]
    IsdbS { frequency: u32, stream_id: u32 },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelConfig {
    pub name: String,

    #[serde(flatten)]
    pub inner: ChannelConfigInner,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub cas: CasConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub tuners: Vec<TunerConfig>,

    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
}

impl Config {
    pub fn load_from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = std::fs::read_to_string(path)?;
        let config = toml::from_str(&file)?;

        Ok(config)
    }
}
