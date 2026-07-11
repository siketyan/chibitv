mod channel;
mod command;
mod config;
mod hevc;
mod m2ts;
mod mmt;
mod mp2;
mod mp4;
mod proto {
    connectrpc::include_generated!();
}
mod registry;
mod remux;
mod rpc;
mod server;
mod stream;
mod tuner;
mod workspace;

use clap::Parser;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

use crate::command::Command;
use crate::config::Config;

#[derive(Clone, Debug, Parser)]
struct Options {
    #[clap(subcommand)]
    command: Command,

    /// Perform verbose logging
    #[clap(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = Options::parse();

    let env_filter = EnvFilter::builder()
        .with_default_directive(
            match options.verbose {
                true => LevelFilter::TRACE,
                _ => LevelFilter::INFO,
            }
            .into(),
        )
        .from_env_lossy();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .init();

    let config = Config::load_from_file("./config.toml")?;

    options.command.run(&config).await
}
