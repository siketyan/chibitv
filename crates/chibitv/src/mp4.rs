use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::num::{NonZeroU16, NonZeroU32};

use bytes::{BufMut, Bytes, BytesMut};
use cros_codecs::codec::h265::parser::{
    Nalu, NaluType, Parser as H265Parser, Pps, ProfileTierLevel, Sps, Vps,
};
use shiguredo_mp4::boxes::{
    AudioSampleEntryFields, EsdsBox, Hev1Box, HvccBox, HvccNalUintArray, Mp4aBox, Mp4vBox,
    SampleEntry, VisualSampleEntryFields,
};
use shiguredo_mp4::descriptors::{
    DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor,
};
use shiguredo_mp4::mux::{Fmp4SegmentMuxer, Mp4FileMuxer, Sample};
use shiguredo_mp4::{FixedPointNumber, TrackKind, Uint};
use tracing::{debug, error, info};

use crate::aac::{AdtsHeader, AdtsParser, LoasFrame, SamplingFrequency};
use crate::demux::TrackType;
use crate::mp2::{Mp2Parser, PictureCodingType, SequenceHeader, picture_coding_type};
use crate::remux::Mux;

const VIDEO_TIMESCALE: u32 = 90_000;

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

trait Track {
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
    parser: H265Parser,
    vps: Option<(Bytes, Vps)>,
    pps: Option<(Bytes, Pps)>,
    sps: Option<(Bytes, Sps)>,
    metadata: Option<TrackMetadata>,
    pending: Option<PendingSample>,
}

impl H265Track {
    fn new() -> Self {
        Self {
            parser: H265Parser::default(),
            vps: None,
            pps: None,
            sps: None,
            metadata: None,
            pending: None,
        }
    }
}

impl Track for H265Track {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<TrackSample>> {
        let mut keyframe = false;
        let mut sample_entry = None::<SampleEntry>;
        let mut nalus = Vec::<Nalu>::new();

        let mut cursor = Cursor::new(data.as_ref());
        while let Ok(nalu) = Nalu::next(&mut cursor) {
            match nalu.header.type_ {
                NaluType::VpsNut if self.metadata.is_none() => match self.parser.parse_vps(&nalu) {
                    Ok(vps) => {
                        self.vps = Some((Bytes::copy_from_slice(nalu.as_ref()), vps.clone()));
                        debug!("VPS NALU found: {:?}", vps);
                    }
                    Err(err) => error!("VPS parse error: {}", err),
                },
                NaluType::PpsNut if self.metadata.is_none() => match self.parser.parse_pps(&nalu) {
                    Ok(pps) => {
                        self.pps = Some((Bytes::copy_from_slice(nalu.as_ref()), pps.clone()));
                        debug!("PPS NALU found: {:?}", pps);
                    }
                    Err(err) => error!("PPS parse error: {}", err),
                },
                NaluType::SpsNut if self.metadata.is_none() => match self.parser.parse_sps(&nalu) {
                    Ok(sps) => {
                        self.sps = Some((Bytes::copy_from_slice(nalu.as_ref()), sps.clone()));
                        debug!("SPS NALU found: {:?}", sps);
                    }
                    Err(err) => error!("SPS parse error: {}", err),
                },
                NaluType::IdrWRadl | NaluType::IdrNLp | NaluType::CraNut => {
                    keyframe = true;
                }
                _ => {}
            }

            nalus.push(nalu);
        }

        if self.metadata.is_none()
            && let (Some(vps), Some(pps), Some(sps)) = (&self.vps, &self.pps, &self.sps)
        {
            sample_entry = Some(build_hev1_sample_entry(vps, pps, sps));

            self.metadata = Some(TrackMetadata {
                sample_duration: sps.1.vui_parameters.num_units_in_tick * VIDEO_TIMESCALE
                    / sps.1.vui_parameters.time_scale,
                timescale: VIDEO_TIMESCALE,
            });

            debug!("H265 track is ready: {:?}", &self.metadata);
        }

        let Some(metadata) = &self.metadata else {
            // Stream is not ready yet.
            return Ok(vec![]);
        };

        let mut bytes = BytesMut::new();
        for nalu in nalus {
            let data = nalu.as_ref();
            bytes.put_u32(data.len() as u32);
            bytes.put(data);
        }

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
            let header = AdtsHeader::parse(&frame)?;
            let sample_entry = self.metadata.is_none().then(|| {
                self.metadata = Some(TrackMetadata {
                    sample_duration: u32::from(header.sample_count()),
                    timescale: header.sampling_frequency as u32,
                });
                info!(
                    audio_object_type = header.audio_object_type,
                    sampling_frequency = header.sampling_frequency as u32,
                    channel_configuration = header.channel_configuration,
                    "AAC-ADTS track is ready"
                );
                build_mp4a_sample_entry(
                    header.audio_object_type,
                    header.sampling_frequency_index,
                    header.sampling_frequency,
                    header.channel_configuration,
                )
            });

            let metadata = self.metadata.as_ref().expect("metadata must be set");
            let data = Bytes::copy_from_slice(header.payload(&frame)?);
            let sample = Sample {
                track_kind: TrackKind::Audio,
                sample_entry,
                keyframe: false,
                timescale: NonZeroU32::new(metadata.timescale).unwrap(),
                duration: u32::from(header.sample_count()),
                composition_time_offset: None,
                data_offset: 0,
                data_size: data.len(),
            };
            let sample_dts = self.next_dts;
            self.next_dts = self
                .next_dts
                .map(|dts| dts + f64::from(header.sample_count()) / f64::from(metadata.timescale));
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

            let sample_duration = 1024;
            let timescale = sample.sampling_frequency as u32;
            let sample_entry = self.metadata.is_none().then(|| {
                self.metadata = Some(TrackMetadata {
                    sample_duration,
                    timescale,
                });

                info!(
                    audio_object_type = sample.audio_object_type,
                    sampling_frequency = sample.sampling_frequency as u32,
                    channel_configuration = sample.channel_configuration,
                    "AAC-LATM track is ready"
                );

                build_mp4a_sample_entry(
                    sample.audio_object_type,
                    sample.sampling_frequency_index,
                    sample.sampling_frequency,
                    sample.channel_configuration,
                )
            });

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

// FragmentedMp4Muxer is moved into a single remuxer thread and is not shared
// between threads. H265Track contains cros-codecs parser state with Rc internals,
// which prevents auto Send even though this usage does not cross-share it.
unsafe impl<W: Send> Send for FragmentedMp4Muxer<W> {}

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

fn build_mp4a_sample_entry(
    audio_object_type: u8,
    sampling_frequency_index: u8,
    sampling_frequency: SamplingFrequency,
    channel_configuration: u8,
) -> SampleEntry {
    let audio = AudioSampleEntryFields {
        data_reference_index: NonZeroU16::new(1).unwrap(),
        channelcount: u16::from(channel_configuration),
        samplesize: 16,
        samplerate: FixedPointNumber::new(sampling_frequency as u16, 0),
    };
    let extension_sampling_index = sampling_frequency_index.saturating_sub(3);
    let mut config = vec![0; 4];
    config[0] = audio_object_type << 3 | (sampling_frequency_index & 0x0F) >> 1;
    config[1] = (sampling_frequency_index & 0x0F) << 7 | (channel_configuration & 0x0F) << 3;
    if audio_object_type == 5 {
        config[1] |= (extension_sampling_index & 0x0F) >> 1;
        config[2] = (extension_sampling_index & 0x01) << 7 | 2 << 2;
    } else {
        config.resize(2, 0);
    }
    let esds_box = EsdsBox {
        es: EsDescriptor {
            es_id: EsDescriptor::MIN_ES_ID,
            stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
            depends_on_es_id: None,
            url_string: None,
            ocr_es_id: None,
            dec_config_descr: DecoderConfigDescriptor {
                object_type_indication:
                    DecoderConfigDescriptor::OBJECT_TYPE_INDICATION_AUDIO_ISO_IEC_14496_3,
                stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                buffer_size_db: Uint::new(0),
                max_bitrate: 0,
                avg_bitrate: 0,
                dec_specific_info: Some(DecoderSpecificInfo { payload: config }),
            },
            sl_config_descr: SlConfigDescriptor,
        },
    };

    SampleEntry::Mp4a(Mp4aBox {
        audio,
        esds_box,
        unknown_boxes: vec![],
    })
}

fn compressor_name() -> [u8; 32] {
    let mut value = [0; 32];
    value[..27].copy_from_slice(b"github.com/siketyan/chibitv");
    value
}

fn build_hev1_sample_entry(
    vps: &(Bytes, Vps),
    pps: &(Bytes, Pps),
    sps: &(Bytes, Sps),
) -> SampleEntry {
    let (vps_raw, vps) = vps;
    let (pps_raw, pps) = pps;
    let (sps_raw, sps) = sps;

    let hvcc_box = HvccBox {
        general_profile_space: Uint::new(sps.profile_tier_level.general_profile_space),
        general_tier_flag: Uint::new(sps.profile_tier_level.general_tier_flag as u8),
        general_profile_idc: Uint::new(sps.profile_tier_level.general_profile_idc),
        general_profile_compatibility_flags: convert_general_profile_compatibility_flags(
            sps.profile_tier_level.general_profile_compatibility_flag,
        ),
        general_constraint_indicator_flags: convert_general_constraint_indicator_flags(
            &sps.profile_tier_level,
        ),
        general_level_idc: sps.profile_tier_level.general_level_idc as u8,
        min_spatial_segmentation_idc: Uint::new(
            sps.vui_parameters.min_spatial_segmentation_idc as u16,
        ),
        parallelism_type: Uint::new(
            match (pps.entropy_coding_sync_enabled_flag, pps.tiles_enabled_flag) {
                (true, true) => 0,
                (false, false) => 1,
                (false, true) => 2,
                (true, false) => 3,
            },
        ),
        chroma_format_idc: Uint::new(sps.chroma_format_idc),
        bit_depth_luma_minus8: Uint::new(sps.bit_depth_luma_minus8),
        bit_depth_chroma_minus8: Uint::new(sps.bit_depth_chroma_minus8),
        avg_frame_rate: 0,
        constant_frame_rate: Uint::new(0),
        num_temporal_layers: Uint::new(vps.max_sub_layers_minus1 + 1),
        temporal_id_nested: Uint::new(vps.temporal_id_nesting_flag as u8),
        length_size_minus_one: Uint::new(3), // NAL length size
        nalu_arrays: vec![
            HvccNalUintArray {
                array_completeness: Uint::new(0),
                nal_unit_type: Uint::new(NaluType::VpsNut as u8),
                nalus: vec![vps_raw.to_vec()],
            },
            HvccNalUintArray {
                array_completeness: Uint::new(0),
                nal_unit_type: Uint::new(NaluType::SpsNut as u8),
                nalus: vec![sps_raw.to_vec()],
            },
            HvccNalUintArray {
                array_completeness: Uint::new(0),
                nal_unit_type: Uint::new(NaluType::PpsNut as u8),
                nalus: vec![pps_raw.to_vec()],
            },
        ],
    };

    let visual = VisualSampleEntryFields {
        data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
        width: sps.width(),
        height: sps.height(),
        horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
        vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
        frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
        compressorname: compressor_name(),
        depth: VisualSampleEntryFields::DEFAULT_DEPTH,
    };

    SampleEntry::Hev1(Hev1Box {
        visual,
        hvcc_box,
        unknown_boxes: vec![],
    })
}

fn convert_general_profile_compatibility_flags(value: [bool; 32]) -> u32 {
    let mut result = 0u32;
    for (i, &flag) in value.iter().enumerate() {
        if flag {
            result |= 1 << (31 - i);
        }
    }
    result
}

fn convert_general_constraint_indicator_flags(ptl: &ProfileTierLevel) -> Uint<u64, 48> {
    let mut value: [u8; 8] = [0; 8];

    value[0] |= (ptl.general_progressive_source_flag as u8) << 7;
    value[0] |= (ptl.general_interlaced_source_flag as u8) << 6;
    value[0] |= (ptl.general_non_packed_constraint_flag as u8) << 5;
    value[0] |= (ptl.general_frame_only_constraint_flag as u8) << 4;
    value[0] |= (ptl.general_max_12bit_constraint_flag as u8) << 3;
    value[0] |= (ptl.general_max_10bit_constraint_flag as u8) << 2;
    value[0] |= (ptl.general_max_8bit_constraint_flag as u8) << 1;
    value[0] |= ptl.general_max_422chroma_constraint_flag as u8;

    value[1] |= (ptl.general_max_420chroma_constraint_flag as u8) << 7;
    value[1] |= (ptl.general_max_monochrome_constraint_flag as u8) << 6;
    value[1] |= (ptl.general_intra_constraint_flag as u8) << 5;
    value[1] |= (ptl.general_one_picture_only_constraint_flag as u8) << 4;
    value[1] |= (ptl.general_lower_bit_rate_constraint_flag as u8) << 3;
    value[1] |= (ptl.general_max_14bit_constraint_flag as u8) << 2;

    // 33 bits reserved

    value[5] |= ptl.general_inbld_flag as u8;

    // 16 bits reserved

    Uint::new(u64::from_be_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
