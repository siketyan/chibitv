use std::time::Duration;

use clap::Parser;

use crate::channel::format_channel_list;
use crate::channel_scanner::{
    ChannelScanner, FIRST_UHF_CHANNEL, LAST_UHF_CHANNEL, ScanDeliverySystem, ScanRequest,
};
use crate::config::Config;
use crate::store;

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Broadcast to scan.
    #[clap(long, value_enum, ignore_case = true, default_value = "ISDB-T")]
    delivery_system: ScanDeliverySystem,

    /// First UHF physical channel to scan. ISDB-T only.
    #[clap(long, default_value_t = FIRST_UHF_CHANNEL)]
    start_channel: u8,

    /// Last UHF physical channel to scan. ISDB-T only.
    #[clap(long, default_value_t = LAST_UHF_CHANNEL)]
    end_channel: u8,

    /// Maximum time in seconds to wait on each channel.
    #[clap(long, default_value_t = 12)]
    timeout: u64,

    /// Read the channel list out of the signalling on one transponder per
    /// network instead of tuning to every stream. Satellite only.
    #[clap(long)]
    fast: bool,
}

pub async fn scan(options: &Options, config: &Config) -> anyhow::Result<()> {
    let request = ScanRequest {
        delivery_system: options.delivery_system,
        fast: options.fast,
        uhf_channels: options.start_channel..=options.end_channel,
        timeout: Duration::from_secs(options.timeout),
    };
    // The request is looked at before a tuner or a card is, so that a mistake
    // on the command line is what the error talks about.
    request.validate()?;

    let found = ChannelScanner::from_config(config)?.scan(&request, None)?;

    // The channels of the broadcast that was walked are replaced by what was
    // found, so one that has left the air stops being kept, while the channels
    // of the other broadcasts are left alone.
    let store = store::open(&config.database.url).await?;
    store
        .replace_channels(options.delivery_system.into(), &found)
        .await?;

    print!("{}", format_channel_list(&store.load_channels().await?));

    Ok(())
}
