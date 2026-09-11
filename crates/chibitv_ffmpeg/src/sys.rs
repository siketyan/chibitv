//! Bindings to the C shim in `csrc/`, which is the only code that includes
//! FFmpeg's headers. The signatures mirror `chibitv_ffmpeg.h`.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int};

pub const CFF_OK: c_int = 0;
pub const CFF_AGAIN: c_int = 1;
pub const CFF_EOF: c_int = 2;

pub const CFF_NO_TIMESTAMP: i64 = i64::MIN;

pub const CFF_LOG_ERROR: c_int = 0;
pub const CFF_LOG_WARNING: c_int = 1;
pub const CFF_LOG_INFO: c_int = 2;

pub enum cff_device {}
pub enum cff_decoder {}
pub enum cff_filter {}
pub enum cff_encoder {}
pub enum cff_frame {}
pub enum cff_packet {}

pub type cff_log_callback = unsafe extern "C" fn(level: c_int, message: *const c_char);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct cff_frame_info {
    pub width: c_int,
    pub height: c_int,
    pub hardware: c_int,
    pub bit_depth: c_int,
    pub interlaced: c_int,
    pub pts: i64,
    pub duration: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct cff_packet_info {
    pub data: *const u8,
    pub size: usize,
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub keyframe: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct cff_encoder_params {
    pub time_base_num: c_int,
    pub time_base_den: c_int,
    pub frame_rate_num: c_int,
    pub frame_rate_den: c_int,
    pub bit_rate: i64,
    pub gop_size: c_int,
    pub options: *const c_char,
}

unsafe extern "C" {
    pub fn cff_version() -> *const c_char;
    pub fn cff_last_error() -> *const c_char;
    pub fn cff_set_log_callback(callback: cff_log_callback);
    pub fn cff_has_codec(name: *const c_char, encoder: c_int) -> c_int;

    pub fn cff_device_create(type_name: *const c_char, device: *const c_char) -> *mut cff_device;
    pub fn cff_device_free(device: *mut cff_device);

    pub fn cff_frame_alloc() -> *mut cff_frame;
    pub fn cff_frame_get_info(frame: *const cff_frame, info: *mut cff_frame_info);
    pub fn cff_frame_unref(frame: *mut cff_frame);
    pub fn cff_frame_free(frame: *mut cff_frame);
    pub fn cff_frame_alloc_picture(
        pixel_format: *const c_char,
        width: c_int,
        height: c_int,
    ) -> *mut cff_frame;
    pub fn cff_frame_plane(frame: *mut cff_frame, plane: c_int, linesize: *mut c_int) -> *mut u8;
    pub fn cff_frame_set_timing(frame: *mut cff_frame, pts: i64, duration: i64);
    pub fn cff_frame_set_interlaced(frame: *mut cff_frame, top_field_first: c_int);

    pub fn cff_packet_alloc() -> *mut cff_packet;
    pub fn cff_packet_get_info(packet: *const cff_packet, info: *mut cff_packet_info);
    pub fn cff_packet_unref(packet: *mut cff_packet);
    pub fn cff_packet_free(packet: *mut cff_packet);

    pub fn cff_decoder_open(
        codec_name: *const c_char,
        device: *mut cff_device,
        options: *const c_char,
    ) -> *mut cff_decoder;
    pub fn cff_decoder_send(
        decoder: *mut cff_decoder,
        data: *const u8,
        size: usize,
        pts: i64,
        dts: i64,
    ) -> c_int;
    pub fn cff_decoder_receive(decoder: *mut cff_decoder, frame: *mut cff_frame) -> c_int;
    pub fn cff_decoder_get_frame_rate(
        decoder: *const cff_decoder,
        num: *mut c_int,
        den: *mut c_int,
    );
    pub fn cff_decoder_free(decoder: *mut cff_decoder);

    pub fn cff_filter_open(
        description: *const c_char,
        first: *const cff_frame,
        frame_rate_num: c_int,
        frame_rate_den: c_int,
        device: *mut cff_device,
    ) -> *mut cff_filter;
    pub fn cff_filter_send(filter: *mut cff_filter, frame: *mut cff_frame) -> c_int;
    pub fn cff_filter_receive(filter: *mut cff_filter, frame: *mut cff_frame) -> c_int;
    pub fn cff_filter_get_output(
        filter: *const cff_filter,
        time_base_num: *mut c_int,
        time_base_den: *mut c_int,
        frame_rate_num: *mut c_int,
        frame_rate_den: *mut c_int,
    );
    pub fn cff_filter_free(filter: *mut cff_filter);

    pub fn cff_encoder_probe(
        codec_name: *const c_char,
        device: *mut cff_device,
        options: *const c_char,
    ) -> c_int;
    pub fn cff_encoder_open(
        codec_name: *const c_char,
        first: *const cff_frame,
        params: *const cff_encoder_params,
    ) -> *mut cff_encoder;
    pub fn cff_encoder_send(encoder: *mut cff_encoder, frame: *mut cff_frame) -> c_int;
    pub fn cff_encoder_receive(encoder: *mut cff_encoder, packet: *mut cff_packet) -> c_int;
    pub fn cff_encoder_free(encoder: *mut cff_encoder);
}
