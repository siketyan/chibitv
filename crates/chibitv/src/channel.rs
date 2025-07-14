use crate::config::ChannelConfigInner;

#[derive(Clone, Debug)]
pub enum ChannelInner {
    IsdbS { frequency: u32, stream_id: u32 },
}

impl From<&ChannelConfigInner> for ChannelInner {
    fn from(value: &ChannelConfigInner) -> Self {
        match value {
            ChannelConfigInner::IsdbS {
                frequency,
                stream_id,
            } => Self::IsdbS {
                frequency: *frequency,
                stream_id: *stream_id,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: usize,
    pub name: String,
    pub inner: ChannelInner,
}
