use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write, stdin, stdout};
use std::sync::{Arc, Mutex};

use chibitv_b61::CasModule;
use clap::{Parser, ValueEnum};
use mpeg2ts::ts::TsPacketWriter;

use crate::config::Config;
use crate::descrambler::Descrambler;
use crate::m2ts::M2tsMuxer;
use crate::mmt::MmtDemuxer;
use crate::mp4::Mp4Muxer;
use crate::remux::{Remux, Remuxer};

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    M2ts,
    Mp4,
}

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Source path of the input stream.
    input: Option<String>,

    /// Destination path of the output stream.
    #[clap(short, long)]
    output: Option<String>,

    /// Format of the output stream.
    #[clap(short, long)]
    format: Option<OutputFormat>,
}

pub async fn remux(options: &Options, config: &Config) -> anyhow::Result<()> {
    let input: Box<dyn Read + Send + Sync> = match options.input.as_deref() {
        Some("-") | None => Box::new(stdin()),
        Some(path) => Box::new(File::open(path)?),
    };

    let cas_module = CasModule::open()?;
    let descrambler = Descrambler::init(cas_module, config.cas.master_key.into())?;
    let reader = BufReader::new(input);
    let demux = MmtDemuxer::new(reader, Arc::new(Mutex::new(descrambler)));

    match options.format.unwrap_or_default() {
        OutputFormat::M2ts => {
            let output: Box<dyn Write + Send + Sync> = match options.output.as_deref() {
                Some("-") | None => Box::new(stdout()),
                Some(path) => Box::new(File::create(path)?),
            };

            let writer = TsPacketWriter::new(BufWriter::new(output));
            let mux = M2tsMuxer::new(writer);
            let mut remux = Remuxer::new(demux, mux, None, None);

            remux.run(None)
        }
        OutputFormat::Mp4 => {
            let Some(path) = options.output.as_deref() else {
                anyhow::bail!("Output path is required for MP4 format.");
            };

            let mux = Mp4Muxer::new(BufWriter::new(File::create(path)?));
            let mut remux = Remuxer::new(demux, mux, None, None);

            remux.run(None)
        }
    }
}
