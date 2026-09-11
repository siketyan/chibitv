/*
 * The C side of chibitv_ffmpeg: a flat API over the handful of FFmpeg calls the
 * crate needs, so that the Rust side binds to functions with stable signatures
 * instead of to FFmpeg's structures.
 *
 * Every timestamp crossing this API is in the 90 kHz clock of MPEG-2 Systems.
 * Errors are negative results; cff_last_error() describes the last one on the
 * calling thread.
 */
#ifndef CHIBITV_FFMPEG_H
#define CHIBITV_FFMPEG_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Results of the send and receive calls. */
#define CFF_OK 0
#define CFF_AGAIN 1 /* Needs more input before it can produce output. */
#define CFF_EOF 2   /* Nothing more will come out. */
#define CFF_ERROR (-1)

/* A timestamp that is not known. */
#define CFF_NO_TIMESTAMP INT64_MIN

typedef struct cff_device cff_device;
typedef struct cff_decoder cff_decoder;
typedef struct cff_filter cff_filter;
typedef struct cff_encoder cff_encoder;
typedef struct cff_frame cff_frame;
typedef struct cff_packet cff_packet;

/* Levels of cff_log_callback: error, warning, info, debug. */
#define CFF_LOG_ERROR 0
#define CFF_LOG_WARNING 1
#define CFF_LOG_INFO 2
#define CFF_LOG_DEBUG 3

typedef void (*cff_log_callback)(int level, const char *message);

const char *cff_version(void);
const char *cff_last_error(void);
/* Routes FFmpeg's log through the callback, which may be called on any thread. */
void cff_set_log_callback(cff_log_callback callback);
int cff_has_codec(const char *name, int encoder);

cff_device *cff_device_create(const char *type_name, const char *device);
void cff_device_free(cff_device *device);

typedef struct cff_frame_info {
    int width;
    int height;
    /* The picture lives in device memory. */
    int hardware;
    /* Bits per sample of the pictures, or of the software pictures behind hardware ones. */
    int bit_depth;
    int interlaced;
    int64_t pts;
    int64_t duration;
} cff_frame_info;

cff_frame *cff_frame_alloc(void);
void cff_frame_get_info(const cff_frame *frame, cff_frame_info *info);
void cff_frame_unref(cff_frame *frame);
void cff_frame_free(cff_frame *frame);

/* Makes a writable picture for feeding an encoder directly, as the tests do. */
cff_frame *cff_frame_alloc_picture(const char *pixel_format, int width, int height);
uint8_t *cff_frame_plane(cff_frame *frame, int plane, int *linesize);
void cff_frame_set_timing(cff_frame *frame, int64_t pts, int64_t duration);
void cff_frame_set_interlaced(cff_frame *frame, int top_field_first);

typedef struct cff_packet_info {
    const uint8_t *data;
    size_t size;
    int64_t pts;
    int64_t dts;
    int64_t duration;
    int keyframe;
} cff_packet_info;

cff_packet *cff_packet_alloc(void);
void cff_packet_get_info(const cff_packet *packet, cff_packet_info *info);
void cff_packet_unref(cff_packet *packet);
void cff_packet_free(cff_packet *packet);

/*
 * Opens a decoder. With a device, the decoder outputs pictures in device
 * memory, either through a hardware acceleration of a software decoder or
 * because the decoder itself is a hardware one. Options are `key=value` pairs
 * separated by colons.
 */
cff_decoder *cff_decoder_open(const char *codec_name, cff_device *device, const char *options);
/* Sends one access unit; a null data pointer flushes the decoder. */
int cff_decoder_send(cff_decoder *decoder, const uint8_t *data, size_t size, int64_t pts, int64_t dts);
int cff_decoder_receive(cff_decoder *decoder, cff_frame *frame);
/* The frame rate the stream declares, or 0/1 when it does not. */
void cff_decoder_get_frame_rate(const cff_decoder *decoder, int *num, int *den);
void cff_decoder_free(cff_decoder *decoder);

/*
 * Builds a filter graph from a description in FFmpeg's graph syntax, configured
 * for pictures like the first one. The device is handed to filters that upload
 * pictures to it.
 */
cff_filter *cff_filter_open(
    const char *description,
    const cff_frame *first,
    int frame_rate_num,
    int frame_rate_den,
    cff_device *device
);
/* Sends one picture; a null frame flushes the graph. The frame is left intact. */
int cff_filter_send(cff_filter *filter, cff_frame *frame);
int cff_filter_receive(cff_filter *filter, cff_frame *frame);
/* The clock the output pictures are timed in and their frame rate, or 0/1 when unknown. */
void cff_filter_get_output(
    const cff_filter *filter,
    int *time_base_num,
    int *time_base_den,
    int *frame_rate_num,
    int *frame_rate_den
);
void cff_filter_free(cff_filter *filter);

typedef struct cff_encoder_params {
    /* The clock the pictures fed to the encoder are timed in. */
    int time_base_num;
    int time_base_den;
    /* 0/1 when unknown. */
    int frame_rate_num;
    int frame_rate_den;
    /* 0 leaves the rate control to the encoder. */
    int64_t bit_rate;
    /* Frames between keyframes; 0 leaves it to the encoder. */
    int gop_size;
    /* `key=value` pairs separated by colons, or null. */
    const char *options;
} cff_encoder_params;

/*
 * Opens and closes an encoder with nominal parameters, to find out whether it
 * works on this machine before any picture is decoded. With a device, the
 * encoder is tried on pictures in device memory.
 */
int cff_encoder_probe(const char *codec_name, cff_device *device, const char *options);
/* Opens an encoder for pictures like the first one. */
cff_encoder *cff_encoder_open(const char *codec_name, const cff_frame *first, const cff_encoder_params *params);
/* Sends one picture; a null frame flushes the encoder. The frame is left intact. */
int cff_encoder_send(cff_encoder *encoder, cff_frame *frame);
int cff_encoder_receive(cff_encoder *encoder, cff_packet *packet);
void cff_encoder_free(cff_encoder *encoder);

#ifdef __cplusplus
}
#endif

#endif
