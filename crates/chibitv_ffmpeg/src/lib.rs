//! Video transcoding for chibitv, done by a minimal FFmpeg built into the
//! crate.
//!
//! A [`Transcoder`] takes the access units of one video stream and gives
//! back those of another: MPEG-2 or HEVC in, H.264, HEVC or AV1 out, with
//! deinterlacing on the way where broadcast video needs it. A stream that is
//! already in the codec asked for goes through untouched.
//!
//! The work is done on a hardware [`Acceleration`] when one is compiled in and
//! usable on this machine, and by software encoders otherwise; the Cargo
//! features of the crate decide what gets compiled in, and
//! [`Acceleration::available`] tells what did. With no backend feature at all
//! the crate builds without FFmpeg and can only pass streams through.
//!
//! Access units are the codecs' own elementary streams: MPEG-2 pictures with
//! their headers, H.264 and HEVC in Annex B with the parameter sets in-band at
//! every keyframe, and AV1 temporal units. Timestamps are seconds, as
//! elsewhere in chibitv.

use std::collections::VecDeque;
use std::fmt;

use bytes::Bytes;

#[cfg(ffmpeg)]
mod av;
#[cfg(any(ffmpeg, test))]
mod backend;
#[cfg(ffmpeg)]
mod pipeline;
#[cfg(ffmpeg)]
mod sys;

/// A video codec the transcoder reads or writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum VideoCodec {
    /// MPEG-2 Video, as ISDB-T broadcasts carry.
    Mpeg2,
    H264,
    /// HEVC, as ISDB-S 4K broadcasts carry.
    H265,
    Av1,
}

/// A way of doing the work, in the order they are tried by default: the
/// vendor-specific accelerations first, software last.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Acceleration {
    /// Apple VideoToolbox (macOS).
    VideoToolbox,
    /// NVIDIA NVDEC and NVENC.
    Nvidia,
    /// Intel Quick Sync Video.
    Qsv,
    /// VA-API (Linux; Intel and AMD).
    Vaapi,
    /// AMD AMF, encoding only: decoding is done in software.
    Amf,
    /// The software decoders and encoders.
    Software,
}

impl Acceleration {
    /// Every acceleration, in the order they are tried by default.
    pub const ALL: [Acceleration; 6] = [
        Acceleration::VideoToolbox,
        Acceleration::Nvidia,
        Acceleration::Qsv,
        Acceleration::Vaapi,
        Acceleration::Amf,
        Acceleration::Software,
    ];

    /// Whether this build has the acceleration compiled in. Whether the
    /// machine has the hardware is only found out by opening a [`Transcoder`].
    pub fn compiled(self) -> bool {
        match self {
            Acceleration::VideoToolbox => cfg!(ffmpeg_videotoolbox),
            Acceleration::Nvidia => cfg!(ffmpeg_nvidia),
            Acceleration::Qsv => cfg!(ffmpeg_qsv),
            Acceleration::Vaapi => cfg!(ffmpeg_vaapi),
            Acceleration::Amf => cfg!(ffmpeg_amf),
            Acceleration::Software => {
                cfg!(any(ffmpeg_x264, ffmpeg_x265, ffmpeg_svt_av1))
            }
        }
    }

    /// The accelerations this build has, in the order they are tried by default.
    pub fn available() -> Vec<Acceleration> {
        Self::ALL
            .into_iter()
            .filter(|acceleration| acceleration.compiled())
            .collect()
    }
}

/// What to do about interlaced pictures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Deinterlace {
    /// Encode them as they are.
    #[default]
    Off,
    /// One progressive frame per interlaced frame, keeping the frame rate.
    Frame,
    /// One progressive frame per field, doubling the frame rate.
    Field,
}

/// How a [`Transcoder`] is to work.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscodeOptions {
    /// The codec to produce.
    pub codec: VideoCodec,
    pub deinterlace: Deinterlace,
    /// The accelerations to try, in order. Ones this build lacks are skipped.
    pub accelerations: Vec<Acceleration>,
    /// The device to open for a hardware acceleration, in the form its driver
    /// takes (a render node such as `/dev/dri/renderD128` for VA-API, a GPU
    /// index for NVIDIA), or the default one.
    pub device: Option<String>,
    /// Bits per second; `None` leaves the rate control to the encoder.
    pub bitrate: Option<u32>,
    /// Seconds between keyframes.
    pub keyframe_interval: f64,
    /// Whether a stream already in the codec asked for is passed through
    /// untouched instead of being re-encoded.
    pub passthrough: bool,
}

impl TranscodeOptions {
    /// Options producing the codec, on whatever acceleration works.
    pub fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            deinterlace: Deinterlace::Off,
            accelerations: Acceleration::available(),
            device: None,
            bitrate: None,
            keyframe_interval: 2.0,
            passthrough: true,
        }
    }
}

/// One access unit of a video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub data: Bytes,
    /// Presentation time in seconds.
    pub pts: Option<f64>,
    /// Decoding time in seconds.
    pub dts: Option<f64>,
    /// Whether decoding can start here. Only meaningful on output; the
    /// decoders work it out for themselves on input.
    pub keyframe: bool,
}

#[derive(Debug)]
pub enum Error {
    /// No acceleration can transcode between these codecs here, each for the
    /// reason given.
    Unsupported {
        input: VideoCodec,
        output: VideoCodec,
        attempts: Vec<(Acceleration, String)>,
    },
    /// The decoder, filter or encoder failed.
    Codec(String),
    /// A packet was pushed after [`Transcoder::finish`].
    Finished,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported {
                input,
                output,
                attempts,
            } => {
                write!(f, "cannot transcode {input:?} to {output:?}")?;
                if attempts.is_empty() {
                    write!(f, ": no acceleration is compiled into this build")?;
                }
                for (acceleration, reason) in attempts {
                    write!(f, "; {acceleration:?}: {reason}")?;
                }
                Ok(())
            }
            Error::Codec(message) => write!(f, "{message}"),
            Error::Finished => write!(f, "the transcoder is finished"),
        }
    }
}

impl std::error::Error for Error {}

/// Turns the access units of one video stream into those of another.
///
/// Push access units in decoding order, then pull what came out; encoders
/// hold pictures back, so output lags input and the rest comes after
/// [`Transcoder::finish`].
pub struct Transcoder {
    input: VideoCodec,
    output: VideoCodec,
    inner: Inner,
    finished: bool,
}

enum Inner {
    Passthrough(VecDeque<Packet>),
    #[cfg(ffmpeg)]
    Pipeline(Box<pipeline::Pipeline>),
}

impl Transcoder {
    /// Opens a transcoder from `input` to what the options ask for, on the
    /// first of their accelerations that works on this machine.
    pub fn new(input: VideoCodec, options: TranscodeOptions) -> Result<Self, Error> {
        let output = options.codec;
        if options.passthrough && input == output {
            return Ok(Self {
                input,
                output,
                inner: Inner::Passthrough(VecDeque::new()),
                finished: false,
            });
        }

        #[cfg(ffmpeg)]
        {
            let pipeline = pipeline::Pipeline::open(input, options)?;
            Ok(Self {
                input,
                output,
                inner: Inner::Pipeline(Box::new(pipeline)),
                finished: false,
            })
        }
        #[cfg(not(ffmpeg))]
        {
            Err(Error::Unsupported {
                input,
                output,
                attempts: Vec::new(),
            })
        }
    }

    pub fn input(&self) -> VideoCodec {
        self.input
    }

    pub fn output(&self) -> VideoCodec {
        self.output
    }

    /// The acceleration doing the work, or `None` when the stream is passed
    /// through.
    pub fn acceleration(&self) -> Option<Acceleration> {
        match &self.inner {
            Inner::Passthrough(_) => None,
            #[cfg(ffmpeg)]
            Inner::Pipeline(pipeline) => Some(pipeline.acceleration()),
        }
    }

    /// Feeds one access unit.
    pub fn push(&mut self, packet: Packet) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Finished);
        }
        match &mut self.inner {
            Inner::Passthrough(queue) => {
                queue.push_back(packet);
                Ok(())
            }
            #[cfg(ffmpeg)]
            Inner::Pipeline(pipeline) => pipeline.push(&packet),
        }
    }

    /// Takes the next access unit that came out, if any is ready.
    pub fn pull(&mut self) -> Option<Packet> {
        match &mut self.inner {
            Inner::Passthrough(queue) => queue.pop_front(),
            #[cfg(ffmpeg)]
            Inner::Pipeline(pipeline) => pipeline.pull(),
        }
    }

    /// Tells the transcoder no more input is coming, so that it lets go of
    /// everything it holds. What is left comes out of [`Transcoder::pull`].
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        match &mut self.inner {
            Inner::Passthrough(_) => Ok(()),
            #[cfg(ffmpeg)]
            Inner::Pipeline(pipeline) => pipeline.finish(),
        }
    }
}

/// The version of FFmpeg built into the crate, or `None` when there is none.
pub fn ffmpeg_version() -> Option<&'static str> {
    #[cfg(ffmpeg)]
    {
        Some(av::version())
    }
    #[cfg(not(ffmpeg))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(data: &'static [u8], pts: f64) -> Packet {
        Packet {
            data: Bytes::from_static(data),
            pts: Some(pts),
            dts: Some(pts),
            keyframe: false,
        }
    }

    #[test]
    fn a_stream_already_in_the_codec_passes_through_untouched() {
        let mut transcoder =
            Transcoder::new(VideoCodec::H265, TranscodeOptions::new(VideoCodec::H265)).unwrap();
        assert_eq!(transcoder.acceleration(), None);

        transcoder.push(packet(b"first", 1.0)).unwrap();
        transcoder.push(packet(b"second", 2.0)).unwrap();
        assert_eq!(transcoder.pull(), Some(packet(b"first", 1.0)));
        transcoder.finish().unwrap();
        assert_eq!(transcoder.pull(), Some(packet(b"second", 2.0)));
        assert_eq!(transcoder.pull(), None);
        assert!(matches!(
            transcoder.push(packet(b"late", 3.0)),
            Err(Error::Finished)
        ));
    }

    #[test]
    fn available_accelerations_are_the_compiled_ones_in_order() {
        let available = Acceleration::available();
        assert!(available.iter().all(|acceleration| acceleration.compiled()));
        assert_eq!(
            available,
            Acceleration::ALL
                .into_iter()
                .filter(|acceleration| acceleration.compiled())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            TranscodeOptions::new(VideoCodec::H264).accelerations,
            available
        );
    }

    #[cfg(not(ffmpeg))]
    #[test]
    fn nothing_but_passthrough_works_without_a_backend() {
        assert_eq!(ffmpeg_version(), None);
        let error = Transcoder::new(VideoCodec::Mpeg2, TranscodeOptions::new(VideoCodec::H264))
            .err()
            .unwrap();
        assert!(matches!(&error, Error::Unsupported { attempts, .. } if attempts.is_empty()));
        assert_eq!(
            error.to_string(),
            "cannot transcode Mpeg2 to H264: no acceleration is compiled into this build"
        );
    }

    #[cfg(ffmpeg)]
    mod with_ffmpeg {
        use super::*;
        use crate::av::{
            Encoder, EncoderParams, Frame, Packet as EncodedPacket, Rational, Status, TIME_BASE,
        };

        const WIDTH: u32 = 320;
        const HEIGHT: u32 = 240;
        const FRAMES: usize = 30;
        /// 29.97 frames per second in the 90 kHz clock.
        const DURATION: i64 = 3003;

        /// A moving pattern that gives the encoders something to do.
        fn draw(frame: &mut Frame, index: usize) {
            let (luma, stride) = frame.plane(0).unwrap();
            for y in 0..HEIGHT as usize {
                for x in 0..WIDTH as usize {
                    luma[y * stride + x] = ((x + y + index * 4) & 0xFF) as u8;
                }
            }
            for plane in 1..3 {
                let (chroma, stride) = frame.plane(plane).unwrap();
                for y in 0..(HEIGHT as usize).div_ceil(2) {
                    for x in 0..(WIDTH as usize).div_ceil(2) {
                        chroma[y * stride + x] =
                            (128 + ((x * plane as usize + index) & 0x3F)) as u8;
                    }
                }
            }
        }

        /// Encodes the pattern with one of FFmpeg's own encoders into an
        /// elementary stream, timed in seconds like a demuxer would.
        fn encode_stream(codec: &str, options: &str, interlaced: bool) -> Vec<Packet> {
            // The MPEG-2 encoder reads its frame rate off the clock the
            // pictures are timed in, so its pictures are timed in frames.
            let time_base = if codec == "mpeg2video" {
                Rational {
                    num: 1001,
                    den: 30_000,
                }
            } else {
                TIME_BASE
            };
            let duration = if time_base == TIME_BASE { DURATION } else { 1 };
            let params = EncoderParams {
                time_base,
                frame_rate: Some(Rational {
                    num: 30_000,
                    den: 1001,
                }),
                bit_rate: Some(2_000_000),
                gop_size: Some(15),
                options: options.into(),
            };
            let mut encoder = None;
            let mut packet = EncodedPacket::new().unwrap();
            let mut packets = Vec::new();

            let drain =
                |encoder: &mut Encoder, packet: &mut EncodedPacket, packets: &mut Vec<Packet>| {
                    while encoder.receive(packet).unwrap() == Status::Ok {
                        let info = packet.info();
                        packets.push(Packet {
                            data: Bytes::copy_from_slice(info.data),
                            pts: Some(info.pts as f64 / 90_000.0),
                            dts: Some(info.dts as f64 / 90_000.0),
                            keyframe: info.keyframe,
                        });
                        packet.clear();
                    }
                };

            for index in 0..FRAMES {
                let mut frame = Frame::picture("yuv420p", WIDTH, HEIGHT).unwrap();
                draw(&mut frame, index);
                frame.set_timing(index as i64 * duration, duration);
                if interlaced {
                    frame.set_interlaced(true);
                }
                let encoder =
                    encoder.get_or_insert_with(|| Encoder::open(codec, &frame, &params).unwrap());
                assert_eq!(encoder.send(&frame).unwrap(), Status::Ok);
                drain(encoder, &mut packet, &mut packets);
            }
            let encoder = encoder.as_mut().unwrap();
            encoder.flush().unwrap();
            drain(encoder, &mut packet, &mut packets);
            assert_eq!(packets.len(), FRAMES);
            packets
        }

        fn transcode(
            input: VideoCodec,
            packets: Vec<Packet>,
            options: TranscodeOptions,
        ) -> (Transcoder, Vec<Packet>) {
            let mut transcoder = Transcoder::new(input, options).unwrap();
            let mut output = Vec::new();
            for packet in packets {
                transcoder.push(packet).unwrap();
                output.extend(std::iter::from_fn(|| transcoder.pull()));
            }
            transcoder.finish().unwrap();
            output.extend(std::iter::from_fn(|| transcoder.pull()));
            (transcoder, output)
        }

        /// The NAL unit types of an Annex B access unit; `header_bits` is how
        /// many bits of the first byte after the start code the type takes.
        fn nal_unit_types(data: &[u8], hevc: bool) -> Vec<u8> {
            let mut types = Vec::new();
            let mut i = 0;
            while i + 3 < data.len() {
                if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                    let header = data[i + 3];
                    types.push(if hevc {
                        (header & 0x7E) >> 1
                    } else {
                        header & 0x1F
                    });
                    i += 3;
                } else {
                    i += 1;
                }
            }
            types
        }

        fn assert_timed_in_order(packets: &[Packet]) {
            let mut last = f64::NEG_INFINITY;
            for packet in packets {
                let dts = packet.dts.or(packet.pts).expect("a timed packet");
                assert!(dts > last, "{dts} after {last}");
                last = dts;
            }
        }

        #[test]
        fn reports_the_version_of_the_ffmpeg_built_in() {
            assert!(ffmpeg_version().unwrap().starts_with("9."));
        }

        #[cfg(all(ffmpeg_mpeg2_encoder, ffmpeg_x264))]
        #[test]
        fn mpeg2_is_deinterlaced_into_h264_with_parameter_sets_at_keyframes() {
            let input = encode_stream("mpeg2video", "", true);
            assert!(
                input[0].data.starts_with(&[0x00, 0x00, 0x01, 0xB3]),
                "a sequence header"
            );

            let mut options = TranscodeOptions::new(VideoCodec::H264);
            options.deinterlace = Deinterlace::Frame;
            options.accelerations = vec![Acceleration::Software];
            let (transcoder, output) = transcode(VideoCodec::Mpeg2, input, options);

            assert_eq!(transcoder.acceleration(), Some(Acceleration::Software));
            assert_eq!(output.len(), FRAMES);
            assert!(output[0].keyframe);
            let types = nal_unit_types(&output[0].data, false);
            assert!(
                types.contains(&7) && types.contains(&8) && types.contains(&5),
                "{types:?}"
            );
            assert_timed_in_order(&output);
            assert_eq!(output[0].pts, Some(0.0));
        }

        #[cfg(all(ffmpeg_mpeg2_encoder, ffmpeg_x264))]
        #[test]
        fn deinterlacing_per_field_doubles_the_frame_rate() {
            let input = encode_stream("mpeg2video", "", true);

            let mut options = TranscodeOptions::new(VideoCodec::H264);
            options.deinterlace = Deinterlace::Field;
            options.accelerations = vec![Acceleration::Software];
            options.bitrate = Some(1_000_000);
            let (_, output) = transcode(VideoCodec::Mpeg2, input, options);

            // The deinterlacer has no field after the last to make its second frame from.
            assert!(output.len() >= FRAMES * 2 - 1, "{}", output.len());
            assert_timed_in_order(&output);
            // Encoders reorder pictures, so the presentation times are sorted
            // before the second is checked to be half a frame after the first.
            let mut pts: Vec<f64> = output.iter().map(|packet| packet.pts.unwrap()).collect();
            pts.sort_by(f64::total_cmp);
            let half_frame = DURATION as f64 / 2.0 / 90_000.0;
            assert!(
                (pts[1] - pts[0] - half_frame).abs() < 1.0 / 90_000.0,
                "{pts:?}"
            );
        }

        #[cfg(all(ffmpeg_mpeg2_encoder, ffmpeg_x265))]
        #[test]
        fn mpeg2_becomes_hevc_with_parameter_sets_at_keyframes() {
            let input = encode_stream("mpeg2video", "", false);
            let mut options = TranscodeOptions::new(VideoCodec::H265);
            options.accelerations = vec![Acceleration::Software];
            let (_, output) = transcode(VideoCodec::Mpeg2, input, options);

            assert_eq!(output.len(), FRAMES);
            let types = nal_unit_types(&output[0].data, true);
            assert!(
                types.contains(&32) && types.contains(&33) && types.contains(&34),
                "{types:?}"
            );
            assert_timed_in_order(&output);
        }

        #[cfg(all(ffmpeg_mpeg2_encoder, ffmpeg_svt_av1))]
        #[test]
        fn mpeg2_becomes_av1_temporal_units() {
            let input = encode_stream("mpeg2video", "", false);
            let mut options = TranscodeOptions::new(VideoCodec::Av1);
            options.accelerations = vec![Acceleration::Software];
            let (_, output) = transcode(VideoCodec::Mpeg2, input, options);

            assert_eq!(output.len(), FRAMES);
            assert!(output[0].keyframe);
            // A temporal delimiter OBU opens every temporal unit.
            assert_eq!(output[0].data[0] & 0x7A, 0x12, "{:#04x}", output[0].data[0]);
            assert_timed_in_order(&output);
        }

        #[cfg(all(ffmpeg_x265, ffmpeg_x264))]
        #[test]
        fn hevc_becomes_h264() {
            let input = encode_stream("libx265", "preset=ultrafast", false);
            assert!(nal_unit_types(&input[0].data, true).contains(&32), "a VPS");

            let mut options = TranscodeOptions::new(VideoCodec::H264);
            options.accelerations = vec![Acceleration::Software];
            let (_, output) = transcode(VideoCodec::H265, input, options);

            assert_eq!(output.len(), FRAMES);
            assert!(
                nal_unit_types(&output[0].data, false).contains(&7),
                "an SPS"
            );
            assert_timed_in_order(&output);
        }

        #[cfg(all(ffmpeg_x265, ffmpeg_x264))]
        #[test]
        fn hevc_is_re_encoded_when_passthrough_is_off() {
            let input = encode_stream("libx265", "preset=ultrafast", false);
            let mut options = TranscodeOptions::new(VideoCodec::H265);
            options.accelerations = vec![Acceleration::Software];
            options.passthrough = false;
            let (transcoder, output) = transcode(VideoCodec::H265, input, options);

            assert_eq!(transcoder.acceleration(), Some(Acceleration::Software));
            assert_eq!(output.len(), FRAMES);
        }

        #[cfg(ffmpeg_x264)]
        #[test]
        fn falls_back_past_hardware_this_machine_does_not_have() {
            // Hardware that is not compiled in is skipped; hardware that is
            // compiled in but absent is tried and passed over.
            let mut options = TranscodeOptions::new(VideoCodec::H264);
            options.accelerations = Acceleration::ALL.to_vec();
            let transcoder = Transcoder::new(VideoCodec::Mpeg2, options).unwrap();
            assert!(transcoder.acceleration().unwrap().compiled());
        }

        #[test]
        fn an_input_nobody_can_decode_names_every_attempt() {
            let mut options = TranscodeOptions::new(VideoCodec::H264);
            options.passthrough = false;
            let error = Transcoder::new(VideoCodec::H264, options).err().unwrap();
            match error {
                Error::Unsupported { attempts, .. } => {
                    assert_eq!(attempts.len(), Acceleration::available().len());
                }
                other => panic!("{other}"),
            }
        }
    }
}
