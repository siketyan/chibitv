//! MPEG-2 video access units in, MSE-ready fMP4 fragments out.
//!
//! This is the piece between Mediabunny's demuxer and the `SourceBuffer`: the
//! worker hands it one MPEG-2 picture at a time together with the AAC access
//! units of the same stream, and takes back fragments converted to H.264 by
//! mpeg2toh264's bitstream-domain transcoder. GOP splitting, conversion, and
//! putting the two tracks on one timeline all happen here.
//!
//! The timeline arithmetic follows mpeg2toh264's own `Session`, which does the
//! same job downstream of its MPEG-TS demuxer. This bridge exists because the
//! input here is not a transport stream: chibitv's server already demuxed,
//! descrambled and remuxed the broadcast, so the elementary stream is fed in
//! directly and the container never crosses into the transcoder.

use std::collections::VecDeque;

use mpeg2toh264::container::adts::AacConfig;
use mpeg2toh264::container::fmp4::{mpeg2_fragment_duration, Fmp4AudioSamples};
use mpeg2toh264::mpeg2::gop_stream::{Mpeg2Gop, Mpeg2GopStream};
use mpeg2toh264::{
    h264_gop_to_fmp4, mpeg2_video_timeline, IncrementalTranscoder, Mpeg2VideoTimeline,
    TranscodeOptions, UnitLeadIn,
};
use wasm_bindgen::prelude::*;

const TIMESCALE: u64 = 90_000;

/// PES presentation timestamps are 33 bits of 90 kHz ticks.
const PTS_MODULUS: i64 = 1 << 33;

/// Hold a picture over a hole in the source no longer than this. A larger jump
/// is left alone and the presentation resnaps early by the missing stretch.
const MAX_HELD_TICKS: i64 = 5 * 90_000;

/// ISO/IEC 14496-3 sampling frequency index table.
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Ticks from `origin` to `pts`, across however many 33-bit wraps lie between.
fn ticks_since(origin: i64, pts: i64) -> u64 {
    (pts - origin).rem_euclid(PTS_MODULUS) as u64
}

/// Presentation time of one coded picture, replicating
/// `Mpeg2VideoTimeline::presentation_time_at`, which is `pub(crate)` upstream.
fn presentation_time_at(timeline: &Mpeg2VideoTimeline, decode_index: usize) -> u64 {
    timeline
        .presentation_times
        .get(decode_index)
        .copied()
        .unwrap_or_else(|| {
            u64::from(
                timeline
                    .presentation_indices
                    .get(decode_index)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1),
            ) * u64::from(timeline.sample_duration)
        })
}

/// Presentation time of the first picture in coding order. A GOP's PES
/// timestamp belongs here, even when leading B pictures display earlier.
fn first_coded_presentation_time(timeline: &Mpeg2VideoTimeline) -> u64 {
    presentation_time_at(timeline, 0)
}

struct AudioFrame {
    /// 90 kHz PES presentation timestamp, in the source's own domain.
    pts: u64,
    data: Vec<u8>,
}

/// One thing to hand to Media Source Extensions.
#[wasm_bindgen]
pub struct Fragment {
    init: Option<Vec<u8>>,
    mime: Option<String>,
    media: Vec<u8>,
    start: f64,
}

#[wasm_bindgen]
impl Fragment {
    /// The initialization segment, present on the fragment that describes the
    /// stream; append it ahead of this fragment's media.
    #[wasm_bindgen(getter, js_name = initSegment)]
    pub fn init_segment(&self) -> Option<Vec<u8>> {
        self.init.clone()
    }

    /// The MIME type to open the `SourceBuffer` with, present alongside the
    /// initialization segment.
    #[wasm_bindgen(getter, js_name = mimeCodec)]
    pub fn mime_codec(&self) -> Option<String> {
        self.mime.clone()
    }

    #[wasm_bindgen(getter, js_name = mediaSegment)]
    pub fn media_segment(&self) -> Vec<u8> {
        self.media.clone()
    }

    /// Where this fragment starts on the presentation timeline, in seconds.
    #[wasm_bindgen(getter, js_name = startSeconds)]
    pub fn start_seconds(&self) -> f64 {
        self.start
    }
}

#[wasm_bindgen]
pub struct Transcoder {
    gops: Mpeg2GopStream,
    video: IncrementalTranscoder,
    recovery_interval: u64,
    audio_config: Option<AacConfig>,
    audio_frames: VecDeque<AudioFrame>,
    /// The source PTS the presentation's zero maps to, fixed by the first
    /// fragment. Video and audio do not start on the same PTS, so both tracks
    /// are placed at their real distance from this instead of each at zero.
    origin: Option<i64>,
    /// Where the next fragment starts, in ticks since `origin`.
    video_presentation_start: u64,
    /// Where the next unit is expected to open, read from the previous unit's
    /// own timestamp so the presentation and the source cannot creep apart.
    expected_pts: Option<i64>,
    sequence_number: u32,
    gops_emitted: u64,
    units_skipped: u64,
}

#[wasm_bindgen]
impl Transcoder {
    #[wasm_bindgen(constructor)]
    pub fn new(oversample: f64, recovery_interval: u32) -> Transcoder {
        let options = TranscodeOptions {
            oversample,
            ..TranscodeOptions::default()
        };
        Transcoder {
            gops: Mpeg2GopStream::new(),
            video: IncrementalTranscoder::new(options),
            recovery_interval: u64::from(recovery_interval),
            audio_config: None,
            audio_frames: VecDeque::new(),
            origin: None,
            video_presentation_start: 0,
            expected_pts: None,
            sequence_number: 0,
            gops_emitted: 0,
            units_skipped: 0,
        }
    }

    /// Declare the AAC track by its AudioSpecificConfig, before any audio is
    /// pushed. A stream with no audio track never calls this.
    #[wasm_bindgen(js_name = setAudioConfig)]
    pub fn set_audio_config(&mut self, audio_specific_config: &[u8]) -> Result<(), JsError> {
        self.audio_config = Some(parse_audio_specific_config(audio_specific_config)?);
        Ok(())
    }

    /// Feed one MPEG-2 video access unit -- a picture with whatever sequence
    /// and GOP headers precede it -- with its 90 kHz presentation timestamp,
    /// in decode order.
    #[wasm_bindgen(js_name = pushVideo)]
    pub fn push_video(&mut self, access_unit: &[u8], pts: f64) -> Result<Vec<Fragment>, JsError> {
        let pts = (pts as i64).rem_euclid(PTS_MODULUS) as u64;
        let gops = self.gops.push(access_unit, Some(pts));
        let mut out = Vec::new();
        for gop in gops {
            self.process_gop(gop, &mut out)?;
        }
        Ok(out)
    }

    /// Feed one AAC access unit (raw, without ADTS framing) with its 90 kHz
    /// presentation timestamp.
    #[wasm_bindgen(js_name = pushAudio)]
    pub fn push_audio(&mut self, access_unit: &[u8], pts: f64) {
        if self.audio_config.is_none() {
            return;
        }
        self.audio_frames.push_back(AudioFrame {
            pts: (pts as i64).rem_euclid(PTS_MODULUS) as u64,
            data: access_unit.to_vec(),
        });
    }

    /// Flush the unit still being collected when the stream ends.
    pub fn finish(&mut self) -> Result<Vec<Fragment>, JsError> {
        let gops = self.gops.finish();
        let mut out = Vec::new();
        for gop in gops {
            self.process_gop(gop, &mut out)?;
        }
        Ok(out)
    }

    /// Units dropped because they would not parse or convert. The decoder is
    /// restarted after each, so playback continues at the next random access
    /// point.
    #[wasm_bindgen(getter, js_name = unitsSkipped)]
    pub fn units_skipped(&self) -> f64 {
        self.units_skipped as f64
    }
}

impl Transcoder {
    fn process_gop(&mut self, gop: Mpeg2Gop, out: &mut Vec<Fragment>) -> Result<(), JsError> {
        // Periodic restart points, so MSE can evict buffered media and a
        // decoder joining mid-stream has somewhere to begin.
        if self.recovery_interval > 0
            && self.gops_emitted > 0
            && self.gops_emitted % self.recovery_interval == 0
        {
            self.video.request_recovery_point();
        }
        let mut starts_at_idr = self.video.awaiting_random_access();

        // Hole detection needs the unit's display geometry before conversion,
        // so draw a preliminary timeline from the headers alone.
        let Ok(prelim) = mpeg2_video_timeline(&gop.data, !starts_at_idr, &[]) else {
            self.skip_unit();
            return Ok(());
        };
        let mut hold = 0u32;
        if let (Some(expected), Some(pts)) = (self.expected_pts, gop.pts) {
            let start = pts as i64 - first_coded_presentation_time(&prelim) as i64;
            let mut ahead = (start - expected).rem_euclid(PTS_MODULUS);
            if ahead > PTS_MODULUS / 2 {
                ahead -= PTS_MODULUS;
            }
            if ahead >= i64::from(prelim.sample_duration) && ahead <= MAX_HELD_TICKS {
                // Hold this unit's opening sample over what the source lost in
                // front of it. Only a unit that opens a random access point
                // has a sample to hold with: the extra copy of its IDR.
                if !starts_at_idr {
                    self.video.request_random_access_point();
                    starts_at_idr = self.video.awaiting_random_access();
                }
                hold = ahead as u32;
            }
        }

        let Ok(converted) = self.video.push(&gop.data) else {
            self.skip_unit();
            return Ok(());
        };
        let Ok(mut timeline) =
            mpeg2_video_timeline(&gop.data, !starts_at_idr, &converted.undecodable)
        else {
            self.skip_unit();
            return Ok(());
        };
        timeline.hold_ticks = hold;

        self.align(&gop, &timeline);

        // The audio that belongs in this fragment is whatever falls before its
        // end. The estimate only decides which side of the boundary a frame
        // lands on; audio decode times are taken from each batch's own first
        // timestamp, so an estimate a frame out corrects itself on the next.
        let lead_in = if starts_at_idr {
            UnitLeadIn::IdrClone
        } else {
            UnitLeadIn::None
        };
        let duration_estimate = mpeg2_fragment_duration(&timeline, lead_in);
        let audio = self.drain_audio(self.video_presentation_start + duration_estimate);
        let audio_samples = audio.as_ref().map_or(0, |track| track.samples.len());

        let fragment = match h264_gop_to_fmp4(
            &converted.bitstream,
            &timeline,
            self.sequence_number,
            self.video_presentation_start,
            self.audio_config.as_ref(),
            audio.as_ref(),
        ) {
            Ok(fragment) => fragment,
            Err(_) => {
                self.skip_unit();
                return Ok(());
            }
        };

        // A unit that yielded no picture and carries no audio makes a moof
        // describing no samples, which there is nothing for a SourceBuffer in.
        if fragment.sample_count == 0 && audio_samples == 0 {
            return Ok(());
        }

        self.sequence_number += 1;
        self.gops_emitted += 1;
        let start_seconds = self.video_presentation_start as f64 / TIMESCALE as f64;
        self.video_presentation_start += fragment.duration;
        if let Some(pts) = gop.pts {
            let start = pts as i64 - first_coded_presentation_time(&timeline) as i64;
            // The hold covered the hole in front of this unit and is not part
            // of what it spans.
            self.expected_pts =
                Some(start + fragment.duration.saturating_sub(u64::from(hold)) as i64);
        }

        out.push(Fragment {
            init: (!fragment.init_segment.is_empty()).then_some(fragment.init_segment),
            mime: (!fragment.mime_codec.is_empty()).then_some(fragment.mime_codec),
            media: fragment.media_segment,
            start: start_seconds,
        });
        Ok(())
    }

    /// Drop a unit that would not parse or convert, and restart the decode
    /// chain: whatever state the unit would have carried forward is gone, so
    /// the next unit has to open a random access point of its own.
    fn skip_unit(&mut self) {
        self.units_skipped += 1;
        self.video.request_random_access_point();
    }

    /// Fix the presentation's origin from the first unit. Both tracks are
    /// placed at their real distance from it, which keeps the few hundred
    /// milliseconds a broadcast starts its audio away from its video.
    fn align(&mut self, gop: &Mpeg2Gop, timeline: &Mpeg2VideoTimeline) {
        if self.origin.is_some() {
            return;
        }
        let Some(pts) = gop.pts else {
            return;
        };
        let video_start = pts as i64 - first_coded_presentation_time(timeline) as i64;
        // Decoding leads display by up to one frame, and the muxer needs
        // somewhere to put that, so the timeline starts a frame early.
        let video_origin = video_start - i64::from(timeline.sample_duration);
        let origin = match self.audio_frames.front() {
            Some(first) => video_origin.min(first.pts as i64),
            None => video_origin,
        };
        self.origin = Some(origin);
        self.video_presentation_start = ticks_since(origin, video_start);
    }

    /// Take the queued AAC access units that display before `end_ticks`, timed
    /// from their own first timestamp rather than accumulated, so the batches
    /// cannot creep away from the source.
    fn drain_audio(&mut self, end_ticks: u64) -> Option<Fmp4AudioSamples> {
        let config = self.audio_config.clone()?;
        let origin = self.origin?;
        let mut samples: Vec<Vec<u8>> = Vec::new();
        let mut first_ticks: Option<u64> = None;
        while let Some(front) = self.audio_frames.front() {
            let ticks = ticks_since(origin, front.pts as i64);
            // Half the modulus away is a frame from before the origin, which
            // has no place on the timeline.
            if ticks > (PTS_MODULUS / 2) as u64 {
                self.audio_frames.pop_front();
                continue;
            }
            if ticks >= end_ticks {
                break;
            }
            first_ticks.get_or_insert(ticks);
            let frame = self.audio_frames.pop_front().expect("front was Some");
            samples.push(frame.data);
        }
        let first = first_ticks?;
        let base_decode_time =
            (first as f64 * f64::from(config.sample_rate) / TIMESCALE as f64 + 0.5).floor() as u64;
        Some(Fmp4AudioSamples {
            config,
            samples,
            base_decode_time,
        })
    }
}

fn parse_audio_specific_config(asc: &[u8]) -> Result<AacConfig, JsError> {
    let [first, second, ..] = *asc else {
        return Err(JsError::new("AudioSpecificConfig is too short"));
    };
    let audio_object_type = first >> 3;
    if audio_object_type == 31 {
        return Err(JsError::new(
            "escaped AudioSpecificConfig object types are not supported",
        ));
    }
    let sampling_frequency_index = ((first & 0x07) << 1) | (second >> 7);
    let Some(&sample_rate) = SAMPLE_RATES.get(usize::from(sampling_frequency_index)) else {
        return Err(JsError::new(
            "explicit AudioSpecificConfig sampling frequencies are not supported",
        ));
    };
    let channel_count = (second >> 3) & 0x0f;
    Ok(AacConfig {
        audio_object_type,
        sample_rate,
        sampling_frequency_index,
        channel_count,
        audio_specific_config: asc.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_two_byte_audio_specific_config() {
        // AAC-LC, 48 kHz, stereo: 0b00010_0011_0010_000.
        let config = parse_audio_specific_config(&[0x11, 0x90]).unwrap();
        assert_eq!(config.audio_object_type, 2);
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.sampling_frequency_index, 3);
        assert_eq!(config.channel_count, 2);
    }

    #[test]
    fn ticks_since_crosses_the_wrap() {
        let origin = PTS_MODULUS - 90_000;
        assert_eq!(ticks_since(origin, 90_000), 180_000);
    }
}
