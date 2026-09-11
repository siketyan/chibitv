//! Safe owners of the objects the C shim hands out.
//!
//! Each type frees what it owns on drop and reports failures as
//! [`Error::Codec`] carrying the shim's description. Every timestamp here is
//! in the 90 kHz clock, and [`NO_TIMESTAMP`] stands for an unknown one.

use std::ffi::{CStr, CString, c_int};
use std::ptr::{NonNull, null, null_mut};
use std::sync::Once;

use crate::Error;
use crate::sys;

/// The unknown timestamp of the shim's API.
pub const NO_TIMESTAMP: i64 = sys::CFF_NO_TIMESTAMP;

/// The clock every timestamp of the shim's API is in.
pub const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub fn is_positive(self) -> bool {
        self.num > 0 && self.den > 0
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }
}

/// What a send or receive call came back with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Ok,
    /// More input is needed before there is output.
    Again,
    /// Nothing more will come out.
    Eof,
}

fn status(code: c_int) -> Result<Status, Error> {
    match code {
        sys::CFF_OK => Ok(Status::Ok),
        sys::CFF_AGAIN => Ok(Status::Again),
        sys::CFF_EOF => Ok(Status::Eof),
        _ => Err(last_error()),
    }
}

/// The error the shim last reported on this thread.
fn last_error() -> Error {
    // SAFETY: the shim returns a pointer to a thread-local, NUL-terminated
    // buffer that outlives this call.
    let message = unsafe { CStr::from_ptr(sys::cff_last_error()) };
    Error::Codec(message.to_string_lossy().into_owned())
}

fn c_string(value: &str) -> CString {
    CString::new(value).expect("an option or name with a NUL byte")
}

/// Routes FFmpeg's log through `tracing` once per process.
pub fn init_logging() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: the callback outlives the process, and is safe to call from
        // any thread as the shim's contract requires.
        unsafe { sys::cff_set_log_callback(log_bridge) };
    });
}

unsafe extern "C" fn log_bridge(level: c_int, message: *const std::ffi::c_char) {
    // SAFETY: the shim passes a NUL-terminated line that lives for the call.
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let message = message.as_ref();
    match level {
        sys::CFF_LOG_ERROR => tracing::error!(target: "ffmpeg", "{message}"),
        sys::CFF_LOG_WARNING => tracing::warn!(target: "ffmpeg", "{message}"),
        sys::CFF_LOG_INFO => tracing::info!(target: "ffmpeg", "{message}"),
        _ => tracing::debug!(target: "ffmpeg", "{message}"),
    }
}

/// The version of the FFmpeg linked in.
pub fn version() -> &'static str {
    // SAFETY: the shim returns a static string.
    unsafe { CStr::from_ptr(sys::cff_version()) }
        .to_str()
        .unwrap_or("unknown")
}

pub fn has_decoder(name: &str) -> bool {
    let name = c_string(name);
    // SAFETY: the name is a valid C string.
    unsafe { sys::cff_has_codec(name.as_ptr(), 0) != 0 }
}

pub fn has_encoder(name: &str) -> bool {
    let name = c_string(name);
    // SAFETY: the name is a valid C string.
    unsafe { sys::cff_has_codec(name.as_ptr(), 1) != 0 }
}

/// A hardware device that decoders, filters and encoders share pictures on.
pub struct Device(NonNull<sys::cff_device>);

// SAFETY: the shim's objects are used from one thread at a time, which owning
// them through `&mut self` guarantees; nothing ties them to the thread that
// made them.
unsafe impl Send for Device {}

impl Device {
    pub fn create(type_name: &str, device: Option<&str>) -> Result<Self, Error> {
        let type_name = c_string(type_name);
        let device = device.map(c_string);
        // SAFETY: both are valid C strings, the device an optional one.
        let created = unsafe {
            sys::cff_device_create(
                type_name.as_ptr(),
                device.as_ref().map_or(null(), |device| device.as_ptr()),
            )
        };
        NonNull::new(created).map(Self).ok_or_else(last_error)
    }

    fn as_ptr(&self) -> *mut sys::cff_device {
        self.0.as_ptr()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_device_free(self.0.as_ptr()) };
    }
}

fn device_ptr(device: Option<&Device>) -> *mut sys::cff_device {
    device.map_or(null_mut(), Device::as_ptr)
}

/// What a decoded or filtered picture is like.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    /// The picture lives in device memory.
    pub hardware: bool,
    /// Bits per sample; of the software picture behind a hardware one.
    pub bit_depth: u32,
    pub interlaced: bool,
    pub pts: i64,
    pub duration: i64,
}

/// A picture, or the room for one.
pub struct Frame(NonNull<sys::cff_frame>);

// SAFETY: see `Device`.
unsafe impl Send for Frame {}

impl Frame {
    pub fn new() -> Result<Self, Error> {
        // SAFETY: allocates a fresh frame or reports why not.
        NonNull::new(unsafe { sys::cff_frame_alloc() })
            .map(Self)
            .ok_or_else(last_error)
    }

    /// A writable picture in the pixel format named the way FFmpeg does.
    #[cfg(test)]
    pub fn picture(pixel_format: &str, width: u32, height: u32) -> Result<Self, Error> {
        let pixel_format = c_string(pixel_format);
        // SAFETY: the name is a valid C string.
        let frame = unsafe {
            sys::cff_frame_alloc_picture(pixel_format.as_ptr(), width as c_int, height as c_int)
        };
        NonNull::new(frame).map(Self).ok_or_else(last_error)
    }

    /// The bytes of one plane and its stride, of a picture made by [`Frame::picture`].
    #[cfg(test)]
    pub fn plane(&mut self, plane: u32) -> Option<(&mut [u8], usize)> {
        let mut linesize: c_int = 0;
        // SAFETY: the frame is a valid, writable picture.
        let data = unsafe { sys::cff_frame_plane(self.0.as_ptr(), plane as c_int, &mut linesize) };
        if data.is_null() || linesize <= 0 {
            return None;
        }
        let rows = usize::try_from(self.info().height).ok()?;
        let rows = if plane == 0 { rows } else { rows.div_ceil(2) };
        // SAFETY: the plane holds `linesize` bytes for each of its rows, and
        // the chroma planes of the formats the tests use are half as tall.
        let bytes = unsafe { std::slice::from_raw_parts_mut(data, linesize as usize * rows) };
        Some((bytes, linesize as usize))
    }

    pub fn set_timing(&mut self, pts: i64, duration: i64) {
        // SAFETY: the frame is valid.
        unsafe { sys::cff_frame_set_timing(self.0.as_ptr(), pts, duration) };
    }

    #[cfg(test)]
    pub fn set_interlaced(&mut self, top_field_first: bool) {
        // SAFETY: the frame is valid.
        unsafe { sys::cff_frame_set_interlaced(self.0.as_ptr(), c_int::from(top_field_first)) };
    }

    pub fn info(&self) -> FrameInfo {
        let mut info = sys::cff_frame_info::default();
        // SAFETY: the frame is valid and the info struct is ours to fill.
        unsafe { sys::cff_frame_get_info(self.0.as_ptr(), &mut info) };
        FrameInfo {
            width: info.width.max(0) as u32,
            height: info.height.max(0) as u32,
            hardware: info.hardware != 0,
            bit_depth: info.bit_depth.max(0) as u32,
            interlaced: info.interlaced != 0,
            pts: info.pts,
            duration: info.duration,
        }
    }

    /// Drops the picture, keeping the room for the next one.
    pub fn clear(&mut self) {
        // SAFETY: the frame is valid.
        unsafe { sys::cff_frame_unref(self.0.as_ptr()) };
    }

    fn as_ptr(&self) -> *mut sys::cff_frame {
        self.0.as_ptr()
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_frame_free(self.0.as_ptr()) };
    }
}

/// An encoded access unit, or the room for one.
pub struct Packet(NonNull<sys::cff_packet>);

// SAFETY: see `Device`.
unsafe impl Send for Packet {}

/// What is in a [`Packet`], borrowed from it.
#[derive(Clone, Copy, Debug)]
pub struct PacketInfo<'a> {
    pub data: &'a [u8],
    pub pts: i64,
    pub dts: i64,
    pub keyframe: bool,
}

impl Packet {
    pub fn new() -> Result<Self, Error> {
        // SAFETY: allocates a fresh packet or reports why not.
        NonNull::new(unsafe { sys::cff_packet_alloc() })
            .map(Self)
            .ok_or_else(last_error)
    }

    pub fn info(&self) -> PacketInfo<'_> {
        let mut info = sys::cff_packet_info {
            data: null(),
            size: 0,
            pts: NO_TIMESTAMP,
            dts: NO_TIMESTAMP,
            duration: 0,
            keyframe: 0,
        };
        // SAFETY: the packet is valid and the info struct is ours to fill.
        unsafe { sys::cff_packet_get_info(self.0.as_ptr(), &mut info) };
        let data = if info.data.is_null() || info.size == 0 {
            &[][..]
        } else {
            // SAFETY: the packet owns `size` bytes at `data` for as long as it
            // is not cleared, which the borrow of `self` rules out.
            unsafe { std::slice::from_raw_parts(info.data, info.size) }
        };
        PacketInfo {
            data,
            pts: info.pts,
            dts: info.dts,
            keyframe: info.keyframe != 0,
        }
    }

    pub fn clear(&mut self) {
        // SAFETY: the packet is valid.
        unsafe { sys::cff_packet_unref(self.0.as_ptr()) };
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_packet_free(self.0.as_ptr()) };
    }
}

pub struct Decoder(NonNull<sys::cff_decoder>);

// SAFETY: see `Device`.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn open(codec: &str, device: Option<&Device>, options: &str) -> Result<Self, Error> {
        let codec = c_string(codec);
        let options = c_string(options);
        // SAFETY: valid C strings and an optional, valid device.
        let decoder =
            unsafe { sys::cff_decoder_open(codec.as_ptr(), device_ptr(device), options.as_ptr()) };
        NonNull::new(decoder).map(Self).ok_or_else(last_error)
    }

    /// Feeds one access unit; `Status::Again` means output has to be taken
    /// out first.
    pub fn send(&mut self, data: &[u8], pts: i64, dts: i64) -> Result<Status, Error> {
        // SAFETY: the decoder is valid and the slice is copied before the call
        // returns.
        status(unsafe {
            sys::cff_decoder_send(self.0.as_ptr(), data.as_ptr(), data.len(), pts, dts)
        })
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        // SAFETY: a null data pointer is the shim's flush.
        status(unsafe { sys::cff_decoder_send(self.0.as_ptr(), null(), 0, 0, 0) }).map(|_| ())
    }

    pub fn receive(&mut self, frame: &mut Frame) -> Result<Status, Error> {
        // SAFETY: both are valid.
        status(unsafe { sys::cff_decoder_receive(self.0.as_ptr(), frame.as_ptr()) })
    }

    /// The frame rate the stream declares, once decoding has started.
    pub fn frame_rate(&self) -> Option<Rational> {
        let (mut num, mut den) = (0, 1);
        // SAFETY: the decoder is valid and the outputs are ours.
        unsafe { sys::cff_decoder_get_frame_rate(self.0.as_ptr(), &mut num, &mut den) };
        let rate = Rational { num, den };
        rate.is_positive().then_some(rate)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_decoder_free(self.0.as_ptr()) };
    }
}

pub struct Filter(NonNull<sys::cff_filter>);

// SAFETY: see `Device`.
unsafe impl Send for Filter {}

impl Filter {
    /// Builds the graph described in FFmpeg's syntax for pictures like `first`.
    pub fn open(
        description: &str,
        first: &Frame,
        frame_rate: Option<Rational>,
        device: Option<&Device>,
    ) -> Result<Self, Error> {
        let description = c_string(description);
        let frame_rate = frame_rate.unwrap_or(Rational { num: 0, den: 1 });
        // SAFETY: a valid C string, frame and optional device.
        let filter = unsafe {
            sys::cff_filter_open(
                description.as_ptr(),
                first.as_ptr(),
                frame_rate.num,
                frame_rate.den,
                device_ptr(device),
            )
        };
        NonNull::new(filter).map(Self).ok_or_else(last_error)
    }

    pub fn send(&mut self, frame: &Frame) -> Result<Status, Error> {
        // SAFETY: both are valid; the shim leaves the frame intact.
        status(unsafe { sys::cff_filter_send(self.0.as_ptr(), frame.as_ptr()) })
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        // SAFETY: a null frame is the shim's flush.
        status(unsafe { sys::cff_filter_send(self.0.as_ptr(), null_mut()) }).map(|_| ())
    }

    pub fn receive(&mut self, frame: &mut Frame) -> Result<Status, Error> {
        // SAFETY: both are valid.
        status(unsafe { sys::cff_filter_receive(self.0.as_ptr(), frame.as_ptr()) })
    }

    /// The clock the output pictures are timed in, and their frame rate if known.
    pub fn output(&self) -> (Rational, Option<Rational>) {
        let (mut tb_num, mut tb_den, mut fr_num, mut fr_den) = (0, 1, 0, 1);
        // SAFETY: the filter is valid and the outputs are ours.
        unsafe {
            sys::cff_filter_get_output(
                self.0.as_ptr(),
                &mut tb_num,
                &mut tb_den,
                &mut fr_num,
                &mut fr_den,
            )
        };
        let time_base = Rational {
            num: tb_num,
            den: tb_den,
        };
        let frame_rate = Rational {
            num: fr_num,
            den: fr_den,
        };
        (
            if time_base.is_positive() {
                time_base
            } else {
                TIME_BASE
            },
            frame_rate.is_positive().then_some(frame_rate),
        )
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_filter_free(self.0.as_ptr()) };
    }
}

/// How an encoder is set up for the pictures it is about to get.
#[derive(Clone, Debug)]
pub struct EncoderParams {
    /// The clock the pictures are timed in.
    pub time_base: Rational,
    pub frame_rate: Option<Rational>,
    /// Bits per second; `None` leaves the rate control to the encoder.
    pub bit_rate: Option<u32>,
    /// Frames between keyframes; `None` leaves it to the encoder.
    pub gop_size: Option<u32>,
    /// `key=value` pairs separated by colons.
    pub options: String,
}

pub struct Encoder(NonNull<sys::cff_encoder>);

// SAFETY: see `Device`.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Opens and closes the encoder with nominal parameters, to tell whether
    /// it works here at all.
    pub fn probe(codec: &str, device: Option<&Device>, options: &str) -> Result<(), Error> {
        let codec = c_string(codec);
        let options = c_string(options);
        // SAFETY: valid C strings and an optional, valid device.
        status(unsafe {
            sys::cff_encoder_probe(codec.as_ptr(), device_ptr(device), options.as_ptr())
        })
        .map(|_| ())
    }

    pub fn open(codec: &str, first: &Frame, params: &EncoderParams) -> Result<Self, Error> {
        let codec = c_string(codec);
        let options = c_string(&params.options);
        let frame_rate = params.frame_rate.unwrap_or(Rational { num: 0, den: 1 });
        let raw = sys::cff_encoder_params {
            time_base_num: params.time_base.num,
            time_base_den: params.time_base.den,
            frame_rate_num: frame_rate.num,
            frame_rate_den: frame_rate.den,
            bit_rate: params.bit_rate.map_or(0, i64::from),
            gop_size: params
                .gop_size
                .and_then(|gop| c_int::try_from(gop).ok())
                .unwrap_or(0),
            options: options.as_ptr(),
        };
        // SAFETY: valid C strings, frame and params for the duration of the call.
        let encoder = unsafe { sys::cff_encoder_open(codec.as_ptr(), first.as_ptr(), &raw) };
        NonNull::new(encoder).map(Self).ok_or_else(last_error)
    }

    pub fn send(&mut self, frame: &Frame) -> Result<Status, Error> {
        // SAFETY: both are valid; the shim leaves the frame intact.
        status(unsafe { sys::cff_encoder_send(self.0.as_ptr(), frame.as_ptr()) })
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        // SAFETY: a null frame is the shim's flush.
        status(unsafe { sys::cff_encoder_send(self.0.as_ptr(), null_mut()) }).map(|_| ())
    }

    pub fn receive(&mut self, packet: &mut Packet) -> Result<Status, Error> {
        // SAFETY: both are valid.
        status(unsafe { sys::cff_encoder_receive(self.0.as_ptr(), packet.0.as_ptr()) })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the shim and is freed once.
        unsafe { sys::cff_encoder_free(self.0.as_ptr()) };
    }
}
