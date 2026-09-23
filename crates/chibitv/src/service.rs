//! The services on air, as the SI and a scan describe them.

use chibitv_b10::descriptor::Descriptor as B10Descriptor;
use chibitv_b10::table::ServiceInformation as B10ServiceInformation;
use chibitv_b24::decode as decode_b24;
use chibitv_b60::descriptor::Descriptor as B60Descriptor;
use chibitv_b60::table::ServiceInformation as B60ServiceInformation;

/// Identifies one service among the ones on air.
///
/// A service id alone does not tell a service apart from every other: BS 2K
/// and BS 4K number theirs alike, so the stream carrying one goes with it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceKey {
    /// The TLV stream id on ISDB-S3, the transport stream id on ISDB-T and
    /// ISDB-S.
    pub stream_id: u16,
    pub service_id: u16,
}

/// A service on air, under the stream carrying it.
///
/// The channel it belongs to is the one carrying that stream, which
/// [`crate::workspace::Workspace`] works out out of the channels being served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Service {
    pub key: ServiceKey,
    pub name: String,
    pub provider_name: String,
}

/// One service of a stream, as a scan or the SDT describes it, which the
/// stream it is carried on is given beside.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredService {
    pub id: u16,
    pub name: String,
    pub provider_name: String,
}

impl StoredService {
    /// The service an SDT entry describes, when it is a television service.
    pub fn from_b10(service: &B10ServiceInformation) -> Option<Self> {
        let descriptor = service.descriptors.iter().find_map(|descriptor| {
            if let B10Descriptor::Service(descriptor) = descriptor {
                Some(descriptor)
            } else {
                None
            }
        })?;

        // Digital television service.
        (descriptor.service_type == 0x01).then(|| Self {
            id: service.service_id,
            name: decode_b24(&descriptor.service_name),
            provider_name: decode_b24(&descriptor.service_provider_name),
        })
    }

    /// The service an MH-SDT entry describes, when it is a television service.
    pub fn from_b60(service: &B60ServiceInformation) -> Option<Self> {
        let descriptor = service.descriptors.iter().find_map(|descriptor| {
            if let B60Descriptor::MhService(descriptor) = descriptor {
                Some(descriptor)
            } else {
                None
            }
        })?;

        // Only TV service is supported for now.
        (descriptor.service_type == 1).then(|| Self {
            id: service.service_id,
            name: String::from_utf8_lossy(&descriptor.service_name).to_string(),
            provider_name: String::from_utf8_lossy(&descriptor.service_provider_name).to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use chibitv_b10::descriptor::ServiceDescriptor;
    use chibitv_b60::descriptor::MhServiceDescriptor;

    use super::*;

    fn b10_service(service_type: u8) -> B10ServiceInformation {
        B10ServiceInformation {
            service_id: 0x5678,
            eit_user_defined_flags: 0,
            eit_schedule_flag: true,
            eit_present_following_flag: true,
            running_status: 4,
            free_ca_mode: false,
            descriptors: vec![B10Descriptor::Service(ServiceDescriptor {
                service_type,
                service_provider_name: b"\x0eProvider".to_vec(),
                service_name: b"\x0eChannel".to_vec(),
            })],
        }
    }

    #[test]
    fn reads_an_isdb_t_television_service() {
        assert_eq!(
            StoredService::from_b10(&b10_service(0x01)),
            Some(StoredService {
                id: 0x5678,
                name: "Channel".to_string(),
                provider_name: "Provider".to_string(),
            })
        );
    }

    #[test]
    fn skips_a_service_other_than_television() {
        assert_eq!(StoredService::from_b10(&b10_service(0xC0)), None);
    }

    #[test]
    fn reads_an_isdb_s3_television_service() {
        let service = B60ServiceInformation {
            service_id: 0x5678,
            eit_user_defined_flags: 0,
            eit_schedule_flag: true,
            eit_present_following_flag: true,
            running_status: 4,
            free_ca_mode: false,
            descriptors: vec![B60Descriptor::MhService(MhServiceDescriptor {
                service_type: 0x01,
                service_provider_name: b"Provider".to_vec(),
                service_name: b"Channel".to_vec(),
            })],
        };

        assert_eq!(
            StoredService::from_b60(&service).map(|service| service.name),
            Some("Channel".to_string())
        );
    }
}
