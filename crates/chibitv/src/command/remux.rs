use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write, stdin, stdout};
use std::sync::{Arc, Mutex};

use chibitv_b61::{B61CasModule, Descrambler};
use clap::{Parser, ValueEnum};
use mpeg2ts::ts::TsPacketWriter;

use crate::config::Config;
use crate::m2ts::{M2tsDemuxer, M2tsMuxer};
use crate::mmt::MmtDemuxer;
use crate::mp4::{FragmentedMp4Muxer, Mp4Muxer};
use crate::remux::{Remux, Remuxer};

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum InputFormat {
    /// MMT/TLV stream
    #[default]
    Mmts,
    /// MPEG-2 Transport Stream
    M2ts,
}

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    /// MPEG-2 Transport Stream
    #[default]
    M2ts,
    /// MPEG-4 / ISO BMFF
    Mp4,
    /// Fragmented MP4
    Fmp4,
}

#[derive(Clone, Debug, Parser)]
pub struct Options {
    /// Source path of the input stream.
    input: Option<String>,

    /// Destination path of the output stream.
    #[clap(short, long)]
    output: Option<String>,

    /// Format of the input stream.
    #[clap(long)]
    input_format: Option<InputFormat>,

    /// Format of the output stream.
    #[clap(short, long)]
    format: Option<OutputFormat>,
}

pub async fn remux(options: &Options, config: &Config) -> anyhow::Result<()> {
    let input = open_input(options)?;

    match options.input_format.unwrap_or_default() {
        InputFormat::Mmts => remux_mmts(input, options, config),
        InputFormat::M2ts => remux_m2ts(input, options),
    }
}

fn open_input(options: &Options) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
    Ok(match options.input.as_deref() {
        Some("-") | None => Box::new(stdin()),
        Some(path) => Box::new(File::open(path)?),
    })
}

fn open_output(options: &Options) -> anyhow::Result<Box<dyn Write + Send + Sync>> {
    Ok(match options.output.as_deref() {
        Some("-") | None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path)?),
    })
}

fn remux_mmts(
    input: Box<dyn Read + Send + Sync>,
    options: &Options,
    config: &Config,
) -> anyhow::Result<()> {
    let cas_module = Arc::new(Mutex::new(B61CasModule::open(
        config.cas.master_key.into(),
    )?));
    let descrambler = Descrambler::init(cas_module, false)?;
    let reader = BufReader::new(input);
    let demux = MmtDemuxer::new(reader, descrambler);

    match options.format.unwrap_or_default() {
        OutputFormat::M2ts => {
            let output = open_output(options)?;
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
        OutputFormat::Fmp4 => {
            let output = open_output(options)?;
            let mux = FragmentedMp4Muxer::new(BufWriter::new(output));
            let mut remux = Remuxer::new(demux, mux, None, None);

            remux.run(None)
        }
    }
}

fn remux_m2ts(input: Box<dyn Read + Send + Sync>, options: &Options) -> anyhow::Result<()> {
    match options.format.unwrap_or_default() {
        OutputFormat::M2ts => {
            let output = open_output(options)?;
            let writer = TsPacketWriter::new(BufWriter::new(output));
            let mux = M2tsMuxer::new(writer);
            let mut remux = Remuxer::new(M2tsDemuxer::new(input), mux, None, None);

            remux.run(None)
        }
        OutputFormat::Mp4 => {
            todo!("MP4 remux from ISDB-T MPEG-TS is not supported yet");
        }
        OutputFormat::Fmp4 => {
            todo!("fragmented MP4 remux from ISDB-T MPEG-TS is not supported yet");
        }
    }
}
