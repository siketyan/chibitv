//! A decode, filter, encode chain on one acceleration, and the search for an
//! acceleration that works on this machine.

use std::collections::VecDeque;

use bytes::Bytes;

use crate::av::{
    self, Decoder, Device, Encoder, EncoderParams, Filter, Frame, FrameInfo, Packet, Rational,
    Status, TIME_BASE,
};
use crate::backend::{self, PictureKind};
use crate::{Acceleration, Error, TranscodeOptions, VideoCodec};

/// Frames between keyframes when the stream does not say how fast it runs.
const DEFAULT_GOP_SIZE: u32 = 60;

pub struct Pipeline {
    acceleration: Acceleration,
    output: VideoCodec,
    options: TranscodeOptions,
    device: Option<Device>,
    decoder: Decoder,
    /// Set up on the first decoded picture, since what it needs depends on it.
    filter: Option<Filter>,
    filter_planned: bool,
    /// Set up on the first picture that reaches it.
    encoder: Option<Encoder>,
    decoded: Frame,
    filtered: Frame,
    packet: Packet,
    /// Fills in the timestamps of pictures the decoder could not time.
    next_pts: Option<i64>,
    queue: VecDeque<crate::Packet>,
}

impl Pipeline {
    /// Tries the accelerations the options list, in order, and keeps the
    /// first one whose decoder and encoder open here.
    pub fn open(input: VideoCodec, options: TranscodeOptions) -> Result<Self, Error> {
        av::init_logging();

        let mut attempts = Vec::new();
        for &acceleration in &options.accelerations {
            if !acceleration.compiled() {
                attempts.push((acceleration, "not compiled into this build".to_owned()));
                continue;
            }
            match Self::try_open(acceleration, input, &options) {
                Ok(pipeline) => {
                    tracing::info!(?acceleration, ?input, output = ?options.codec, "Transcoding");
                    return Ok(pipeline);
                }
                Err(Error::Codec(reason)) => {
                    tracing::debug!(?acceleration, reason, "Acceleration is not usable");
                    attempts.push((acceleration, reason));
                }
                Err(error) => return Err(error),
            }
        }

        Err(Error::Unsupported {
            input,
            output: options.codec,
            attempts,
        })
    }

    fn try_open(
        acceleration: Acceleration,
        input: VideoCodec,
        options: &TranscodeOptions,
    ) -> Result<Self, Error> {
        let output = options.codec;
        let decoder_name = backend::decoder(acceleration, input)
            .ok_or_else(|| Error::Codec(format!("cannot decode {input:?}")))?;
        let encoder_name = backend::encoder(acceleration, output)
            .ok_or_else(|| Error::Codec(format!("cannot encode {output:?}")))?;
        if !av::has_decoder(decoder_name) {
            return Err(Error::Codec(format!(
                "this build has no {decoder_name} decoder"
            )));
        }
        if !av::has_encoder(encoder_name) {
            return Err(Error::Codec(format!(
                "this build has no {encoder_name} encoder"
            )));
        }

        let device = match backend::device_type(acceleration) {
            Some(type_name) => Some(Device::create(type_name, options.device.as_deref())?),
            None => None,
        };
        let decoder = Decoder::open(
            decoder_name,
            device
                .as_ref()
                .filter(|_| backend::decodes_on_device(acceleration)),
            &backend::decoder_options(acceleration, options.deinterlace),
        )?;
        // The encoder is opened for real once a picture tells what it is
        // going to get, so this is the moment to learn that it never will.
        Encoder::probe(
            encoder_name,
            device.as_ref(),
            &backend::encoder_options(acceleration, output, options.bitrate),
        )?;

        Ok(Self {
            acceleration,
            output,
            options: options.clone(),
            device,
            decoder,
            filter: None,
            filter_planned: false,
            encoder: None,
            decoded: Frame::new()?,
            filtered: Frame::new()?,
            packet: Packet::new()?,
            next_pts: None,
            queue: VecDeque::new(),
        })
    }

    pub fn acceleration(&self) -> Acceleration {
        self.acceleration
    }

    pub fn push(&mut self, packet: &crate::Packet) -> Result<(), Error> {
        let pts = to_ticks(packet.pts);
        let dts = to_ticks(packet.dts);
        loop {
            match self.decoder.send(&packet.data, pts, dts)? {
                Status::Ok | Status::Eof => break,
                // The decoder holds pictures that must come out first.
                Status::Again => self.drain_decoder()?,
            }
        }
        self.drain_decoder()
    }

    pub fn pull(&mut self) -> Option<crate::Packet> {
        self.queue.pop_front()
    }

    pub fn finish(&mut self) -> Result<(), Error> {
        self.decoder.flush()?;
        self.drain_decoder()?;
        if let Some(filter) = &mut self.filter {
            filter.flush()?;
            self.drain_filter()?;
        }
        if let Some(encoder) = &mut self.encoder {
            encoder.flush()?;
            self.drain_encoder()?;
        }
        Ok(())
    }

    fn drain_decoder(&mut self) -> Result<(), Error> {
        loop {
            match self.decoder.receive(&mut self.decoded)? {
                Status::Again | Status::Eof => return Ok(()),
                Status::Ok => {}
            }
            self.time_picture();
            let result = self.process_decoded();
            self.decoded.clear();
            result?;
        }
    }

    /// Gives a picture the decoder could not time the one after the last.
    fn time_picture(&mut self) {
        let info = self.decoded.info();
        let pts = if info.pts == av::NO_TIMESTAMP {
            self.next_pts.unwrap_or(0)
        } else {
            info.pts
        };
        if pts != info.pts {
            self.decoded.set_timing(pts, info.duration);
        }
        self.next_pts = Some(pts + info.duration.max(0));
    }

    fn process_decoded(&mut self) -> Result<(), Error> {
        if !self.filter_planned {
            self.plan_filter()?;
        }

        if self.filter.is_some() {
            self.filter.as_mut().expect("planned").send(&self.decoded)?;
            self.drain_filter()
        } else {
            let time_base = TIME_BASE;
            let frame_rate = self.frame_rate_of_decoder();
            self.encode(time_base, frame_rate, false)
        }
    }

    fn plan_filter(&mut self) -> Result<(), Error> {
        let info = self.decoded.info();
        let picture = PictureKind {
            bit_depth: info.bit_depth,
            hardware: info.hardware,
        };
        let graph = backend::filter_graph(
            self.acceleration,
            self.options.deinterlace,
            self.output,
            picture,
        );
        if let Some(graph) = graph {
            tracing::debug!(graph, "Filtering");
            let frame_rate = self.frame_rate_of_decoder();
            self.filter = Some(Filter::open(
                &graph,
                &self.decoded,
                frame_rate,
                self.device.as_ref(),
            )?);
        }
        self.filter_planned = true;
        Ok(())
    }

    fn drain_filter(&mut self) -> Result<(), Error> {
        loop {
            let filter = self
                .filter
                .as_mut()
                .expect("draining a filter that is there");
            match filter.receive(&mut self.filtered)? {
                Status::Again | Status::Eof => return Ok(()),
                Status::Ok => {}
            }
            let (time_base, frame_rate) = filter.output();
            let result = self.encode(time_base, frame_rate, true);
            self.filtered.clear();
            result?;
        }
    }

    /// Encodes the decoded or the filtered picture, opening the encoder for
    /// it if this is the first.
    fn encode(
        &mut self,
        time_base: Rational,
        frame_rate: Option<Rational>,
        filtered: bool,
    ) -> Result<(), Error> {
        let frame = if filtered {
            &self.filtered
        } else {
            &self.decoded
        };
        if self.encoder.is_none() {
            let frame_rate = frame_rate.or_else(|| frame_rate_of_duration(frame.info(), time_base));
            let gop_size = frame_rate
                .map(|rate| (self.options.keyframe_interval * rate.as_f64()).round() as u32)
                .filter(|gop| *gop > 0)
                .unwrap_or(DEFAULT_GOP_SIZE);
            let params = EncoderParams {
                time_base,
                frame_rate,
                bit_rate: self.options.bitrate,
                gop_size: Some(gop_size),
                options: backend::encoder_options(
                    self.acceleration,
                    self.output,
                    self.options.bitrate,
                ),
            };
            let name = backend::encoder(self.acceleration, self.output).expect("checked on open");
            self.encoder = Some(Encoder::open(name, frame, &params)?);
        }

        let encoder = self.encoder.as_mut().expect("opened above");
        loop {
            match encoder.send(frame)? {
                Status::Ok | Status::Eof => break,
                Status::Again => {}
            }
            // The encoder holds packets that must come out first.
            Self::drain_encoder_into(encoder, &mut self.packet, &mut self.queue)?;
        }
        Self::drain_encoder_into(encoder, &mut self.packet, &mut self.queue)
    }

    fn drain_encoder(&mut self) -> Result<(), Error> {
        let encoder = self
            .encoder
            .as_mut()
            .expect("draining an encoder that is there");
        Self::drain_encoder_into(encoder, &mut self.packet, &mut self.queue)
    }

    fn drain_encoder_into(
        encoder: &mut Encoder,
        packet: &mut Packet,
        queue: &mut VecDeque<crate::Packet>,
    ) -> Result<(), Error> {
        loop {
            match encoder.receive(packet)? {
                Status::Again | Status::Eof => return Ok(()),
                Status::Ok => {}
            }
            let info = packet.info();
            queue.push_back(crate::Packet {
                data: Bytes::copy_from_slice(info.data),
                pts: to_seconds(info.pts),
                dts: to_seconds(info.dts),
                keyframe: info.keyframe,
            });
            packet.clear();
        }
    }

    fn frame_rate_of_decoder(&self) -> Option<Rational> {
        self.decoder.frame_rate()
    }
}

/// The frame rate a picture's duration implies, when the stream does not
/// declare one.
fn frame_rate_of_duration(info: FrameInfo, time_base: Rational) -> Option<Rational> {
    if info.duration <= 0 || !time_base.is_positive() {
        return None;
    }
    // duration * num / den seconds per frame, so den / (duration * num) per second.
    let den = i64::from(time_base.num).checked_mul(info.duration)?;
    Some(Rational {
        num: time_base.den,
        den: i32::try_from(den).ok()?,
    })
}

fn to_ticks(seconds: Option<f64>) -> i64 {
    match seconds {
        Some(seconds) => (seconds * TIME_BASE.den as f64).round() as i64,
        None => av::NO_TIMESTAMP,
    }
}

fn to_seconds(ticks: i64) -> Option<f64> {
    (ticks != av::NO_TIMESTAMP).then(|| ticks as f64 / TIME_BASE.den as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_in_the_stream_clock_becomes_a_frame_rate() {
        let info = FrameInfo {
            duration: 3003,
            ..FrameInfo::default()
        };
        assert_eq!(
            frame_rate_of_duration(info, TIME_BASE),
            Some(Rational {
                num: 90_000,
                den: 3003
            })
        );
        assert_eq!(
            frame_rate_of_duration(FrameInfo::default(), TIME_BASE),
            None
        );
    }

    #[test]
    fn seconds_and_ticks_round_trip() {
        assert_eq!(to_ticks(Some(1.5)), 135_000);
        assert_eq!(to_ticks(None), av::NO_TIMESTAMP);
        assert_eq!(to_seconds(135_000), Some(1.5));
        assert_eq!(to_seconds(av::NO_TIMESTAMP), None);
    }
}
