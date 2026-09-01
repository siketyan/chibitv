use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::slice;

use bytes::{BufMut, Bytes, BytesMut};
use shiguredo_mp4::bitstream::aac::{
    AudioObjectType, AudioSpecificConfig, ChannelConfiguration, Mp4aSampleEntryConfig,
    SamplingFrequency, build_mp4a_box, parse_adts_frame,
};
use shiguredo_mp4::bitstream::h265::{
    H265ConstantFrameRate, H265NalUnitType, H265SampleEntryConfig, LengthSize, build_hev1_box,
    parse_annexb_nal_units, parse_sps,
};
use shiguredo_mp4::boxes::{EsdsBox, Mp4vBox, SampleEntry, VisualSampleEntryFields};
use shiguredo_mp4::descriptors::{
    DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor,
};
use shiguredo_mp4::mux::{Fmp4SegmentMuxer, Mp4FileMuxer, Sample};
use shiguredo_mp4::{TrackKind, Uint};
use tracing::{debug, info, warn};

use crate::aac::{AdtsParser, LoasFrame};
use crate::demux::TrackType;
use crate::mp2::{Mp2Parser, PictureCodingType, SequenceHeader, picture_coding_type};
use crate::remux::Mux;

const VIDEO_TIMESCALE: u32 = 90_000;

/// The number of samples in one AAC raw data block.
///
/// `parse_adts_frame` only accepts frames holding a single raw data block.
const AAC_SAMPLES_PER_FRAME: u32 = 1024;

/// The `audioObjectType` of AAC-LC.
const AAC_OBJECT_TYPE_LC: u8 = 2;

/// The `audioObjectType` of SBR (HE-AAC).
const AAC_OBJECT_TYPE_SBR: u8 = 5;

/// The frame rate assumed when an H.265 SPS carries no VUI timing information.
///
/// Only the very first sample keeps this duration: every later sample has its
/// duration recomputed from the DTS delta of the following sample.
const DEFAULT_H265_FRAME_RATE: u32 = 30;

/// NAL unit types that start a random access point (ITU-T H.265 Table 7-1).
///
/// IDR_W_RADL (19) / IDR_N_LP (20) / CRA_NUT (21).
const H265_RANDOM_ACCESS_NAL_UNIT_TYPES: [u8; 3] = [19, 20, 21];

#[derive(Clone, Debug)]
struct TrackMetadata {
    sample_duration: u32,
    timescale: u32,
}

struct PendingSample {
    sample: Sample,
    data: Bytes,
    dts: f64,
}

struct TrackSample {
    sample: Sample,
    data: Bytes,
    dts: Option<f64>,
}

struct MediaFragment {
    metadata: Bytes,
    payload: Bytes,
}

#[derive(Default)]
struct FragmentedTrackState {
    first_dts: Option<f64>,
    sample_entry: Option<SampleEntry>,
    ready: bool,
}

impl FragmentedTrackState {
    fn observe_samples(&mut self, samples: &[TrackSample]) {
        for sample in samples {
            if self.first_dts.is_none() {
                self.first_dts = sample.dts;
            }

            if let Some(sample_entry) = &sample.sample.sample_entry {
                self.sample_entry = Some(sample_entry.clone());
            }
        }
    }

    fn attach_sample_entry_if_needed(&self, sample: &mut Sample) {
        if sample.sample_entry.is_none()
            && !self.ready
            && let Some(sample_entry) = &self.sample_entry
        {
            sample.sample_entry = Some(sample_entry.clone());
        }
    }

    fn observe_fragment_samples(&mut self, samples: &[Sample]) {
        for sample in samples {
            if sample.sample_entry.is_some() {
                self.ready = true;
            }
        }
    }
}

/// A per-track sample writer.
///
/// The muxers own their tracks and are moved into a remuxer thread, so every
/// implementation must be `Send`.
trait Track: Send {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>>;

    fn finalize(&mut self) -> anyhow::Result<Vec<TrackSample>> {
        Ok(vec![])
    }
}

struct Mpeg2VideoTrack {
    parser: Mp2Parser,
    metadata: Option<TrackMetadata>,
    timestamps: VecDeque<(Option<f64>, Option<f64>)>,
    next_dts: Option<f64>,
    next_pts: Option<f64>,
}

impl Mpeg2VideoTrack {
    fn new() -> Self {
        Self {
            parser: Mp2Parser::default(),
            metadata: None,
            timestamps: VecDeque::new(),
            next_dts: None,
            next_pts: None,
        }
    }

    fn write_frame(&mut self, data: Bytes) -> anyhow::Result<Vec<TrackSample>> {
        let (dts, pts) = self
            .timestamps
            .pop_front()
            .unwrap_or((self.next_dts, self.next_pts));
        // MPEG-TS may omit DTS when it is equal to PTS. Keep a continuous
        // decode timeline for fMP4's base media decode time in that case.
        let dts = dts.or(self.next_dts).or(pts);
        let mut sample_entry = None;

        if self.metadata.is_none() {
            let sequence = match SequenceHeader::parse(&data) {
                Ok(sequence) => sequence,
                Err(error) => {
                    debug!(%error, "Waiting for an MPEG-2 sequence header");
                    return Ok(vec![]);
                }
            };
            sample_entry = Some(build_mp4v_sample_entry(&sequence));
            self.metadata = Some(TrackMetadata {
                sample_duration: sequence.sample_duration(VIDEO_TIMESCALE),
                timescale: VIDEO_TIMESCALE,
            });
            info!(
                width = sequence.width,
                height = sequence.height,
                frame_rate_numerator = sequence.frame_rate_numerator,
                frame_rate_denominator = sequence.frame_rate_denominator,
                "MPEG-2 video track is ready"
            );
        }

        let metadata = self.metadata.as_ref().expect("metadata must be set");
        let duration_seconds = f64::from(metadata.sample_duration) / f64::from(metadata.timescale);
        self.next_dts = dts.map(|dts| dts + duration_seconds);
        self.next_pts = pts.map(|pts| pts + duration_seconds);

        let sample = Sample {
            track_kind: TrackKind::Video,
            sample_entry,
            keyframe: picture_coding_type(&data) == Some(PictureCodingType::Intra),
            timescale: NonZeroU32::new(metadata.timescale).unwrap(),
            duration: metadata.sample_duration,
            composition_time_offset: pts
                .zip(dts)
                .map(|(pts, dts)| seconds_to_timescale_units(pts - dts, metadata.timescale)),
            data_offset: 0,
            data_size: data.len(),
        };

        Ok(vec![TrackSample { sample, data, dts }])
    }
}

impl Track for Mpeg2VideoTrack {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>> {
        self.timestamps.push_back((dts, pts));
        let mut samples = Vec::new();
        let mut input = Some(data);

        while let Some(data) = self
            .parser
            .push(input.take().as_deref().unwrap_or_default())
        {
            samples.extend(self.write_frame(data)?);
        }

        Ok(samples)
    }

    fn finalize(&mut self) -> anyhow::Result<Vec<TrackSample>> {
        let mut samples = Vec::new();
        if let Some(data) = self.parser.flush() {
            samples.extend(self.write_frame(data)?);
        }
        Ok(samples)
    }
}

struct H265Track {
    /// The parameter set NAL units, kept as EBSP for the `hvcC` box.
    vps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    metadata: Option<TrackMetadata>,
    pending: Option<PendingSample>,
}

impl H265Track {
    fn new() -> Self {
        Self {
            vps: None,
            pps: None,
            sps: None,
            metadata: None,
            pending: None,
        }
    }

    /// Builds the `hev1` sample entry once all three parameter sets are known.
    fn build_sample_entry(&self) -> anyhow::Result<(SampleEntry, TrackMetadata)> {
        let (Some(vps), Some(sps), Some(pps)) = (&self.vps, &self.sps, &self.pps) else {
            anyhow::bail!("VPS, SPS and PPS are all required to build a sample entry");
        };

        let parsed = parse_sps(sps)?;
        let (sample_duration, avg_frame_rate) = match parsed.vui_timing_info {
            Some(timing) => {
                let num_units_in_tick = u64::from(timing.num_units_in_tick.get());
                let time_scale = u64::from(timing.time_scale.get());
                let sample_duration = u64::from(VIDEO_TIMESCALE) * num_units_in_tick / time_scale;
                // hvcC の avgFrameRate は 256 秒あたりのフレーム数
                let avg_frame_rate = 256 * time_scale / num_units_in_tick;
                (
                    u32::try_from(sample_duration).unwrap_or(u32::MAX),
                    u16::try_from(avg_frame_rate)
                        .unwrap_or(H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED),
                )
            }
            None => {
                warn!("H265 SPS has no VUI timing info; assuming {DEFAULT_H265_FRAME_RATE} fps");
                (
                    VIDEO_TIMESCALE / DEFAULT_H265_FRAME_RATE,
                    H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
                )
            }
        };

        let mut hev1 = build_hev1_box(
            slice::from_ref(vps),
            slice::from_ref(sps),
            slice::from_ref(pps),
            &H265SampleEntryConfig {
                length_size: LengthSize::FourBytes,
                avg_frame_rate,
                constant_frame_rate: H265ConstantFrameRate::Unknown,
            },
        )?;
        hev1.visual.compressorname = compressor_name();

        Ok((
            SampleEntry::Hev1(hev1),
            TrackMetadata {
                sample_duration,
                timescale: VIDEO_TIMESCALE,
            },
        ))
    }
}

impl Track for H265Track {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>> {
        // A corrupted access unit must not tear down the whole remux: log it and
        // drop the sample instead.
        let nal_units = match parse_annexb_nal_units(&data) {
            Ok(nal_units) => nal_units,
            Err(err) => {
                warn!("Failed to parse an H265 access unit: {err}");
                return Ok(vec![]);
            }
        };

        let mut keyframe = false;
        let mut sample_entry = None::<SampleEntry>;
        let mut bytes = BytesMut::new();
        let collecting_parameter_sets = self.metadata.is_none();

        for nal_unit in &nal_units {
            match nal_unit.nal_unit_type {
                H265NalUnitType::Vps if collecting_parameter_sets && self.vps.is_none() => {
                    self.vps = Some(nal_unit.data.to_vec());
                    debug!("VPS NALU found");
                }
                H265NalUnitType::Pps if collecting_parameter_sets && self.pps.is_none() => {
                    self.pps = Some(nal_unit.data.to_vec());
                    debug!("PPS NALU found");
                }
                H265NalUnitType::Sps if collecting_parameter_sets && self.sps.is_none() => {
                    self.sps = Some(nal_unit.data.to_vec());
                    debug!("SPS NALU found");
                }
                H265NalUnitType::Other(nal_unit_type)
                    if H265_RANDOM_ACCESS_NAL_UNIT_TYPES.contains(&nal_unit_type) =>
                {
                    keyframe = true;
                }
                _ => {}
            }

            // Annex B の開始コードを 4 バイトの長さプレフィックスに置き換える
            bytes.put_u32(nal_unit.data.len() as u32);
            bytes.put(nal_unit.data);
        }

        if self.metadata.is_none() && self.vps.is_some() && self.sps.is_some() && self.pps.is_some()
        {
            let (entry, metadata) = self.build_sample_entry()?;
            sample_entry = Some(entry);
            self.metadata = Some(metadata);

            debug!("H265 track is ready: {:?}", &self.metadata);
        }

        let Some(metadata) = &self.metadata else {
            // Stream is not ready yet.
            return Ok(vec![]);
        };

        let sample = Sample {
            track_kind: TrackKind::Video,
            sample_entry,
            keyframe,
            timescale: NonZeroU32::new(metadata.timescale).unwrap(),
            duration: metadata.sample_duration,
            composition_time_offset: pts
                .zip(dts)
                .map(|(pts, dts)| seconds_to_timescale_units(pts - dts, metadata.timescale)),
            data_offset: 0,
            data_size: bytes.len(),
        };

        let data = bytes.freeze();
        let Some(dts) = dts else {
            return Ok(vec![TrackSample {
                sample,
                data,
                dts: None,
            }]);
        };

        let Some(mut pending) = self.pending.replace(PendingSample { sample, data, dts }) else {
            return Ok(vec![]);
        };

        let duration = seconds_to_timescale_units(dts - pending.dts, metadata.timescale);
        if duration > 0 {
            pending.sample.duration = duration as u32;
        }

        Ok(vec![TrackSample {
            sample: pending.sample,
            data: pending.data,
            dts: Some(pending.dts),
        }])
    }

    fn finalize(&mut self) -> anyhow::Result<Vec<TrackSample>> {
        Ok(self
            .pending
            .take()
            .map(|pending| TrackSample {
                sample: pending.sample,
                data: pending.data,
                dts: Some(pending.dts),
            })
            .into_iter()
            .collect())
    }
}

struct AacAdtsTrack {
    parser: AdtsParser,
    metadata: Option<TrackMetadata>,
    next_dts: Option<f64>,
}

impl AacAdtsTrack {
    fn new() -> Self {
        Self {
            parser: AdtsParser::default(),
            metadata: None,
            next_dts: None,
        }
    }
}

impl Track for AacAdtsTrack {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>> {
        if self.parser.is_empty() {
            // AAC has no frame reordering, so its PTS is also its decode
            // timestamp when an MPEG-TS PES omits DTS.
            self.next_dts = dts.or(pts);
        }

        let mut samples = Vec::new();
        let mut input = Some(data);
        while let Some(frame) = self
            .parser
            .push(input.take().as_deref().unwrap_or_default())
        {
            let (header, payload) = parse_adts_frame(&frame)?;
            let sampling_frequency = header.sampling_frequency();

            let mut sample_entry = None;
            if self.metadata.is_none() {
                info!(
                    audio_object_type = ?header.audio_object_type,
                    sampling_frequency = sampling_frequency.hz(),
                    channel_configuration = header.channel_configuration.as_u8(),
                    "AAC-ADTS track is ready"
                );
                sample_entry = Some(build_mp4a_sample_entry(
                    header.audio_object_type.as_u8(),
                    sampling_frequency,
                    header.channel_configuration,
                )?);
                self.metadata = Some(TrackMetadata {
                    sample_duration: AAC_SAMPLES_PER_FRAME,
                    timescale: sampling_frequency.hz(),
                });
            }

            let metadata = self.metadata.as_ref().expect("metadata must be set");
            let data = Bytes::copy_from_slice(payload);
            let sample = Sample {
                track_kind: TrackKind::Audio,
                sample_entry,
                keyframe: false,
                timescale: NonZeroU32::new(metadata.timescale).unwrap(),
                duration: AAC_SAMPLES_PER_FRAME,
                composition_time_offset: None,
                data_offset: 0,
                data_size: data.len(),
            };
            let sample_dts = self.next_dts;
            self.next_dts = self
                .next_dts
                .map(|dts| dts + f64::from(AAC_SAMPLES_PER_FRAME) / f64::from(metadata.timescale));
            samples.push(TrackSample {
                sample,
                data,
                dts: sample_dts,
            });
        }

        Ok(samples)
    }
}

struct AacLatmTrack {
    metadata: Option<TrackMetadata>,
}

impl AacLatmTrack {
    fn new() -> Self {
        Self { metadata: None }
    }
}

impl Track for AacLatmTrack {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        _pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>> {
        let mut samples = Vec::<TrackSample>::new();

        let mut cursor = Cursor::new(data.as_ref());
        let mut previous = None::<LoasFrame>;

        while let Ok(sample) = LoasFrame::next(&mut cursor, previous.as_ref()) {
            previous = Some(sample.clone());

            let timescale = sample.sampling_frequency.hz();
            let mut sample_entry = None;
            if self.metadata.is_none() {
                info!(
                    audio_object_type = sample.audio_object_type,
                    sampling_frequency = timescale,
                    channel_configuration = sample.channel_configuration,
                    "AAC-LATM track is ready"
                );

                sample_entry = Some(build_mp4a_sample_entry(
                    sample.audio_object_type,
                    sample.sampling_frequency,
                    ChannelConfiguration::from_raw(sample.channel_configuration)?,
                )?);
                self.metadata = Some(TrackMetadata {
                    sample_duration: AAC_SAMPLES_PER_FRAME,
                    timescale,
                });
            }

            let Some(data) = sample.data else {
                continue;
            };

            let metadata = self.metadata.as_ref().expect("metadata must be set");
            let sample = Sample {
                track_kind: TrackKind::Audio,
                sample_entry,
                keyframe: false,
                timescale: NonZeroU32::new(metadata.timescale).unwrap(),
                duration: metadata.sample_duration,
                composition_time_offset: None,
                data_offset: 0,
                data_size: data.len(),
            };

            let sample_index = samples.len() as f64;
            let sample_dts =
                dts.map(|dts| dts + sample_index * 1024_f64 / f64::from(metadata.timescale));

            samples.push(TrackSample {
                sample,
                data,
                dts: sample_dts,
            });
        }

        Ok(samples)
    }
}

/// ISOBMFF/MP4 muxer
pub struct Mp4Muxer<W> {
    muxer: Mp4FileMuxer,
    writer: W,
    data_offset: u64,
    track_map: BTreeMap<u16, Box<dyn Track>>,
}

impl<W: Write + Seek> Mp4Muxer<W> {
    pub fn new(writer: W) -> Self {
        Self {
            muxer: Mp4FileMuxer::new().unwrap(),
            writer,
            data_offset: 0,
            track_map: BTreeMap::new(),
        }
    }

    fn append_track_samples(&mut self, samples: Vec<TrackSample>) -> anyhow::Result<()> {
        for TrackSample {
            mut sample, data, ..
        } in samples
        {
            sample.data_offset = self.data_offset;
            self.writer.write_all(&data)?;
            self.muxer.append_sample(&sample)?;
            self.data_offset += sample.data_size as u64;
        }
        Ok(())
    }
}

impl<W: Write + Seek> Mux for Mp4Muxer<W> {
    fn add_track(&mut self, track_id: u16, ty: TrackType) {
        if self.track_map.contains_key(&track_id) {
            return;
        }

        match ty {
            TrackType::Mpeg2Video => {
                self.track_map
                    .insert(track_id, Box::new(Mpeg2VideoTrack::new()));
                info!(track_id, "Added an MPEG-2 video track");
            }
            TrackType::AacAdts => {
                self.track_map
                    .insert(track_id, Box::new(AacAdtsTrack::new()));
                info!(track_id, "Added an AAC-ADTS audio track");
            }
            TrackType::H265 => {
                self.track_map.insert(track_id, Box::new(H265Track::new()));
                info!(track_id, "Added a H265 video track");
            }
            TrackType::AacLatm => {
                self.track_map
                    .insert(track_id, Box::new(AacLatmTrack::new()));
                info!(track_id, "Added an AAC-LATM audio track");
            }
        }
    }

    fn begin(&mut self) -> anyhow::Result<()> {
        let initial_bytes = self.muxer.initial_boxes_bytes();

        self.writer.write_all(initial_bytes)?;
        self.data_offset += initial_bytes.len() as u64;

        Ok(())
    }

    fn write_sample(
        &mut self,
        track_id: u16,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<()> {
        let Some(track) = self.track_map.get_mut(&track_id) else {
            return Ok(());
        };

        let samples = track.write_sample(data, dts, pts)?;
        self.append_track_samples(samples)
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        let track_ids = self.track_map.keys().copied().collect::<Vec<_>>();
        for track_id in track_ids {
            let samples = self.track_map.get_mut(&track_id).unwrap().finalize()?;
            self.append_track_samples(samples)?;
        }

        for (offset, bytes) in self.muxer.finalize()?.offset_and_bytes_pairs() {
            self.writer.seek(SeekFrom::Start(offset))?;
            self.writer.write_all(bytes)?;
        }

        Ok(())
    }
}

pub trait WriteMp4Fragment {
    fn write_fragment(&mut self, data: Bytes) -> anyhow::Result<()>;
}

impl<T> WriteMp4Fragment for T
where
    T: Write,
{
    fn write_fragment(&mut self, data: Bytes) -> anyhow::Result<()> {
        self.write_all(&data)?;
        Ok(())
    }
}

pub struct FragmentedMp4Muxer<W> {
    writer: W,
    muxer: Fmp4SegmentMuxer,
    track_map: BTreeMap<u16, Box<dyn Track>>,
    track_states: BTreeMap<u16, FragmentedTrackState>,
    sync_start_dts: Option<f64>,
    pending_fragments: Vec<MediaFragment>,
    init_segment_written: bool,
}

impl<W: WriteMp4Fragment> FragmentedMp4Muxer<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            muxer: Fmp4SegmentMuxer::new().expect("failed to create fMP4 muxer"),
            track_map: BTreeMap::new(),
            track_states: BTreeMap::new(),
            sync_start_dts: None,
            pending_fragments: Vec::new(),
            init_segment_written: false,
        }
    }

    fn write_track_samples(
        &mut self,
        track_id: u16,
        mut samples: Vec<TrackSample>,
    ) -> anyhow::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }

        {
            let track_state = self.track_states.entry(track_id).or_default();
            track_state.observe_samples(&samples);
        }

        if self.sync_start_dts.is_none()
            && self
                .track_states
                .values()
                .all(|track_state| track_state.first_dts.is_some())
        {
            self.sync_start_dts = self
                .track_states
                .values()
                .filter_map(|track_state| track_state.first_dts)
                .max_by(f64::total_cmp);
        }

        if self.track_states.len() > 1 && self.sync_start_dts.is_none() {
            return Ok(());
        }

        if let Some(sync_start_dts) = self.sync_start_dts {
            samples.retain(|sample| sample.dts.is_none_or(|dts| dts >= sync_start_dts));
        }
        if samples.is_empty() {
            return Ok(());
        }

        let track_state = self.track_states.entry(track_id).or_default();
        let mut payload = BytesMut::new();
        let mut segment_samples = Vec::with_capacity(samples.len());
        for TrackSample {
            mut sample, data, ..
        } in samples
        {
            track_state.attach_sample_entry_if_needed(&mut sample);
            sample.data_offset = payload.len() as u64;
            payload.extend_from_slice(&data);
            segment_samples.push(sample);
        }

        track_state.observe_fragment_samples(&segment_samples);

        let metadata = Bytes::from(self.muxer.create_media_segment_metadata(&segment_samples)?);
        let payload = payload.freeze();

        if !self.init_segment_written {
            self.pending_fragments
                .push(MediaFragment { metadata, payload });

            if !self
                .track_states
                .values()
                .all(|track_state| track_state.ready)
            {
                return Ok(());
            }

            let init_segment = self.muxer.init_segment_bytes()?;
            self.writer.write_fragment(Bytes::from(init_segment))?;
            self.init_segment_written = true;

            for fragment in self.pending_fragments.drain(..) {
                self.writer.write_fragment(fragment.metadata)?;
                self.writer.write_fragment(fragment.payload)?;
            }

            return Ok(());
        }

        self.writer.write_fragment(metadata)?;
        self.writer.write_fragment(payload)?;

        Ok(())
    }
}

impl<W: WriteMp4Fragment> Mux for FragmentedMp4Muxer<W> {
    fn add_track(&mut self, track_id: u16, ty: TrackType) {
        if self.track_map.contains_key(&track_id) {
            return;
        }

        match ty {
            TrackType::Mpeg2Video => {
                self.track_map
                    .insert(track_id, Box::new(Mpeg2VideoTrack::new()));
                self.track_states.entry(track_id).or_default();
                info!(track_id, "Added an MPEG-2 video track");
            }
            TrackType::AacAdts => {
                self.track_map
                    .insert(track_id, Box::new(AacAdtsTrack::new()));
                self.track_states.entry(track_id).or_default();
                info!(track_id, "Added an AAC-ADTS audio track");
            }
            TrackType::H265 => {
                self.track_map.insert(track_id, Box::new(H265Track::new()));
                self.track_states.entry(track_id).or_default();
                info!(track_id, "Added a H265 video track");
            }
            TrackType::AacLatm => {
                self.track_map
                    .insert(track_id, Box::new(AacLatmTrack::new()));
                self.track_states.entry(track_id).or_default();
                info!(track_id, "Added an AAC-LATM audio track");
            }
        }
    }

    fn write_sample(
        &mut self,
        track_id: u16,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<()> {
        let Some(track) = self.track_map.get_mut(&track_id) else {
            return Ok(());
        };

        let samples = track.write_sample(data, dts, pts)?;
        self.write_track_samples(track_id, samples)
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        let track_ids = self.track_map.keys().copied().collect::<Vec<_>>();
        for track_id in track_ids {
            let samples = self.track_map.get_mut(&track_id).unwrap().finalize()?;
            self.write_track_samples(track_id, samples)?;
        }
        Ok(())
    }
}

fn seconds_to_timescale_units(seconds: f64, timescale: u32) -> i64 {
    (seconds * f64::from(timescale)).round() as i64
}

fn build_mp4v_sample_entry(sequence: &SequenceHeader) -> SampleEntry {
    let visual = VisualSampleEntryFields {
        data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
        width: sequence.width,
        height: sequence.height,
        horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
        vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
        frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
        compressorname: compressor_name(),
        depth: VisualSampleEntryFields::DEFAULT_DEPTH,
    };
    let esds_box = EsdsBox {
        es: EsDescriptor {
            es_id: EsDescriptor::MIN_ES_ID,
            stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
            depends_on_es_id: None,
            url_string: None,
            ocr_es_id: None,
            dec_config_descr: DecoderConfigDescriptor {
                object_type_indication: sequence.object_type_indication(),
                stream_type: Uint::new(0x04), // VisualStream
                up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                buffer_size_db: Uint::new(sequence.vbv_buffer_size),
                max_bitrate: sequence.bit_rate,
                avg_bitrate: sequence.bit_rate,
                dec_specific_info: Some(DecoderSpecificInfo {
                    payload: sequence.decoder_config.to_vec(),
                }),
            },
            sl_config_descr: SlConfigDescriptor,
        },
    };

    SampleEntry::Mp4v(Mp4vBox {
        visual,
        esds_box,
        unknown_boxes: vec![],
    })
}

/// Builds the `mp4a` sample entry for an AAC track.
///
/// `shiguredo_mp4` only assembles AAC-LC AudioSpecificConfigs, so HE-AAC (SBR)
/// reuses the same box and only swaps the config payload.
fn build_mp4a_sample_entry(
    audio_object_type: u8,
    sampling_frequency: SamplingFrequency,
    channel_configuration: ChannelConfiguration,
) -> anyhow::Result<SampleEntry> {
    let mut mp4a = build_mp4a_box(
        &AudioSpecificConfig {
            audio_object_type: AudioObjectType::AacLc,
            sampling_frequency,
            channel_configuration,
        },
        &Mp4aSampleEntryConfig {
            es_id: EsDescriptor::MIN_ES_ID,
            buffer_size_db: 0,
            max_bitrate: 0,
            avg_bitrate: 0,
        },
    )?;

    match audio_object_type {
        AAC_OBJECT_TYPE_LC => {}
        AAC_OBJECT_TYPE_SBR => {
            mp4a.esds_box.es.dec_config_descr.dec_specific_info = Some(DecoderSpecificInfo {
                payload: encode_explicit_sbr_config(sampling_frequency, channel_configuration)?,
            });
        }
        _ => anyhow::bail!("Unsupported AAC audio object type: {audio_object_type}"),
    }

    Ok(SampleEntry::Mp4a(mp4a))
}

/// Builds the AudioSpecificConfig of the explicit SBR signalling (ISO/IEC
/// 14496-3 1.6.2.1).
///
/// The 22 bits are the SBR object type (5), the core sampling frequency index
/// (4), the channel configuration (4), the extension sampling frequency index
/// (4) and the core object type (5), written MSB first.
fn encode_explicit_sbr_config(
    sampling_frequency: SamplingFrequency,
    channel_configuration: ChannelConfiguration,
) -> anyhow::Result<Vec<u8>> {
    let index = sampling_frequency_index(sampling_frequency)?;
    // In the index table, three entries up is twice the frequency: 48 kHz
    // (index 3) extends to 96 kHz (index 0).
    let extension_index = index.saturating_sub(3);

    let bits = (u32::from(AAC_OBJECT_TYPE_SBR) << 17)
        | (u32::from(index) << 13)
        | (u32::from(channel_configuration.as_u8()) << 9)
        | (u32::from(extension_index) << 5)
        | u32::from(AAC_OBJECT_TYPE_LC);

    Ok(vec![
        (bits >> 14) as u8,
        (bits >> 6) as u8,
        (bits << 2) as u8,
    ])
}

/// Recovers the `samplingFrequencyIndex` of a sampling frequency.
///
/// `SamplingFrequency` does not expose the index, so the table is searched by
/// the effective frequency.
fn sampling_frequency_index(sampling_frequency: SamplingFrequency) -> anyhow::Result<u8> {
    (0..=12u8)
        .find(|index| {
            SamplingFrequency::from_index(*index)
                .is_ok_and(|frequency| frequency == sampling_frequency)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No samplingFrequencyIndex for {} Hz",
                sampling_frequency.hz()
            )
        })
}

fn compressor_name() -> [u8; 32] {
    let mut value = [0; 32];
    value[..27].copy_from_slice(b"github.com/siketyan/chibitv");
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-assembled HEVC VPS NAL unit (type 32).
    ///
    /// Only the two-byte NAL header is inspected, so the payload is arbitrary.
    const H265_VPS: &[u8] = &[0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF];

    /// A hand-assembled HEVC PPS NAL unit (type 34).
    ///
    /// Only the two-byte NAL header is inspected, so the payload is arbitrary.
    const H265_PPS: &[u8] = &[0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40];

    /// A hand-assembled HEVC SPS NAL unit (type 33).
    ///
    /// Main profile / level 3.0 / 320x240 / 4:2:0 / 8 bit, with a VUI carrying
    /// `vui_num_units_in_tick = 1` and `vui_time_scale = 30`, i.e. 30 fps. The
    /// bytes are the EBSP, so they include emulation prevention bytes.
    const H265_SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x5A, 0xA0, 0x0A, 0x08, 0x0F, 0x16, 0x52, 0xE4, 0x93, 0x2A, 0x87, 0x40, 0x00,
        0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x03, 0x07, 0x84,
    ];

    /// An IDR_W_RADL slice NAL unit (type 19), which starts a random access point.
    const H265_IDR: &[u8] = &[0x26, 0x01, 0xAF, 0x06, 0x30];

    /// Concatenates NAL units into an Annex B access unit.
    fn h265_access_unit(nal_units: &[&[u8]]) -> Bytes {
        let mut data = Vec::new();
        for nal_unit in nal_units {
            data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            data.extend_from_slice(nal_unit);
        }
        Bytes::from(data)
    }

    fn mpeg2_picture(coding_type: u8) -> Vec<u8> {
        vec![
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            coding_type << 3,
            0x00,
            0x00,
            0x01,
            0x01,
            0xAA,
        ]
    }

    fn mpeg2_sequence_and_picture() -> Bytes {
        let mut data = vec![
            0x00, 0x00, 0x01, 0xB3, 0x78, 0x04, 0x38, 0x34, 0x09, 0xC4, 0x23, 0x80,
        ];
        data.extend_from_slice(&mpeg2_picture(1));
        Bytes::from(data)
    }

    fn adts_frame(payload: &[u8]) -> Bytes {
        let frame_length = 7 + payload.len();
        let mut data = vec![
            0xFF,
            0xF1,
            0x50, // AAC-LC, 44.1 kHz, channel_configuration high bit
            0x80 | ((frame_length >> 11) & 0x03) as u8,
            (frame_length >> 3) as u8,
            ((frame_length & 0x07) << 5) as u8 | 0x1F,
            0xFC,
        ];
        data.extend_from_slice(payload);
        Bytes::from(data)
    }

    #[test]
    fn creates_mp4v_samples_from_mpeg2_video() {
        let mut track = Mpeg2VideoTrack::new();

        assert!(
            track
                .write_sample(mpeg2_sequence_and_picture(), Some(0.0), Some(0.0))
                .unwrap()
                .is_empty()
        );
        let samples = track
            .write_sample(Bytes::from(mpeg2_picture(2)), None, None)
            .unwrap();

        assert_eq!(samples.len(), 1);
        assert!(samples[0].sample.keyframe);
        assert!(matches!(
            samples[0].sample.sample_entry,
            Some(SampleEntry::Mp4v(_))
        ));
        assert!(samples[0].data.starts_with(&[0x00, 0x00, 0x01, 0xB3]));
    }

    #[test]
    fn creates_mp4a_samples_from_adts_without_the_adts_header() {
        let mut track = AacAdtsTrack::new();
        let samples = track
            .write_sample(adts_frame(&[0xDE, 0xAD]), Some(1.0), None)
            .unwrap();

        assert_eq!(samples.len(), 1);
        assert_eq!(&samples[0].data[..], &[0xDE, 0xAD]);
        assert_eq!(samples[0].sample.duration, 1024);
        assert!(matches!(
            samples[0].sample.sample_entry,
            Some(SampleEntry::Mp4a(_))
        ));
    }

    #[test]
    fn writes_mpeg2_video_to_a_fragmented_mp4() {
        let mut mux = FragmentedMp4Muxer::new(Vec::new());
        mux.add_track(1, TrackType::Mpeg2Video);

        mux.write_sample(1, mpeg2_sequence_and_picture(), Some(0.0), Some(0.0))
            .unwrap();
        assert!(mux.writer.is_empty());
        mux.finalize().unwrap();

        assert!(mux.writer.windows(4).any(|bytes| bytes == b"ftyp"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"mp4v"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"moof"));
    }

    #[test]
    fn writes_the_first_mp4v_sample_entry_when_dts_is_missing_later() {
        let mut mux = Mp4Muxer::new(Cursor::new(Vec::new()));
        mux.add_track(1, TrackType::Mpeg2Video);
        mux.begin().unwrap();

        mux.write_sample(1, mpeg2_sequence_and_picture(), Some(0.0), Some(0.0))
            .unwrap();
        mux.write_sample(1, Bytes::from(mpeg2_picture(2)), None, None)
            .unwrap();
        mux.finalize().unwrap();

        let output = mux.writer.into_inner();
        assert!(output.windows(4).any(|bytes| bytes == b"mp4v"));
    }

    #[test]
    fn writes_adts_audio_to_a_fragmented_mp4() {
        let mut mux = FragmentedMp4Muxer::new(Vec::new());
        mux.add_track(1, TrackType::AacAdts);
        mux.write_sample(1, adts_frame(&[0xDE, 0xAD]), Some(0.0), None)
            .unwrap();

        assert!(mux.writer.windows(4).any(|bytes| bytes == b"ftyp"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"mp4a"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"moof"));
    }

    #[test]
    fn writes_fragmented_mp4_when_adts_has_only_pts() {
        let mut mux = FragmentedMp4Muxer::new(Vec::new());
        mux.add_track(1, TrackType::Mpeg2Video);
        mux.add_track(2, TrackType::AacAdts);

        mux.write_sample(1, mpeg2_sequence_and_picture(), Some(0.0), Some(0.0))
            .unwrap();
        mux.write_sample(1, Bytes::from(mpeg2_picture(2)), Some(1.0 / 30.0), None)
            .unwrap();
        mux.write_sample(2, adts_frame(&[0xDE, 0xAD]), None, Some(0.0))
            .unwrap();
        mux.write_sample(1, Bytes::from(mpeg2_picture(2)), Some(2.0 / 30.0), None)
            .unwrap();
        mux.finalize().unwrap();

        assert!(mux.writer.windows(4).any(|bytes| bytes == b"ftyp"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"mp4v"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"mp4a"));
        assert!(mux.writer.windows(4).any(|bytes| bytes == b"moof"));
    }
    #[test]
    fn builds_an_h265_sample_entry_from_the_parameter_sets() {
        let mut track = H265Track::new();

        let samples = track
            .write_sample(
                h265_access_unit(&[H265_VPS, H265_SPS, H265_PPS, H265_IDR]),
                None,
                None,
            )
            .unwrap();

        assert_eq!(samples.len(), 1);
        let sample = &samples[0].sample;
        assert!(sample.keyframe);
        assert_eq!(sample.timescale.get(), VIDEO_TIMESCALE);
        // 90000 / 30 fps
        assert_eq!(sample.duration, 3000);

        let Some(SampleEntry::Hev1(hev1)) = &sample.sample_entry else {
            panic!("the first sample must carry an hev1 sample entry");
        };
        assert_eq!(hev1.visual.width, 320);
        assert_eq!(hev1.visual.height, 240);
        assert_eq!(hev1.visual.compressorname, compressor_name());
        // 256 seconds worth of frames at 30 fps
        assert_eq!(hev1.hvcc_box.avg_frame_rate, 7680);
        assert_eq!(hev1.hvcc_box.length_size_minus_one.get(), 3);
        assert_eq!(hev1.hvcc_box.nalu_arrays[0].nalus, vec![H265_VPS.to_vec()]);
        assert_eq!(hev1.hvcc_box.nalu_arrays[1].nalus, vec![H265_SPS.to_vec()]);
        assert_eq!(hev1.hvcc_box.nalu_arrays[2].nalus, vec![H265_PPS.to_vec()]);

        // The Annex B start codes are replaced with 4-byte length prefixes.
        let data = &samples[0].data;
        assert_eq!(&data[..4], &(H265_VPS.len() as u32).to_be_bytes());
        assert_eq!(&data[4..4 + H265_VPS.len()], H265_VPS);
    }

    #[test]
    fn emits_no_h265_sample_until_every_parameter_set_arrives() {
        let mut track = H265Track::new();

        // VPS と SPS だけではサンプルエントリーを組めない
        let samples = track
            .write_sample(
                h265_access_unit(&[H265_VPS, H265_SPS, H265_IDR]),
                None,
                None,
            )
            .unwrap();
        assert!(samples.is_empty());

        let samples = track
            .write_sample(h265_access_unit(&[H265_PPS, H265_IDR]), None, None)
            .unwrap();
        assert_eq!(samples.len(), 1);
        assert!(samples[0].sample.sample_entry.is_some());
    }

    #[test]
    fn drops_a_malformed_h265_access_unit() {
        let mut track = H265Track::new();

        // 開始コードの無い入力は解析できないが、エラーにはしない
        let samples = track
            .write_sample(Bytes::from_static(&[0x01, 0x02, 0x03]), None, None)
            .unwrap();

        assert!(samples.is_empty());
    }
    #[test]
    fn builds_an_aac_lc_sample_entry() {
        let entry = build_mp4a_sample_entry(
            AAC_OBJECT_TYPE_LC,
            SamplingFrequency::from_hz(48000).unwrap(),
            ChannelConfiguration::Stereo,
        )
        .unwrap();

        let SampleEntry::Mp4a(mp4a) = entry else {
            panic!("an AAC track must use an mp4a sample entry");
        };
        assert_eq!(mp4a.audio.channelcount, 2);
        assert_eq!(mp4a.audio.samplerate.integer, 48000);
        // AOT 2 / sampling frequency index 3 / channel configuration 2
        assert_eq!(
            mp4a.esds_box
                .es
                .dec_config_descr
                .dec_specific_info
                .map(|info| info.payload),
            Some(vec![0x11, 0x90])
        );
    }

    #[test]
    fn builds_an_explicit_sbr_sample_entry() {
        let entry = build_mp4a_sample_entry(
            AAC_OBJECT_TYPE_SBR,
            SamplingFrequency::from_hz(48000).unwrap(),
            ChannelConfiguration::Stereo,
        )
        .unwrap();

        let SampleEntry::Mp4a(mp4a) = entry else {
            panic!("an AAC track must use an mp4a sample entry");
        };
        // AOT 5 / core index 3 (48 kHz) / channel configuration 2 /
        // extension index 0 (96 kHz) / core AOT 2
        assert_eq!(
            mp4a.esds_box
                .es
                .dec_config_descr
                .dec_specific_info
                .map(|info| info.payload),
            Some(vec![0x29, 0x90, 0x08])
        );
    }

    #[test]
    fn rejects_an_unsupported_audio_object_type() {
        let result = build_mp4a_sample_entry(
            1,
            SamplingFrequency::from_hz(48000).unwrap(),
            ChannelConfiguration::Stereo,
        );

        assert!(result.is_err());
    }
}
