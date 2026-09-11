//! What each acceleration does a transcode with: the names FFmpeg knows its
//! decoders, encoders, devices and filters by, and the options they take.
//! Nothing here calls FFmpeg, so the choices can be tested on their own.

// Without a backend only the tests use this module.
#![cfg_attr(not(ffmpeg), allow(dead_code))]

use crate::{Acceleration, Deinterlace, VideoCodec};

/// What a decoded picture is like, as far as the choice of filters cares.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PictureKind {
    /// Bits per sample.
    pub bit_depth: u32,
    /// The picture lives in device memory.
    pub hardware: bool,
}

/// The hardware device type an acceleration works on, or `None` when it
/// exchanges pictures in system memory.
pub fn device_type(acceleration: Acceleration) -> Option<&'static str> {
    match acceleration {
        Acceleration::VideoToolbox => Some("videotoolbox"),
        Acceleration::Nvidia => Some("cuda"),
        Acceleration::Qsv => Some("qsv"),
        Acceleration::Vaapi => Some("vaapi"),
        Acceleration::Amf | Acceleration::Software => None,
    }
}

/// Whether the decoder needs the device to output pictures on it. AMF encodes
/// from system memory, so its decode is a plain software one.
pub fn decodes_on_device(acceleration: Acceleration) -> bool {
    device_type(acceleration).is_some()
}

pub fn decoder(acceleration: Acceleration, input: VideoCodec) -> Option<&'static str> {
    Some(match (acceleration, input) {
        (Acceleration::Nvidia, VideoCodec::Mpeg2) => "mpeg2_cuvid",
        (Acceleration::Nvidia, VideoCodec::H265) => "hevc_cuvid",
        (Acceleration::Qsv, VideoCodec::Mpeg2) => "mpeg2_qsv",
        (Acceleration::Qsv, VideoCodec::H265) => "hevc_qsv",
        (_, VideoCodec::Mpeg2) => "mpeg2video",
        (_, VideoCodec::H265) => "hevc",
        (_, VideoCodec::H264 | VideoCodec::Av1) => return None,
    })
}

/// The options the decoder takes. The CUVID decoders deinterlace themselves.
pub fn decoder_options(acceleration: Acceleration, deinterlace: Deinterlace) -> String {
    match (acceleration, deinterlace) {
        (Acceleration::Nvidia, Deinterlace::Frame) => "deint=adaptive:drop_second_field=1".into(),
        (Acceleration::Nvidia, Deinterlace::Field) => "deint=adaptive".into(),
        _ => String::new(),
    }
}

pub fn encoder(acceleration: Acceleration, output: VideoCodec) -> Option<&'static str> {
    Some(match (acceleration, output) {
        (Acceleration::VideoToolbox, VideoCodec::H264) => "h264_videotoolbox",
        (Acceleration::VideoToolbox, VideoCodec::H265) => "hevc_videotoolbox",
        (Acceleration::Nvidia, VideoCodec::H264) => "h264_nvenc",
        (Acceleration::Nvidia, VideoCodec::H265) => "hevc_nvenc",
        (Acceleration::Nvidia, VideoCodec::Av1) => "av1_nvenc",
        (Acceleration::Qsv, VideoCodec::H264) => "h264_qsv",
        (Acceleration::Qsv, VideoCodec::H265) => "hevc_qsv",
        (Acceleration::Qsv, VideoCodec::Av1) => "av1_qsv",
        (Acceleration::Vaapi, VideoCodec::H264) => "h264_vaapi",
        (Acceleration::Vaapi, VideoCodec::H265) => "hevc_vaapi",
        (Acceleration::Vaapi, VideoCodec::Av1) => "av1_vaapi",
        (Acceleration::Amf, VideoCodec::H264) => "h264_amf",
        (Acceleration::Amf, VideoCodec::H265) => "hevc_amf",
        (Acceleration::Amf, VideoCodec::Av1) => "av1_amf",
        (Acceleration::Software, VideoCodec::H264) => "libx264",
        (Acceleration::Software, VideoCodec::H265) => "libx265",
        (Acceleration::Software, VideoCodec::Av1) => "libsvtav1",
        (Acceleration::Software, VideoCodec::Mpeg2) => "mpeg2video",
        (_, VideoCodec::Mpeg2) => return None,
        (Acceleration::VideoToolbox, VideoCodec::Av1) => return None,
    })
}

/// The options the encoder takes, tuned for keeping up with a live stream.
/// The bit rate goes through the generic setting instead, so it is not here.
pub fn encoder_options(
    acceleration: Acceleration,
    output: VideoCodec,
    bitrate: Option<u32>,
) -> String {
    let mut options: Vec<&str> = Vec::new();
    match (acceleration, output) {
        (Acceleration::Software, VideoCodec::H264 | VideoCodec::H265) => {
            options.push("preset=veryfast")
        }
        (Acceleration::Software, VideoCodec::Av1) => options.push("preset=10"),
        (Acceleration::Nvidia, _) => options.push("preset=p4"),
        (Acceleration::Qsv, _) => {
            options.push("preset=veryfast");
            if bitrate.is_none() {
                // Constant quality, which the encoder needs told when there
                // is no bit rate.
                options.push("global_quality=25");
            }
        }
        _ => {}
    }
    options.join(":")
}

/// The filter graph between the decoder and the encoder, in FFmpeg's syntax,
/// or `None` when the pictures can go straight through.
pub fn filter_graph(
    acceleration: Acceleration,
    deinterlace: Deinterlace,
    output: VideoCodec,
    picture: PictureKind,
) -> Option<String> {
    let mut filters: Vec<String> = Vec::new();

    // H.264 is kept 8-bit, which is what decoders in browsers and on phones
    // take. The others keep the depth of the input.
    let narrow = output == VideoCodec::H264 && picture.bit_depth > 8;

    match acceleration {
        Acceleration::Software | Acceleration::Amf => {
            if let Some(mode) = bwdif_mode(deinterlace) {
                filters.push(format!("bwdif=mode={mode}:parity=auto:deint=interlaced"));
            }
            // What the encoders take; the format filter keeps the input's
            // when it is listed.
            let formats = match (acceleration, output) {
                (_, VideoCodec::H264) => "yuv420p",
                (Acceleration::Amf, _) => "nv12|p010le",
                (_, VideoCodec::Mpeg2) => "yuv420p",
                _ => "yuv420p|yuv420p10le",
            };
            filters.push(format!("format={formats}"));
        }
        Acceleration::Nvidia => {
            // Deinterlacing happened in the decoder. Narrowing has no CUDA
            // filter in this build, so it takes a trip through system memory.
            if narrow {
                filters.push("hwdownload".into());
                filters.push("format=nv12".into());
                filters.push("hwupload".into());
            }
        }
        Acceleration::Vaapi => {
            if deinterlace != Deinterlace::Off {
                let rate = if deinterlace == Deinterlace::Field {
                    "field"
                } else {
                    "frame"
                };
                filters.push(format!("deinterlace_vaapi=mode=default:rate={rate}"));
            }
            if narrow {
                filters.push("scale_vaapi=format=nv12".into());
            }
        }
        Acceleration::Qsv => {
            if deinterlace != Deinterlace::Off {
                filters.push("deinterlace_qsv=mode=advanced".into());
            }
            if narrow {
                filters.push("scale_qsv=format=nv12".into());
            }
        }
        Acceleration::VideoToolbox => {
            if let Some(mode) = bwdif_mode(deinterlace) {
                filters.push(format!(
                    "yadif_videotoolbox=mode={mode}:parity=auto:deint=interlaced"
                ));
            }
            if narrow {
                filters.push("scale_vt=format=nv12".into());
            }
        }
    }

    (!filters.is_empty()).then(|| filters.join(","))
}

fn bwdif_mode(deinterlace: Deinterlace) -> Option<&'static str> {
    match deinterlace {
        Deinterlace::Off => None,
        Deinterlace::Frame => Some("send_frame"),
        Deinterlace::Field => Some("send_field"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIGHT_BIT: PictureKind = PictureKind {
        bit_depth: 8,
        hardware: false,
    };
    const TEN_BIT_ON_DEVICE: PictureKind = PictureKind {
        bit_depth: 10,
        hardware: true,
    };

    #[test]
    fn software_deinterlaces_and_narrows_to_what_the_encoder_takes() {
        assert_eq!(
            filter_graph(
                Acceleration::Software,
                Deinterlace::Frame,
                VideoCodec::H264,
                EIGHT_BIT
            )
            .as_deref(),
            Some("bwdif=mode=send_frame:parity=auto:deint=interlaced,format=yuv420p")
        );
        assert_eq!(
            filter_graph(
                Acceleration::Software,
                Deinterlace::Field,
                VideoCodec::H265,
                EIGHT_BIT
            )
            .as_deref(),
            Some("bwdif=mode=send_field:parity=auto:deint=interlaced,format=yuv420p|yuv420p10le")
        );
        assert_eq!(
            filter_graph(
                Acceleration::Software,
                Deinterlace::Off,
                VideoCodec::Av1,
                EIGHT_BIT
            )
            .as_deref(),
            Some("format=yuv420p|yuv420p10le")
        );
    }

    #[test]
    fn hardware_pictures_pass_straight_through_when_nothing_is_to_be_done() {
        assert_eq!(
            filter_graph(
                Acceleration::Nvidia,
                Deinterlace::Field,
                VideoCodec::H265,
                TEN_BIT_ON_DEVICE
            ),
            None
        );
        assert_eq!(
            filter_graph(
                Acceleration::Vaapi,
                Deinterlace::Off,
                VideoCodec::H265,
                TEN_BIT_ON_DEVICE
            ),
            None
        );
        assert_eq!(
            filter_graph(
                Acceleration::VideoToolbox,
                Deinterlace::Off,
                VideoCodec::H265,
                TEN_BIT_ON_DEVICE
            ),
            None
        );
    }

    #[test]
    fn ten_bit_input_is_narrowed_for_h264_on_the_device() {
        assert_eq!(
            filter_graph(
                Acceleration::Nvidia,
                Deinterlace::Frame,
                VideoCodec::H264,
                TEN_BIT_ON_DEVICE
            )
            .as_deref(),
            Some("hwdownload,format=nv12,hwupload")
        );
        assert_eq!(
            filter_graph(
                Acceleration::Vaapi,
                Deinterlace::Field,
                VideoCodec::H264,
                TEN_BIT_ON_DEVICE
            )
            .as_deref(),
            Some("deinterlace_vaapi=mode=default:rate=field,scale_vaapi=format=nv12")
        );
        assert_eq!(
            filter_graph(
                Acceleration::VideoToolbox,
                Deinterlace::Frame,
                VideoCodec::H264,
                TEN_BIT_ON_DEVICE
            )
            .as_deref(),
            Some(
                "yadif_videotoolbox=mode=send_frame:parity=auto:deint=interlaced,scale_vt=format=nv12"
            )
        );
    }

    #[test]
    fn the_cuvid_decoders_deinterlace_themselves() {
        assert_eq!(
            decoder(Acceleration::Nvidia, VideoCodec::Mpeg2),
            Some("mpeg2_cuvid")
        );
        assert_eq!(
            decoder_options(Acceleration::Nvidia, Deinterlace::Frame),
            "deint=adaptive:drop_second_field=1"
        );
        assert_eq!(
            decoder_options(Acceleration::Nvidia, Deinterlace::Field),
            "deint=adaptive"
        );
        assert_eq!(decoder_options(Acceleration::Nvidia, Deinterlace::Off), "");
        assert_eq!(
            decoder_options(Acceleration::Software, Deinterlace::Field),
            ""
        );
    }

    #[test]
    fn only_decodable_inputs_and_encodable_outputs_have_names() {
        assert_eq!(decoder(Acceleration::Software, VideoCodec::H264), None);
        assert_eq!(encoder(Acceleration::VideoToolbox, VideoCodec::Av1), None);
        assert_eq!(encoder(Acceleration::Vaapi, VideoCodec::Mpeg2), None);
        assert_eq!(
            encoder(Acceleration::Software, VideoCodec::H264),
            Some("libx264")
        );
    }

    #[test]
    fn quick_sync_is_told_a_quality_when_there_is_no_bit_rate() {
        assert_eq!(
            encoder_options(Acceleration::Qsv, VideoCodec::H264, None),
            "preset=veryfast:global_quality=25"
        );
        assert_eq!(
            encoder_options(Acceleration::Qsv, VideoCodec::H264, Some(4_000_000)),
            "preset=veryfast"
        );
        assert_eq!(
            encoder_options(Acceleration::Vaapi, VideoCodec::H264, None),
            ""
        );
    }
}
