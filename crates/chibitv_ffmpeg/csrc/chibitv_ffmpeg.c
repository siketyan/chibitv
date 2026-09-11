#include "chibitv_ffmpeg.h"

#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavutil/avutil.h>
#include <libavutil/dict.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/log.h>
#include <libavutil/mathematics.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <libavutil/pixfmt.h>
#include <libavutil/rational.h>

#if defined(_MSC_VER)
#define CFF_THREAD_LOCAL __declspec(thread)
#else
#define CFF_THREAD_LOCAL _Thread_local
#endif

/* The clock every timestamp crossing the API is in. */
static const AVRational CFF_TIME_BASE = {1, 90000};

/* The nominal picture an encoder is probed with. */
#define PROBE_WIDTH 1920
#define PROBE_HEIGHT 1080

static CFF_THREAD_LOCAL char last_error[512];

struct cff_device {
    AVBufferRef *ref;
    enum AVHWDeviceType type;
};

struct cff_decoder {
    AVCodecContext *context;
    /* The pixel format of the device the decoder outputs to, or AV_PIX_FMT_NONE. */
    enum AVPixelFormat hardware_format;
};

struct cff_filter {
    AVFilterGraph *graph;
    AVFilterContext *source;
    AVFilterContext *sink;
};

struct cff_encoder {
    AVCodecContext *context;
};

/* Errors */

static int fail(const char *operation, int error) {
    char detail[AV_ERROR_MAX_STRING_SIZE];
    av_strerror(error, detail, sizeof(detail));
    snprintf(last_error, sizeof(last_error), "%s: %s", operation, detail);
    return CFF_ERROR;
}

static int fail_message(const char *format, ...) {
    va_list args;
    va_start(args, format);
    vsnprintf(last_error, sizeof(last_error), format, args);
    va_end(args);
    return CFF_ERROR;
}

static int fail_memory(void) {
    return fail_message("Out of memory");
}

const char *cff_last_error(void) {
    return last_error;
}

/* Turns a send/receive result into the values of the API. */
static int status(const char *operation, int result) {
    if (result >= 0) {
        return CFF_OK;
    }
    if (result == AVERROR(EAGAIN)) {
        return CFF_AGAIN;
    }
    if (result == AVERROR_EOF) {
        return CFF_EOF;
    }
    return fail(operation, result);
}

/* Logging */

static cff_log_callback log_callback;

static void log_bridge(void *avcl, int level, const char *format, va_list args) {
    if (!log_callback || level > AV_LOG_VERBOSE) {
        return;
    }

    char line[1024];
    int print_prefix = 1;
    av_log_format_line2(avcl, level, format, args, line, sizeof(line), &print_prefix);

    size_t length = strlen(line);
    while (length > 0 && (line[length - 1] == '\n' || line[length - 1] == '\r')) {
        line[--length] = '\0';
    }
    if (length == 0) {
        return;
    }

    int mapped;
    if (level <= AV_LOG_ERROR) {
        mapped = CFF_LOG_ERROR;
    } else if (level <= AV_LOG_WARNING) {
        mapped = CFF_LOG_WARNING;
    } else if (level <= AV_LOG_INFO) {
        mapped = CFF_LOG_INFO;
    } else {
        mapped = CFF_LOG_DEBUG;
    }
    log_callback(mapped, line);
}

void cff_set_log_callback(cff_log_callback callback) {
    log_callback = callback;
    av_log_set_level(AV_LOG_VERBOSE);
    av_log_set_callback(log_bridge);
}

const char *cff_version(void) {
    return av_version_info();
}

int cff_has_codec(const char *name, int encoder) {
    return (encoder ? avcodec_find_encoder_by_name(name) : avcodec_find_decoder_by_name(name)) != NULL;
}

/* Options */

static int parse_options(const char *options, AVDictionary **dictionary) {
    if (!options || !*options) {
        return CFF_OK;
    }

    const int result = av_dict_parse_string(dictionary, options, "=", ":", 0);
    if (result < 0) {
        av_dict_free(dictionary);
        return fail(options, result);
    }
    return CFF_OK;
}

/* Options nobody consumed are misspelt or unsupported: report them but carry on. */
static void warn_unused_options(const char *codec, AVDictionary *dictionary) {
    const AVDictionaryEntry *entry = NULL;
    while ((entry = av_dict_iterate(dictionary, entry))) {
        av_log(NULL, AV_LOG_WARNING, "%s ignored the option %s=%s\n", codec, entry->key, entry->value);
    }
}

/* Devices */

cff_device *cff_device_create(const char *type_name, const char *device) {
    const enum AVHWDeviceType type = av_hwdevice_find_type_by_name(type_name);
    if (type == AV_HWDEVICE_TYPE_NONE) {
        fail_message("This build has no %s device support", type_name);
        return NULL;
    }

    cff_device *created = av_mallocz(sizeof(*created));
    if (!created) {
        fail_memory();
        return NULL;
    }
    created->type = type;

    const int result = av_hwdevice_ctx_create(&created->ref, type, device && *device ? device : NULL, NULL, 0);
    if (result < 0) {
        fail(type_name, result);
        av_free(created);
        return NULL;
    }
    return created;
}

void cff_device_free(cff_device *device) {
    if (!device) {
        return;
    }
    av_buffer_unref(&device->ref);
    av_free(device);
}

/*
 * Finds the pixel format a codec exchanges with a device of the given type,
 * whichever way the codec uses the device.
 */
static enum AVPixelFormat device_pixel_format(const AVCodec *codec, const cff_device *device) {
    for (int i = 0;; i++) {
        const AVCodecHWConfig *config = avcodec_get_hw_config(codec, i);
        if (!config) {
            return AV_PIX_FMT_NONE;
        }
        if (config->device_type == device->type) {
            return config->pix_fmt;
        }
    }
}

/* Frames */

static enum AVPixelFormat software_format(const AVFrame *frame) {
    if (frame->hw_frames_ctx) {
        return ((const AVHWFramesContext *)frame->hw_frames_ctx->data)->sw_format;
    }
    return frame->format;
}

cff_frame *cff_frame_alloc(void) {
    AVFrame *frame = av_frame_alloc();
    if (!frame) {
        fail_memory();
    }
    return (cff_frame *)frame;
}

void cff_frame_get_info(const cff_frame *frame, cff_frame_info *info) {
    const AVFrame *av = (const AVFrame *)frame;
    const AVPixFmtDescriptor *descriptor = av_pix_fmt_desc_get(software_format(av));

    info->width = av->width;
    info->height = av->height;
    info->hardware = av->hw_frames_ctx != NULL;
    info->bit_depth = descriptor ? descriptor->comp[0].depth : 0;
    info->interlaced = (av->flags & AV_FRAME_FLAG_INTERLACED) != 0;
    info->pts = av->pts;
    info->duration = av->duration;
}

void cff_frame_unref(cff_frame *frame) {
    av_frame_unref((AVFrame *)frame);
}

void cff_frame_free(cff_frame *frame) {
    AVFrame *av = (AVFrame *)frame;
    av_frame_free(&av);
}

cff_frame *cff_frame_alloc_picture(const char *pixel_format, int width, int height) {
    const enum AVPixelFormat format = av_get_pix_fmt(pixel_format);
    if (format == AV_PIX_FMT_NONE) {
        fail_message("Unknown pixel format %s", pixel_format);
        return NULL;
    }

    AVFrame *frame = av_frame_alloc();
    if (!frame) {
        fail_memory();
        return NULL;
    }
    frame->format = format;
    frame->width = width;
    frame->height = height;

    const int result = av_frame_get_buffer(frame, 0);
    if (result < 0) {
        fail("av_frame_get_buffer", result);
        av_frame_free(&frame);
        return NULL;
    }
    return (cff_frame *)frame;
}

uint8_t *cff_frame_plane(cff_frame *frame, int plane, int *linesize) {
    AVFrame *av = (AVFrame *)frame;
    if (plane < 0 || plane >= AV_NUM_DATA_POINTERS || !av->data[plane]) {
        return NULL;
    }
    *linesize = av->linesize[plane];
    return av->data[plane];
}

void cff_frame_set_timing(cff_frame *frame, int64_t pts, int64_t duration) {
    AVFrame *av = (AVFrame *)frame;
    av->pts = pts;
    av->duration = duration;
}

void cff_frame_set_interlaced(cff_frame *frame, int top_field_first) {
    AVFrame *av = (AVFrame *)frame;
    av->flags |= AV_FRAME_FLAG_INTERLACED;
    if (top_field_first) {
        av->flags |= AV_FRAME_FLAG_TOP_FIELD_FIRST;
    } else {
        av->flags &= ~AV_FRAME_FLAG_TOP_FIELD_FIRST;
    }
}

/* Packets */

cff_packet *cff_packet_alloc(void) {
    AVPacket *packet = av_packet_alloc();
    if (!packet) {
        fail_memory();
    }
    return (cff_packet *)packet;
}

void cff_packet_get_info(const cff_packet *packet, cff_packet_info *info) {
    const AVPacket *av = (const AVPacket *)packet;
    info->data = av->data;
    info->size = av->size < 0 ? 0 : (size_t)av->size;
    info->pts = av->pts;
    info->dts = av->dts;
    info->duration = av->duration;
    info->keyframe = (av->flags & AV_PKT_FLAG_KEY) != 0;
}

void cff_packet_unref(cff_packet *packet) {
    av_packet_unref((AVPacket *)packet);
}

void cff_packet_free(cff_packet *packet) {
    AVPacket *av = (AVPacket *)packet;
    av_packet_free(&av);
}

/* Decoders */

/*
 * Picks the device's pixel format among those the decoder offers. Falling back
 * to a software format here would hand software pictures to a graph and an
 * encoder set up for the device, so a decoder that cannot use the device fails
 * instead.
 */
static enum AVPixelFormat choose_hardware_format(AVCodecContext *context, const enum AVPixelFormat *formats) {
    const cff_decoder *decoder = context->opaque;
    for (const enum AVPixelFormat *format = formats; *format != AV_PIX_FMT_NONE; format++) {
        if (*format == decoder->hardware_format) {
            return *format;
        }
    }

    av_log(context, AV_LOG_ERROR, "The decoder cannot output to the device\n");
    return AV_PIX_FMT_NONE;
}

cff_decoder *cff_decoder_open(const char *codec_name, cff_device *device, const char *options) {
    const AVCodec *codec = avcodec_find_decoder_by_name(codec_name);
    if (!codec) {
        fail_message("This build has no %s decoder", codec_name);
        return NULL;
    }

    cff_decoder *decoder = av_mallocz(sizeof(*decoder));
    AVCodecContext *context = avcodec_alloc_context3(codec);
    if (!decoder || !context) {
        fail_memory();
        avcodec_free_context(&context);
        av_free(decoder);
        return NULL;
    }
    decoder->context = context;
    decoder->hardware_format = AV_PIX_FMT_NONE;

    context->pkt_timebase = CFF_TIME_BASE;
    context->opaque = decoder;

    if (device) {
        decoder->hardware_format = device_pixel_format(codec, device);
        if (decoder->hardware_format == AV_PIX_FMT_NONE) {
            fail_message("The %s decoder cannot use the device", codec_name);
            cff_decoder_free(decoder);
            return NULL;
        }
        context->hw_device_ctx = av_buffer_ref(device->ref);
        context->get_format = choose_hardware_format;
    }

    AVDictionary *dictionary = NULL;
    if (parse_options(options, &dictionary) < 0) {
        cff_decoder_free(decoder);
        return NULL;
    }

    const int result = avcodec_open2(context, codec, &dictionary);
    warn_unused_options(codec_name, dictionary);
    av_dict_free(&dictionary);
    if (result < 0) {
        fail(codec_name, result);
        cff_decoder_free(decoder);
        return NULL;
    }
    return decoder;
}

int cff_decoder_send(cff_decoder *decoder, const uint8_t *data, size_t size, int64_t pts, int64_t dts) {
    if (!data) {
        const int result = avcodec_send_packet(decoder->context, NULL);
        return result == AVERROR_EOF ? CFF_OK : status("avcodec_send_packet", result);
    }
    if (size == 0 || size > INT_MAX) {
        return fail_message("An access unit of %zu bytes cannot be decoded", size);
    }

    AVPacket *packet = av_packet_alloc();
    if (!packet) {
        return fail_memory();
    }

    int result = av_new_packet(packet, (int)size);
    if (result < 0) {
        av_packet_free(&packet);
        return fail("av_new_packet", result);
    }
    memcpy(packet->data, data, size);
    packet->pts = pts;
    packet->dts = dts;

    result = avcodec_send_packet(decoder->context, packet);
    av_packet_free(&packet);
    return status("avcodec_send_packet", result);
}

int cff_decoder_receive(cff_decoder *decoder, cff_frame *frame) {
    AVFrame *av = (AVFrame *)frame;
    const int result = status("avcodec_receive_frame", avcodec_receive_frame(decoder->context, av));
    if (result == CFF_OK && av->pts == AV_NOPTS_VALUE) {
        av->pts = av->best_effort_timestamp;
    }
    return result;
}

void cff_decoder_get_frame_rate(const cff_decoder *decoder, int *num, int *den) {
    const AVRational rate = decoder->context->framerate;
    if (rate.num > 0 && rate.den > 0) {
        *num = rate.num;
        *den = rate.den;
    } else {
        *num = 0;
        *den = 1;
    }
}

void cff_decoder_free(cff_decoder *decoder) {
    if (!decoder) {
        return;
    }
    avcodec_free_context(&decoder->context);
    av_free(decoder);
}

/* Filters */

cff_filter *cff_filter_open(
    const char *description,
    const cff_frame *first,
    int frame_rate_num,
    int frame_rate_den,
    cff_device *device
) {
    const AVFrame *frame = (const AVFrame *)first;
    cff_filter *filter = av_mallocz(sizeof(*filter));
    AVBufferSrcParameters *parameters = av_buffersrc_parameters_alloc();
    AVFilterInOut *inputs = avfilter_inout_alloc();
    AVFilterInOut *outputs = avfilter_inout_alloc();
    if (!filter || !parameters || !inputs || !outputs) {
        fail_memory();
        goto fail;
    }

    filter->graph = avfilter_graph_alloc();
    if (!filter->graph) {
        fail_memory();
        goto fail;
    }

    filter->source = avfilter_graph_alloc_filter(filter->graph, avfilter_get_by_name("buffer"), "in");
    filter->sink = avfilter_graph_alloc_filter(filter->graph, avfilter_get_by_name("buffersink"), "out");
    if (!filter->source || !filter->sink) {
        fail_message("This build has no buffer source and sink filters");
        goto fail;
    }

    parameters->format = frame->format;
    parameters->width = frame->width;
    parameters->height = frame->height;
    parameters->sample_aspect_ratio = frame->sample_aspect_ratio;
    parameters->time_base = CFF_TIME_BASE;
    parameters->frame_rate = (AVRational){frame_rate_num, frame_rate_den};
    parameters->color_space = frame->colorspace;
    parameters->color_range = frame->color_range;
    parameters->hw_frames_ctx = frame->hw_frames_ctx;

    int result = av_buffersrc_parameters_set(filter->source, parameters);
    if (result >= 0) {
        result = avfilter_init_str(filter->source, NULL);
    }
    if (result >= 0) {
        result = avfilter_init_str(filter->sink, NULL);
    }
    if (result < 0) {
        fail("buffer source and sink", result);
        goto fail;
    }

    /* The description's unlabelled ends are the source's output and the sink's input. */
    outputs->name = av_strdup("in");
    outputs->filter_ctx = filter->source;
    inputs->name = av_strdup("out");
    inputs->filter_ctx = filter->sink;
    if (!outputs->name || !inputs->name) {
        fail_memory();
        goto fail;
    }

    result = avfilter_graph_parse_ptr(filter->graph, description, &inputs, &outputs, NULL);
    if (result < 0) {
        fail(description, result);
        goto fail;
    }

    /* Filters that upload pictures find the device here. */
    if (device) {
        for (unsigned i = 0; i < filter->graph->nb_filters; i++) {
            AVFilterContext *context = filter->graph->filters[i];
            if (!context->hw_device_ctx) {
                context->hw_device_ctx = av_buffer_ref(device->ref);
            }
        }
    }

    result = avfilter_graph_config(filter->graph, NULL);
    if (result < 0) {
        fail(description, result);
        goto fail;
    }

    av_free(parameters);
    avfilter_inout_free(&inputs);
    avfilter_inout_free(&outputs);
    return filter;

fail:
    av_free(parameters);
    avfilter_inout_free(&inputs);
    avfilter_inout_free(&outputs);
    cff_filter_free(filter);
    return NULL;
}

int cff_filter_send(cff_filter *filter, cff_frame *frame) {
    const int result = av_buffersrc_add_frame_flags(filter->source, (AVFrame *)frame, AV_BUFFERSRC_FLAG_KEEP_REF);
    return status("av_buffersrc_add_frame", result);
}

int cff_filter_receive(cff_filter *filter, cff_frame *frame) {
    return status("av_buffersink_get_frame", av_buffersink_get_frame(filter->sink, (AVFrame *)frame));
}

void cff_filter_get_output(
    const cff_filter *filter,
    int *time_base_num,
    int *time_base_den,
    int *frame_rate_num,
    int *frame_rate_den
) {
    const AVRational time_base = av_buffersink_get_time_base(filter->sink);
    const AVRational frame_rate = av_buffersink_get_frame_rate(filter->sink);
    *time_base_num = time_base.num;
    *time_base_den = time_base.den;
    if (frame_rate.num > 0 && frame_rate.den > 0) {
        *frame_rate_num = frame_rate.num;
        *frame_rate_den = frame_rate.den;
    } else {
        *frame_rate_num = 0;
        *frame_rate_den = 1;
    }
}

void cff_filter_free(cff_filter *filter) {
    if (!filter) {
        return;
    }
    avfilter_graph_free(&filter->graph);
    av_free(filter);
}

/* Encoders */

static int open_encoder(AVCodecContext *context, const AVCodec *codec, const char *options) {
    AVDictionary *dictionary = NULL;
    if (parse_options(options, &dictionary) < 0) {
        return CFF_ERROR;
    }

    const int result = avcodec_open2(context, codec, &dictionary);
    warn_unused_options(codec->name, dictionary);
    av_dict_free(&dictionary);
    return result < 0 ? fail(codec->name, result) : CFF_OK;
}

int cff_encoder_probe(const char *codec_name, cff_device *device, const char *options) {
    const AVCodec *codec = avcodec_find_encoder_by_name(codec_name);
    if (!codec) {
        return fail_message("This build has no %s encoder", codec_name);
    }

    AVCodecContext *context = avcodec_alloc_context3(codec);
    if (!context) {
        return fail_memory();
    }
    context->width = PROBE_WIDTH;
    context->height = PROBE_HEIGHT;
    context->time_base = CFF_TIME_BASE;
    context->framerate = (AVRational){30000, 1001};

    int result = CFF_OK;
    if (device) {
        const enum AVPixelFormat format = device_pixel_format(codec, device);
        if (format == AV_PIX_FMT_NONE) {
            result = fail_message("The %s encoder cannot use the device", codec_name);
            goto done;
        }

        /* Encoders take the device from the pool their input pictures come from. */
        AVBufferRef *frames = av_hwframe_ctx_alloc(device->ref);
        if (!frames) {
            result = fail_memory();
            goto done;
        }
        AVHWFramesContext *pool = (AVHWFramesContext *)frames->data;
        pool->format = format;
        pool->sw_format = AV_PIX_FMT_NV12;
        pool->width = PROBE_WIDTH;
        pool->height = PROBE_HEIGHT;
        pool->initial_pool_size = 4;

        const int init = av_hwframe_ctx_init(frames);
        if (init < 0) {
            result = fail("av_hwframe_ctx_init", init);
            av_buffer_unref(&frames);
            goto done;
        }
        context->hw_frames_ctx = frames;
        context->pix_fmt = format;
    } else {
        const enum AVPixelFormat *formats = NULL;
        int count = 0;
        avcodec_get_supported_config(context, codec, AV_CODEC_CONFIG_PIX_FORMAT, 0, (const void **)&formats, &count);
        context->pix_fmt = formats && count > 0 ? formats[0] : AV_PIX_FMT_YUV420P;
    }

    result = open_encoder(context, codec, options);

done:
    avcodec_free_context(&context);
    return result;
}

cff_encoder *cff_encoder_open(const char *codec_name, const cff_frame *first, const cff_encoder_params *params) {
    const AVFrame *frame = (const AVFrame *)first;
    const AVCodec *codec = avcodec_find_encoder_by_name(codec_name);
    if (!codec) {
        fail_message("This build has no %s encoder", codec_name);
        return NULL;
    }

    cff_encoder *encoder = av_mallocz(sizeof(*encoder));
    AVCodecContext *context = avcodec_alloc_context3(codec);
    if (!encoder || !context) {
        fail_memory();
        avcodec_free_context(&context);
        av_free(encoder);
        return NULL;
    }
    encoder->context = context;

    context->width = frame->width;
    context->height = frame->height;
    context->pix_fmt = frame->format;
    context->sample_aspect_ratio = frame->sample_aspect_ratio;
    context->colorspace = frame->colorspace;
    context->color_primaries = frame->color_primaries;
    context->color_trc = frame->color_trc;
    context->color_range = frame->color_range;
    context->chroma_sample_location = frame->chroma_location;
    context->time_base = (AVRational){params->time_base_num, params->time_base_den};
    context->framerate = (AVRational){params->frame_rate_num, params->frame_rate_den};
    context->bit_rate = params->bit_rate;
    if (params->gop_size > 0) {
        context->gop_size = params->gop_size;
    }
    if (frame->flags & AV_FRAME_FLAG_INTERLACED) {
        context->flags |= AV_CODEC_FLAG_INTERLACED_DCT | AV_CODEC_FLAG_INTERLACED_ME;
    }
    if (frame->hw_frames_ctx) {
        context->hw_frames_ctx = av_buffer_ref(frame->hw_frames_ctx);
        if (!context->hw_frames_ctx) {
            fail_memory();
            cff_encoder_free(encoder);
            return NULL;
        }
    }

    if (open_encoder(context, codec, params->options) < 0) {
        cff_encoder_free(encoder);
        return NULL;
    }
    return encoder;
}

int cff_encoder_send(cff_encoder *encoder, cff_frame *frame) {
    return status("avcodec_send_frame", avcodec_send_frame(encoder->context, (AVFrame *)frame));
}

int cff_encoder_receive(cff_encoder *encoder, cff_packet *packet) {
    AVPacket *av = (AVPacket *)packet;
    const int result = status("avcodec_receive_packet", avcodec_receive_packet(encoder->context, av));
    if (result == CFF_OK) {
        av_packet_rescale_ts(av, encoder->context->time_base, CFF_TIME_BASE);
    }
    return result;
}

void cff_encoder_free(cff_encoder *encoder) {
    if (!encoder) {
        return;
    }
    avcodec_free_context(&encoder->context);
    av_free(encoder);
}
