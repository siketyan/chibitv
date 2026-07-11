use std::net::{Ipv6Addr, SocketAddr};
use std::path::Path;

use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};

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

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TunerConfig {
    Stdin,
    Dvb { adapter_num: u8, frontend_num: u8 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "delivery_system")]
pub enum ChannelConfigInner {
    #[serde(rename = "ISDB-S")]
    IsdbS { frequency: u32, stream_id: u32 },

    #[serde(rename = "ISDB-T")]
    IsdbT {
        frequency: u32,

        #[serde(
            default = "default_isdb_t_bandwidth_hz",
            skip_serializing_if = "is_default_isdb_t_bandwidth_hz"
        )]
        bandwidth_hz: u32,
    },
}

fn default_isdb_t_bandwidth_hz() -> u32 {
    6_000_000
}

fn is_default_isdb_t_bandwidth_hz(value: &u32) -> bool {
    *value == default_isdb_t_bandwidth_hz()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChannelConfig {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_stream_id: Option<u16>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceConfig>,

    #[serde(flatten)]
    pub inner: ChannelConfigInner,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServiceConfig {
    pub id: u16,
    pub name: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_name: String,
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

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct ChannelList {
        channels: Vec<ChannelConfig>,
    }

    #[test]
    fn reads_scanned_services_from_channel_config() {
        let config = toml::from_str::<ChannelList>(
            r#"
                [[channels]]
                name = "TOKYO MX"
                delivery_system = "ISDB-T"
                frequency = 515142857
                bandwidth_hz = 6000000
                transport_stream_id = 12345

                [[channels.services]]
                id = 23608
                name = "TOKYO MX1"
                provider_name = "TOKYO MX"
            "#,
        )
        .unwrap();

        let channel = &config.channels[0];
        assert_eq!(channel.transport_stream_id, Some(12345));
        assert_eq!(channel.services.len(), 1);
        assert_eq!(channel.services[0].id, 23608);
        assert_eq!(channel.services[0].name, "TOKYO MX1");
    }

    #[test]
    fn keeps_legacy_channel_config_compatible() {
        let config = toml::from_str::<ChannelList>(
            r#"
                [[channels]]
                name = "Legacy"
                delivery_system = "ISDB-T"
                frequency = 515142857
            "#,
        )
        .unwrap();

        let channel = &config.channels[0];
        assert_eq!(channel.transport_stream_id, None);
        assert!(channel.services.is_empty());
    }
}
