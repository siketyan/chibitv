//! The FFmpeg objects the pipeline is made of, on rsmpeg's wrappers.
//!
//! rsmpeg owns and frees FFmpeg's structures; what is here is the shape the
//! pipeline needs on top of that: a fixed clock, `Status` for the send and
//! receive calls, hardware device selection, and errors carrying FFmpeg's
//! description. Every timestamp here is in the 90 kHz clock, and
//! [`NO_TIMESTAMP`] stands for an unknown one.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr::{self, NonNull};
use std::sync::Once;

use rsmpeg::UnsafeDerefMut;
use rsmpeg::avcodec::{AVCodec, AVCodecContext, AVPacket};
use rsmpeg::avfilter::{AVFilter, AVFilterGraph, AVFilterInOut};
use rsmpeg::avutil::{AVDictionary, AVFrame, AVHWDeviceContext, hwdevice_find_type_by_name};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;

use crate::Error;

/// The unknown timestamp of FFmpeg's API.
pub const NO_TIMESTAMP: i64 = i64::MIN;

/// The clock every timestamp of this module is in.
pub const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};

/// The nominal picture an encoder is probed with.
const PROBE_WIDTH: i32 = 1920;
const PROBE_HEIGHT: i32 = 1080;

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

    fn from_ffi(rational: ffi::AVRational) -> Option<Self> {
        let rational = Self {
            num: rational.num,
            den: rational.den,
        };
        rational.is_positive().then_some(rational)
    }

    fn to_ffi(self) -> ffi::AVRational {
        ffi::AVRational {
            num: self.num,
            den: self.den,
        }
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

/// FFmpeg's description of an error code.
fn describe(code: c_int) -> String {
    let mut buffer = [0 as c_char; ffi::AV_ERROR_MAX_STRING_SIZE as usize];
    // SAFETY: the buffer is as long as told and gets NUL-terminated.
    unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    // SAFETY: NUL-terminated by the call above.
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn error(operation: &str, code: c_int) -> Error {
    Error::Codec(format!("{operation}: {}", describe(code)))
}

/// Turns the result of one of FFmpeg's send or receive calls into a `Status`.
fn status(operation: &str, code: c_int) -> Result<Status, Error> {
    if code >= 0 {
        Ok(Status::Ok)
    } else if code == ffi::AVERROR(ffi::EAGAIN) {
        Ok(Status::Again)
    } else if code == ffi::AVERROR_EOF {
        Ok(Status::Eof)
    } else {
        Err(error(operation, code))
    }
}

/// Turns one of rsmpeg's errors into ours, with FFmpeg's description.
fn rsmpeg_error(operation: &str, error: RsmpegError) -> Error {
    Error::Codec(match error {
        RsmpegError::AVError(code)
        | RsmpegError::SendPacketError(code)
        | RsmpegError::ReceiveFrameError(code)
        | RsmpegError::SendFrameError(code)
        | RsmpegError::ReceivePacketError(code)
        | RsmpegError::BufferSinkGetFrameError(code) => {
            format!("{operation}: {}", describe(code))
        }
        other => format!("{operation}: {other}"),
    })
}

fn c_string(value: &str) -> CString {
    CString::new(value).expect("an option or name with a NUL byte")
}

/// Routes FFmpeg's log through `tracing` once per process.
pub fn init_logging() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: the callback outlives the process and is safe to call from
        // any thread, as FFmpeg requires.
        unsafe {
            ffi::av_log_set_level(ffi::AV_LOG_VERBOSE as c_int);
            ffi::av_log_set_callback(Some(log_bridge));
        }
    });
}

/// The type bindgen gives a `va_list` parameter: the pointer an array of
/// `__va_list_tag` decays to on x86-64 (System V), the platform's `va_list`
/// itself elsewhere.
#[cfg(all(target_arch = "x86_64", not(windows)))]
type VaList = *mut ffi::__va_list_tag;
#[cfg(not(all(target_arch = "x86_64", not(windows))))]
type VaList = ffi::va_list;

unsafe extern "C" fn log_bridge(
    context: *mut c_void,
    level: c_int,
    format: *const c_char,
    arguments: VaList,
) {
    if level > ffi::AV_LOG_VERBOSE as c_int {
        return;
    }

    let mut line = [0 as c_char; 1024];
    let mut print_prefix: c_int = 1;
    // SAFETY: FFmpeg calls this with a valid context, format and argument
    // list, which are handed straight back to it along with a buffer of the
    // length told.
    unsafe {
        ffi::av_log_format_line2(
            context,
            level,
            format,
            arguments,
            line.as_mut_ptr(),
            line.len() as c_int,
            &mut print_prefix,
        );
    }
    // SAFETY: the buffer is NUL-terminated by the call above.
    let message = unsafe { CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
    let message = message.trim_end();
    if message.is_empty() {
        return;
    }

    if level <= ffi::AV_LOG_ERROR as c_int {
        tracing::error!(target: "ffmpeg", "{message}");
    } else if level <= ffi::AV_LOG_WARNING as c_int {
        tracing::warn!(target: "ffmpeg", "{message}");
    } else if level <= ffi::AV_LOG_INFO as c_int {
        tracing::info!(target: "ffmpeg", "{message}");
    } else {
        tracing::debug!(target: "ffmpeg", "{message}");
    }
}

/// The version of the FFmpeg linked in.
pub fn version() -> &'static str {
    // SAFETY: FFmpeg returns a static string.
    unsafe { CStr::from_ptr(ffi::av_version_info()) }
        .to_str()
        .unwrap_or("unknown")
}

pub fn has_decoder(name: &str) -> bool {
    AVCodec::find_decoder_by_name(&c_string(name)).is_some()
}

pub fn has_encoder(name: &str) -> bool {
    AVCodec::find_encoder_by_name(&c_string(name)).is_some()
}

/// Turns `key=value` pairs separated by colons into a dictionary; `None` for
/// no options at all.
fn options_dictionary(options: &str) -> Result<Option<AVDictionary>, Error> {
    if options.is_empty() {
        return Ok(None);
    }
    AVDictionary::from_string(&c_string(options), c"=", c":", 0)
        .map(Some)
        .ok_or_else(|| Error::Codec(format!("invalid options {options}")))
}

/// Options nobody consumed are misspelt or unsupported: report them but carry on.
fn warn_unused_options(codec: &str, unused: Option<AVDictionary>) {
    for entry in unused.iter().flat_map(|dictionary| dictionary.iter()) {
        tracing::warn!(
            target: "ffmpeg",
            "{codec} ignored the option {}={}",
            entry.key().to_string_lossy(),
            entry.value().to_string_lossy()
        );
    }
}

/// A hardware device that decoders, filters and encoders share pictures on.
pub struct Device {
    context: AVHWDeviceContext,
    kind: ffi::AVHWDeviceType,
}

impl Device {
    pub fn create(type_name: &str, device: Option<&str>) -> Result<Self, Error> {
        let kind = hwdevice_find_type_by_name(&c_string(type_name));
        if kind == ffi::AV_HWDEVICE_TYPE_NONE {
            return Err(Error::Codec(format!(
                "this build has no {type_name} device support"
            )));
        }
        let device = device.map(c_string);
        let context = AVHWDeviceContext::create(kind, device.as_deref(), None, 0)
            .map_err(|error| rsmpeg_error(type_name, error))?;
        Ok(Self { context, kind })
    }

    /// The pixel format a codec exchanges with this device, whichever way the
    /// codec uses it.
    fn pixel_format_for(&self, codec: &AVCodec) -> Option<ffi::AVPixelFormat> {
        (0..)
            .map_while(|index| codec.hw_config(index))
            .find(|config| config.device_type == self.kind)
            .map(|config| config.pix_fmt)
    }

    fn as_ptr(&self) -> *mut ffi::AVBufferRef {
        self.context.as_ptr() as *mut _
    }
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
pub struct Frame(AVFrame);

impl Frame {
    pub fn new() -> Result<Self, Error> {
        Ok(Self(AVFrame::new()))
    }

    /// A writable picture in the pixel format named the way FFmpeg does.
    #[cfg(test)]
    pub fn picture(pixel_format: &str, width: u32, height: u32) -> Result<Self, Error> {
        // SAFETY: a valid C string.
        let format = unsafe { ffi::av_get_pix_fmt(c_string(pixel_format).as_ptr()) };
        if format == ffi::AV_PIX_FMT_NONE {
            return Err(Error::Codec(format!("unknown pixel format {pixel_format}")));
        }
        let mut frame = AVFrame::new();
        frame.set_format(format);
        frame.set_width(width as i32);
        frame.set_height(height as i32);
        frame
            .alloc_buffer()
            .map_err(|error| rsmpeg_error("av_frame_get_buffer", error))?;
        Ok(Self(frame))
    }

    /// The bytes of one plane and its stride, of a picture made by [`Frame::picture`].
    #[cfg(test)]
    pub fn plane(&mut self, plane: usize) -> Option<(&mut [u8], usize)> {
        let data = self.0.data[plane];
        let linesize = self.0.linesize[plane];
        if data.is_null() || linesize <= 0 {
            return None;
        }
        let rows = usize::try_from(self.0.height).ok()?;
        let rows = if plane == 0 { rows } else { rows.div_ceil(2) };
        // SAFETY: the plane holds `linesize` bytes for each of its rows, and
        // the chroma planes of the formats the tests use are half as tall.
        let bytes = unsafe { std::slice::from_raw_parts_mut(data, linesize as usize * rows) };
        Some((bytes, linesize as usize))
    }

    pub fn set_timing(&mut self, pts: i64, duration: i64) {
        self.0.set_pts(pts);
        // SAFETY: a plain field of a frame this owns.
        unsafe { self.0.deref_mut().duration = duration };
    }

    #[cfg(test)]
    pub fn set_interlaced(&mut self, top_field_first: bool) {
        // SAFETY: a plain field of a frame this owns.
        let flags = unsafe { &mut self.0.deref_mut().flags };
        *flags |= ffi::AV_FRAME_FLAG_INTERLACED as c_int;
        if top_field_first {
            *flags |= ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST as c_int;
        } else {
            *flags &= !(ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST as c_int);
        }
    }

    /// The pixel format of the picture, or of the software picture behind a
    /// hardware one.
    fn software_format(&self) -> ffi::AVPixelFormat {
        if self.0.hw_frames_ctx.is_null() {
            return self.0.format;
        }
        // SAFETY: a frame's hardware frames context is a buffer whose data
        // is an `AVHWFramesContext`, kept alive by the frame.
        unsafe { (*((*self.0.hw_frames_ctx).data as *const ffi::AVHWFramesContext)).sw_format }
    }

    pub fn info(&self) -> FrameInfo {
        // SAFETY: returns a static descriptor, or null for an unknown format.
        let descriptor = unsafe { ffi::av_pix_fmt_desc_get(self.software_format()) };
        let bit_depth = if descriptor.is_null() {
            0
        } else {
            // SAFETY: checked non-null; descriptors are static.
            unsafe { (*descriptor).comp[0].depth }
        };
        FrameInfo {
            width: self.0.width.max(0) as u32,
            height: self.0.height.max(0) as u32,
            hardware: !self.0.hw_frames_ctx.is_null(),
            bit_depth: bit_depth.max(0) as u32,
            interlaced: self.0.flags & ffi::AV_FRAME_FLAG_INTERLACED as c_int != 0,
            pts: self.0.pts,
            duration: self.0.duration,
        }
    }

    /// Drops the picture, keeping the room for the next one.
    pub fn clear(&mut self) {
        // SAFETY: the frame is valid.
        unsafe { ffi::av_frame_unref(self.0.as_mut_ptr()) };
    }

    fn as_mut_ptr(&mut self) -> *mut ffi::AVFrame {
        self.0.as_mut_ptr()
    }
}

/// An encoded access unit, or the room for one.
pub struct Packet(AVPacket);

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
        Ok(Self(AVPacket::new()))
    }

    pub fn info(&self) -> PacketInfo<'_> {
        let data = if self.0.data.is_null() || self.0.size <= 0 {
            &[][..]
        } else {
            // SAFETY: the packet owns `size` bytes at `data` for as long as
            // it is not cleared, which the borrow of `self` rules out.
            unsafe { std::slice::from_raw_parts(self.0.data, self.0.size as usize) }
        };
        PacketInfo {
            data,
            pts: self.0.pts,
            dts: self.0.dts,
            keyframe: self.0.flags & ffi::AV_PKT_FLAG_KEY as c_int != 0,
        }
    }

    pub fn clear(&mut self) {
        // SAFETY: the packet is valid.
        unsafe { ffi::av_packet_unref(self.0.as_mut_ptr()) };
    }
}

pub struct Decoder {
    context: AVCodecContext,
    /// The pixel format of the device the decoder outputs to, which the
    /// `get_format` callback reads through the context's opaque pointer.
    hardware_format: Option<Box<ffi::AVPixelFormat>>,
}

/// Picks the device's pixel format among those the decoder offers. Falling
/// back to a software format here would hand software pictures to a graph and
/// an encoder set up for the device, so a decoder that cannot use the device
/// fails instead.
unsafe extern "C" fn choose_hardware_format(
    context: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: the opaque pointer is the decoder's boxed format, alive as
    // long as the context; the format list is NONE-terminated.
    unsafe {
        let wanted = *((*context).opaque as *const ffi::AVPixelFormat);
        let mut format = formats;
        while *format != ffi::AV_PIX_FMT_NONE {
            if *format == wanted {
                return wanted;
            }
            format = format.add(1);
        }
    }
    tracing::error!(target: "ffmpeg", "The decoder cannot output to the device");
    ffi::AV_PIX_FMT_NONE
}

impl Decoder {
    /// Opens a decoder. With a device, the decoder outputs pictures in device
    /// memory, either through a hardware acceleration of a software decoder
    /// or because the decoder itself is a hardware one.
    pub fn open(codec_name: &str, device: Option<&Device>, options: &str) -> Result<Self, Error> {
        let codec = AVCodec::find_decoder_by_name(&c_string(codec_name))
            .ok_or_else(|| Error::Codec(format!("this build has no {codec_name} decoder")))?;
        let mut context = AVCodecContext::new(&codec);
        context.set_pkt_timebase(TIME_BASE.to_ffi());

        let mut hardware_format = None;
        if let Some(device) = device {
            let format = device.pixel_format_for(&codec).ok_or_else(|| {
                Error::Codec(format!("the {codec_name} decoder cannot use the device"))
            })?;
            let format = Box::new(format);
            // SAFETY: plain fields of a context this owns; the box lives as
            // long as the context does.
            unsafe { context.deref_mut().opaque = &*format as *const _ as *mut c_void };
            context.set_hw_device_ctx(device.context.clone());
            context.set_get_format(Some(choose_hardware_format));
            hardware_format = Some(format);
        }

        let unused = context
            .open(options_dictionary(options)?)
            .map_err(|error| rsmpeg_error(codec_name, error))?;
        warn_unused_options(codec_name, unused);
        Ok(Self {
            context,
            hardware_format,
        })
    }

    /// Feeds one access unit; `Status::Again` means output has to be taken
    /// out first.
    pub fn send(&mut self, data: &[u8], pts: i64, dts: i64) -> Result<Status, Error> {
        let size = c_int::try_from(data.len())
            .ok()
            .filter(|size| *size > 0)
            .ok_or_else(|| {
                Error::Codec(format!(
                    "an access unit of {} bytes cannot be decoded",
                    data.len()
                ))
            })?;
        let mut packet = AVPacket::new();
        // SAFETY: allocates `size` bytes the slice is copied into.
        let allocated = unsafe { ffi::av_new_packet(packet.as_mut_ptr(), size) };
        if allocated < 0 {
            return Err(error("av_new_packet", allocated));
        }
        // SAFETY: the packet holds `size` bytes, as many as the slice.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), packet.data, data.len()) };
        packet.set_pts(pts);
        packet.set_dts(dts);

        match self.context.send_packet(Some(&packet)) {
            Ok(()) => Ok(Status::Ok),
            Err(RsmpegError::DecoderFullError) => Ok(Status::Again),
            Err(RsmpegError::DecoderFlushedError) => Ok(Status::Eof),
            Err(error) => Err(rsmpeg_error("avcodec_send_packet", error)),
        }
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        match self.context.send_packet(None) {
            Ok(()) | Err(RsmpegError::DecoderFlushedError) => Ok(()),
            Err(error) => Err(rsmpeg_error("avcodec_send_packet", error)),
        }
    }

    pub fn receive(&mut self, frame: &mut Frame) -> Result<Status, Error> {
        // SAFETY: both are valid.
        let code =
            unsafe { ffi::avcodec_receive_frame(self.context.as_mut_ptr(), frame.as_mut_ptr()) };
        let status = status("avcodec_receive_frame", code)?;
        if status == Status::Ok && frame.0.pts == NO_TIMESTAMP {
            frame.0.set_pts(frame.0.best_effort_timestamp);
        }
        Ok(status)
    }

    /// The frame rate the stream declares, once decoding has started.
    pub fn frame_rate(&self) -> Option<Rational> {
        Rational::from_ffi(self.context.framerate)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // The context, and with it any use of the opaque pointer, goes
        // before the box it points into.
        // SAFETY: nothing else points at the format.
        unsafe { self.context.deref_mut().opaque = ptr::null_mut() };
        self.hardware_format.take();
    }
}

pub struct Filter {
    graph: AVFilterGraph,
    /// Owned by the graph, which outlives them.
    source: NonNull<ffi::AVFilterContext>,
    sink: NonNull<ffi::AVFilterContext>,
}

// SAFETY: the graph owns the filter contexts, and rsmpeg's graph is `Send`.
unsafe impl Send for Filter {}

impl Filter {
    /// Builds the graph described in FFmpeg's syntax for pictures like
    /// `first`. The device is handed to filters that upload pictures to it.
    pub fn open(
        description: &str,
        first: &Frame,
        frame_rate: Option<Rational>,
        device: Option<&Device>,
    ) -> Result<Self, Error> {
        let graph = AVFilterGraph::new();
        // The ends are borrowed from the graph while it is put together and
        // kept as raw pointers once it is, so that the graph can move.
        let (source, sink) = Self::build(&graph, description, first, frame_rate, device)?;
        Ok(Self {
            graph,
            source,
            sink,
        })
    }

    fn build(
        graph: &AVFilterGraph,
        description: &str,
        first: &Frame,
        frame_rate: Option<Rational>,
        device: Option<&Device>,
    ) -> Result<(NonNull<ffi::AVFilterContext>, NonNull<ffi::AVFilterContext>), Error> {
        let buffer = AVFilter::get_by_name(c"buffer");
        let buffersink = AVFilter::get_by_name(c"buffersink");
        let (Some(buffer), Some(buffersink)) = (buffer, buffersink) else {
            return Err(Error::Codec(
                "this build has no buffer source and sink filters".into(),
            ));
        };
        let (Some(mut source), Some(mut sink)) = (
            graph.alloc_filter_context(&buffer, c"in"),
            graph.alloc_filter_context(&buffersink, c"out"),
        ) else {
            return Err(Error::Codec("could not allocate the graph's ends".into()));
        };

        // SAFETY: the parameters are filled from a valid frame and freed
        // after being applied; the source is not initialised yet.
        let applied = unsafe {
            let parameters = ffi::av_buffersrc_parameters_alloc();
            if parameters.is_null() {
                return Err(Error::Codec("out of memory".into()));
            }
            let frame = &*first.0;
            (*parameters).format = frame.format;
            (*parameters).width = frame.width;
            (*parameters).height = frame.height;
            (*parameters).sample_aspect_ratio = frame.sample_aspect_ratio;
            (*parameters).time_base = TIME_BASE.to_ffi();
            (*parameters).frame_rate = frame_rate.unwrap_or(Rational { num: 0, den: 1 }).to_ffi();
            (*parameters).color_space = frame.colorspace;
            (*parameters).color_range = frame.color_range;
            (*parameters).hw_frames_ctx = frame.hw_frames_ctx;
            let applied = ffi::av_buffersrc_parameters_set(source.as_mut_ptr(), parameters);
            ffi::av_free(parameters.cast());
            applied
        };
        if applied < 0 {
            return Err(error("av_buffersrc_parameters_set", applied));
        }
        source
            .init_str(None)
            .and_then(|()| sink.init_str(None))
            .map_err(|error| rsmpeg_error("buffer source and sink", error))?;

        // The description's unlabelled ends are the source's output and the
        // sink's input.
        let outputs = AVFilterInOut::new(c"in", &mut source, 0);
        let inputs = AVFilterInOut::new(c"out", &mut sink, 0);
        let source = NonNull::new(source.as_mut_ptr()).expect("allocated above");
        let sink = NonNull::new(sink.as_mut_ptr()).expect("allocated above");
        graph
            .parse_ptr(&c_string(description), Some(inputs), Some(outputs))
            .map_err(|error| rsmpeg_error(description, error))?;

        // Filters that upload pictures find the device here.
        if let Some(device) = device {
            // SAFETY: the graph's filter array is valid until it is freed,
            // and a new reference to the device is taken for each filter.
            unsafe {
                for index in 0..graph.nb_filters as usize {
                    let filter = *graph.filters.add(index);
                    if (*filter).hw_device_ctx.is_null() {
                        (*filter).hw_device_ctx = ffi::av_buffer_ref(device.as_ptr());
                    }
                }
            }
        }

        graph
            .config()
            .map_err(|error| rsmpeg_error(description, error))?;
        Ok((source, sink))
    }

    /// Sends one picture, leaving it intact.
    pub fn send(&mut self, frame: &Frame) -> Result<Status, Error> {
        // SAFETY: both are valid; the flag keeps the frame's references.
        let code = unsafe {
            ffi::av_buffersrc_add_frame_flags(
                self.source.as_ptr(),
                frame.0.as_ptr() as *mut _,
                ffi::AV_BUFFERSRC_FLAG_KEEP_REF as c_int,
            )
        };
        status("av_buffersrc_add_frame", code)
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        // SAFETY: a null frame marks the end of the input.
        let code =
            unsafe { ffi::av_buffersrc_add_frame_flags(self.source.as_ptr(), ptr::null_mut(), 0) };
        status("av_buffersrc_add_frame", code).map(|_| ())
    }

    pub fn receive(&mut self, frame: &mut Frame) -> Result<Status, Error> {
        // SAFETY: both are valid.
        let code = unsafe { ffi::av_buffersink_get_frame(self.sink.as_ptr(), frame.as_mut_ptr()) };
        status("av_buffersink_get_frame", code)
    }

    /// The clock the output pictures are timed in, and their frame rate if known.
    pub fn output(&self) -> (Rational, Option<Rational>) {
        // SAFETY: the sink is configured.
        let (time_base, frame_rate) = unsafe {
            (
                ffi::av_buffersink_get_time_base(self.sink.as_ptr()),
                ffi::av_buffersink_get_frame_rate(self.sink.as_ptr()),
            )
        };
        (
            Rational::from_ffi(time_base).unwrap_or(TIME_BASE),
            Rational::from_ffi(frame_rate),
        )
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        // Spelt out so that the raw ends are known to die with the graph.
        let _ = &self.graph;
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

pub struct Encoder {
    context: AVCodecContext,
}

impl Encoder {
    /// Opens and closes the encoder with nominal parameters, to tell whether
    /// it works here at all. With a device, it is tried on pictures in device
    /// memory.
    pub fn probe(codec_name: &str, device: Option<&Device>, options: &str) -> Result<(), Error> {
        let codec = AVCodec::find_encoder_by_name(&c_string(codec_name))
            .ok_or_else(|| Error::Codec(format!("this build has no {codec_name} encoder")))?;
        let mut context = AVCodecContext::new(&codec);
        context.set_width(PROBE_WIDTH);
        context.set_height(PROBE_HEIGHT);
        context.set_time_base(TIME_BASE.to_ffi());
        context.set_framerate(ffi::AVRational {
            num: 30_000,
            den: 1001,
        });

        if let Some(device) = device {
            let format = device.pixel_format_for(&codec).ok_or_else(|| {
                Error::Codec(format!("the {codec_name} encoder cannot use the device"))
            })?;
            // Encoders take the device from the pool their input pictures
            // come from.
            let mut frames = device.context.hwframe_ctx_alloc();
            let pool = frames.data();
            pool.format = format;
            pool.sw_format = ffi::AV_PIX_FMT_NV12;
            pool.width = PROBE_WIDTH;
            pool.height = PROBE_HEIGHT;
            pool.initial_pool_size = 4;
            frames
                .init()
                .map_err(|error| rsmpeg_error("av_hwframe_ctx_init", error))?;
            context.set_hw_frames_ctx(frames);
            context.set_pix_fmt(format);
        } else {
            let formats = context
                .get_supported_pix_fmts(Some(&codec))
                .map_err(|error| rsmpeg_error(codec_name, error))?;
            context.set_pix_fmt(formats.first().copied().unwrap_or(ffi::AV_PIX_FMT_YUV420P));
        }

        let unused = context
            .open(options_dictionary(options)?)
            .map_err(|error| rsmpeg_error(codec_name, error))?;
        warn_unused_options(codec_name, unused);
        Ok(())
    }

    /// Opens an encoder for pictures like `first`.
    pub fn open(codec_name: &str, first: &Frame, params: &EncoderParams) -> Result<Self, Error> {
        let codec = AVCodec::find_encoder_by_name(&c_string(codec_name))
            .ok_or_else(|| Error::Codec(format!("this build has no {codec_name} encoder")))?;
        let mut context = AVCodecContext::new(&codec);
        let frame = &*first.0;
        context.set_width(frame.width);
        context.set_height(frame.height);
        context.set_pix_fmt(frame.format);
        context.set_sample_aspect_ratio(frame.sample_aspect_ratio);
        context.set_time_base(params.time_base.to_ffi());
        context.set_framerate(
            params
                .frame_rate
                .unwrap_or(Rational { num: 0, den: 1 })
                .to_ffi(),
        );
        context.set_bit_rate(params.bit_rate.map_or(0, i64::from));
        if let Some(gop_size) = params.gop_size.and_then(|gop| c_int::try_from(gop).ok()) {
            context.set_gop_size(gop_size);
        }
        if frame.flags & ffi::AV_FRAME_FLAG_INTERLACED as c_int != 0 {
            context.set_flags(
                context.flags
                    | ffi::AV_CODEC_FLAG_INTERLACED_DCT as c_int
                    | ffi::AV_CODEC_FLAG_INTERLACED_ME as c_int,
            );
        }
        // SAFETY: plain fields of a context this owns, set before it opens.
        unsafe {
            let raw = context.deref_mut();
            raw.colorspace = frame.colorspace;
            raw.color_primaries = frame.color_primaries;
            raw.color_trc = frame.color_trc;
            raw.color_range = frame.color_range;
            raw.chroma_sample_location = frame.chroma_location;
            if !frame.hw_frames_ctx.is_null() {
                raw.hw_frames_ctx = ffi::av_buffer_ref(frame.hw_frames_ctx);
                if raw.hw_frames_ctx.is_null() {
                    return Err(Error::Codec("out of memory".into()));
                }
            }
        }

        let unused = context
            .open(options_dictionary(&params.options)?)
            .map_err(|error| rsmpeg_error(codec_name, error))?;
        warn_unused_options(codec_name, unused);
        Ok(Self { context })
    }

    /// Sends one picture, leaving it intact.
    pub fn send(&mut self, frame: &Frame) -> Result<Status, Error> {
        match self.context.send_frame(Some(&frame.0)) {
            Ok(()) => Ok(Status::Ok),
            Err(RsmpegError::SendFrameAgainError) => Ok(Status::Again),
            Err(RsmpegError::EncoderFlushedError) => Ok(Status::Eof),
            Err(error) => Err(rsmpeg_error("avcodec_send_frame", error)),
        }
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        match self.context.send_frame(None) {
            Ok(()) | Err(RsmpegError::EncoderFlushedError) => Ok(()),
            Err(error) => Err(rsmpeg_error("avcodec_send_frame", error)),
        }
    }

    pub fn receive(&mut self, packet: &mut Packet) -> Result<Status, Error> {
        // SAFETY: both are valid.
        let code = unsafe {
            ffi::avcodec_receive_packet(self.context.as_mut_ptr(), packet.0.as_mut_ptr())
        };
        let status = status("avcodec_receive_packet", code)?;
        if status == Status::Ok {
            packet
                .0
                .rescale_ts(self.context.time_base, TIME_BASE.to_ffi());
        }
        Ok(status)
    }
}
