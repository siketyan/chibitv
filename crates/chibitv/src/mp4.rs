use std::collections::BTreeMap;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::num::{NonZeroU16, NonZeroU32};

use crate::aac::LoasFrame;
use crate::remux::{Mux, TrackType};
use bytes::{BufMut, Bytes, BytesMut};
use cros_codecs::codec::h265::parser::{
    Nalu, NaluType, Parser as H265Parser, Pps, ProfileTierLevel, Sps, Vps,
};
use shiguredo_mp4::boxes::{
    AudioSampleEntryFields, EsdsBox, Hev1Box, HvccBox, HvccNalUintArray, Mp4aBox, SampleEntry,
    VisualSampleEntryFields,
};
use shiguredo_mp4::descriptors::{
    DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor,
};
use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};
use shiguredo_mp4::{FixedPointNumber, TrackKind, Uint};
use tracing::{debug, error, info};

#[derive(Clone, Debug)]
struct TrackMetadata {
    sample_duration: u32,
}

trait Track {
    fn write_sample(
        &mut self,
        data: Bytes,
        dts: Option<f64>,
        pts: Option<f64>,
    ) -> anyhow::Result<Vec<(Sample, Bytes)>>;
}

struct H265Track {
    parser: H265Parser,
    vps: Option<(Bytes, Vps)>,
    pps: Option<(Bytes, Pps)>,
    sps: Option<(Bytes, Sps)>,
    metadata: Option<TrackMetadata>,
}

impl H265Track {
    fn new() -> Self {
        Self {
            parser: H265Parser::default(),
            vps: None,
            pps: None,
            sps: None,
            metadata: None,
        }
    }
}

impl Track for H265Track {
    fn write_sample(
        &mut self,
        data: Bytes,
        _dts: Option<f64>,
        _pts: Option<f64>,
    ) -> anyhow::Result<Vec<(Sample, Bytes)>> {
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
                sample_duration: sps.1.vui_parameters.num_units_in_tick * 1000
                    / sps.1.vui_parameters.time_scale,
            });

            debug!("Stream is ready: {:?}", &self.metadata);
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
            timescale: NonZeroU32::new(1000).unwrap(),
            duration: metadata.sample_duration,
            composition_time_offset: None,
            data_offset: 0,
            data_size: bytes.len(),
        };

        Ok(vec![(sample, bytes.freeze())])
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
        _dts: Option<f64>,
        _pts: Option<f64>,
    ) -> anyhow::Result<Vec<(Sample, Bytes)>> {
        let mut samples = Vec::<(Sample, Bytes)>::new();

        let mut cursor = Cursor::new(data.as_ref());
        let mut previous = None::<LoasFrame>;

        while let Ok(sample) = LoasFrame::next(&mut cursor, previous.as_ref()) {
            previous = Some(sample.clone());

            let sample_duration = 1024 * 1000 / (sample.sampling_frequency as u32);
            let sample_entry = self.metadata.is_none().then(|| {
                let audio = AudioSampleEntryFields {
                    data_reference_index: NonZeroU16::new(1).unwrap(),
                    channelcount: u16::from(sample.channel_configuration),
                    samplesize: 16,
                    samplerate: FixedPointNumber::new(sample.sampling_frequency as u16, 0),
                };

                let dec_specific_info = DecoderSpecificInfo {
                    payload: {
                        let sampling_index = sample.sampling_frequency_index;
                        let mut extension_sampling_index = sampling_index;
                        let mut audio_object_type = 5; // HE-AAC
                        if sampling_index >= 6 {
                            extension_sampling_index = sampling_index - 3;
                        } else if sample.channel_configuration == 1 {
                            audio_object_type = 2; // LC-AAC
                        }

                        let mut config = vec![0; 4];

                        config[0] = audio_object_type << 3 | (sampling_index & 0x0F) >> 1;
                        config[1] = (sampling_index & 0x0F) << 7 | (sample.channel_configuration & 0x0F) << 3;

                        if audio_object_type == 5 {
                            config[1] |= (extension_sampling_index & 0x0F) >> 1;
                            config[2] = (extension_sampling_index & 0x01) << 7 | 2 << 2;

                            config
                        } else {
                            config.resize(2, 0);
                            config
                        }
                    }
                };

                let esds_box = EsdsBox {
                    es: EsDescriptor {
                        es_id: EsDescriptor::MIN_ES_ID,
                        stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
                        depends_on_es_id: None,
                        url_string: None,
                        ocr_es_id: None,
                        dec_config_descr: DecoderConfigDescriptor {
                            object_type_indication: DecoderConfigDescriptor::OBJECT_TYPE_INDICATION_AUDIO_ISO_IEC_14496_3,
                            stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                            up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                            buffer_size_db: Uint::new(0),
                            max_bitrate: 0,
                            avg_bitrate: 0,
                            dec_specific_info: Some(dec_specific_info),
                        },
                        sl_config_descr: SlConfigDescriptor,
                    },
                };

                self.metadata = Some(TrackMetadata { sample_duration });

                SampleEntry::Mp4a(Mp4aBox {
                    audio,
                    esds_box,
                    unknown_boxes: vec![],
                })
            });

            let Some(data) = sample.data else {
                continue;
            };

            let sample = Sample {
                track_kind: TrackKind::Audio,
                sample_entry,
                keyframe: false,
                timescale: NonZeroU32::new(1000).unwrap(),
                duration: sample_duration,
                composition_time_offset: None,
                data_offset: 0,
                data_size: data.len(),
            };

            samples.push((sample, data));
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
}

impl<W: Write + Seek + Send + Sync> Mux for Mp4Muxer<W> {
    fn add_track(&mut self, track_id: u16, ty: TrackType) {
        if self.track_map.contains_key(&track_id) {
            return;
        }

        match ty {
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

        for (mut sample, bytes) in track.write_sample(data, dts, pts)? {
            sample.data_offset = self.data_offset;

            self.writer.write_all(&bytes)?;
            self.muxer.append_sample(&sample)?;

            self.data_offset += sample.data_size as u64;
        }

        Ok(())
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        for (offset, bytes) in self.muxer.finalize()?.offset_and_bytes_pairs() {
            self.writer.seek(SeekFrom::Start(offset))?;
            self.writer.write_all(bytes)?;
        }

        Ok(())
    }
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
        compressorname: {
            let mut value = [0u8; 32];
            value[..27].copy_from_slice(b"github.com/siketyan/chibitv");
            value
        },
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
