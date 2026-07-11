#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/mathematics.h>
#include <libavutil/pixfmt.h>
#include <libavutil/rational.h>

#include <emscripten/emscripten.h>

#define FRAME_QUEUE_CAPACITY 16
#define TIME_BASE_NUM 1
#define TIME_BASE_DEN 1000000

static AVCodecContext *decoder_context;
static AVFilterGraph *filter_graph;
static AVFilterContext *buffer_source;
static AVFilterContext *buffer_sink;
static enum AVColorSpace filter_color_space = AVCOL_SPC_UNSPECIFIED;
static enum AVColorRange filter_color_range = AVCOL_RANGE_UNSPECIFIED;
static AVFrame *decoded_frame;
static AVFrame *current_frame;
static AVFrame *frame_queue[FRAME_QUEUE_CAPACITY];
static int frame_queue_head;
static int frame_queue_length;
static int64_t fallback_duration;
static char last_error[AV_ERROR_MAX_STRING_SIZE + 96];

static int fail_ffmpeg(const char *operation, int error) {
    char detail[AV_ERROR_MAX_STRING_SIZE];
    av_strerror(error, detail, sizeof(detail));
    snprintf(last_error, sizeof(last_error), "%s: %s", operation, detail);
    return error < 0 ? error : AVERROR_UNKNOWN;
}

static int fail_message(const char *message) {
    snprintf(last_error, sizeof(last_error), "%s", message);
    return AVERROR(EINVAL);
}

static void clear_frame_queue(void) {
    for (int i = 0; i < frame_queue_length; i++) {
        const int index = (frame_queue_head + i) % FRAME_QUEUE_CAPACITY;
        av_frame_free(&frame_queue[index]);
    }
    frame_queue_head = 0;
    frame_queue_length = 0;
}

static int enqueue_frame(AVFrame *frame) {
    if (frame_queue_length == FRAME_QUEUE_CAPACITY) {
        av_frame_free(&frame);
        return fail_message("Decoded frame queue overflowed");
    }

    const int index = (frame_queue_head + frame_queue_length) % FRAME_QUEUE_CAPACITY;
    frame_queue[index] = frame;
    frame_queue_length++;
    return 0;
}

static int drain_filter(void) {
    for (;;) {
        AVFrame *frame = av_frame_alloc();
        if (!frame) {
            return fail_message("Could not allocate a filtered frame");
        }

        const int result = av_buffersink_get_frame(buffer_sink, frame);
        if (result == AVERROR(EAGAIN) || result == AVERROR_EOF) {
            av_frame_free(&frame);
            return 0;
        }
        if (result < 0) {
            av_frame_free(&frame);
            return fail_ffmpeg("av_buffersink_get_frame", result);
        }
        if (frame->format != AV_PIX_FMT_YUV420P) {
            av_frame_free(&frame);
            return fail_message("The deinterlacer produced a pixel format other than yuv420p");
        }

        // BWDIF changes the output link's time base (to 1/2 of the input time
        // base even in send_frame mode). Convert its integer timestamps back
        // into the microsecond time base exposed by this wrapper.
        const AVRational output_time_base = av_buffersink_get_time_base(buffer_sink);
        const AVRational microsecond_time_base = (AVRational){TIME_BASE_NUM, TIME_BASE_DEN};
        if (frame->pts != AV_NOPTS_VALUE) {
            frame->pts = av_rescale_q(frame->pts, output_time_base, microsecond_time_base);
        }
        if (frame->duration > 0) {
            frame->duration = av_rescale_q(frame->duration, output_time_base, microsecond_time_base);
        }
        if (frame->duration <= 0) {
            frame->duration = fallback_duration;
        }

        const int enqueue_result = enqueue_frame(frame);
        if (enqueue_result < 0) {
            return enqueue_result;
        }
    }
}

static int init_filter(const AVFrame *frame) {
    if (frame->format != AV_PIX_FMT_YUV420P) {
        return fail_message("The PoC currently supports MPEG-2 yuv420p output only");
    }

    filter_graph = avfilter_graph_alloc();
    if (!filter_graph) {
        return fail_message("Could not allocate the BWDIF filter graph");
    }

    const AVFilter *buffer_filter = avfilter_get_by_name("buffer");
    const AVFilter *bwdif_filter = avfilter_get_by_name("bwdif");
    const AVFilter *sink_filter = avfilter_get_by_name("buffersink");
    if (!buffer_filter || !bwdif_filter || !sink_filter) {
        return fail_message("Required FFmpeg filters are missing");
    }

    const AVRational aspect = frame->sample_aspect_ratio.num > 0
        ? frame->sample_aspect_ratio
        : (AVRational){1, 1};

    // MPEG-2 streams sometimes don't repeat their colour description on the
    // first decoded picture. Pick the broadcast default in that case and keep
    // it stable: many filters (including bwdif) don't support renegotiating
    // colour properties after the graph has been configured.
    filter_color_space = frame->colorspace != AVCOL_SPC_UNSPECIFIED
        ? frame->colorspace
        : (frame->height >= 720 ? AVCOL_SPC_BT709 : AVCOL_SPC_SMPTE170M);
    filter_color_range = frame->color_range != AVCOL_RANGE_UNSPECIFIED
        ? frame->color_range
        : AVCOL_RANGE_MPEG;

    char args[256];
    snprintf(
        args,
        sizeof(args),
        "video_size=%dx%d:pix_fmt=%d:time_base=%d/%d:pixel_aspect=%d/%d:colorspace=%d:range=%d",
        frame->width,
        frame->height,
        frame->format,
        TIME_BASE_NUM,
        TIME_BASE_DEN,
        aspect.num,
        aspect.den,
        filter_color_space,
        filter_color_range
    );

    int result = avfilter_graph_create_filter(
        &buffer_source,
        buffer_filter,
        "input",
        args,
        NULL,
        filter_graph
    );
    if (result < 0) {
        return fail_ffmpeg("create buffer filter", result);
    }

    result = avfilter_graph_create_filter(
        &buffer_sink,
        sink_filter,
        "output",
        NULL,
        NULL,
        filter_graph
    );
    if (result < 0) {
        return fail_ffmpeg("create buffersink filter", result);
    }

    AVFilterContext *bwdif = NULL;
    result = avfilter_graph_create_filter(
        &bwdif,
        bwdif_filter,
        "bwdif",
        "mode=send_frame:parity=auto:deint=interlaced",
        NULL,
        filter_graph
    );
    if (result < 0) {
        return fail_ffmpeg("create bwdif filter", result);
    }

    result = avfilter_link(buffer_source, 0, bwdif, 0);
    if (result >= 0) {
        result = avfilter_link(bwdif, 0, buffer_sink, 0);
    }
    if (result >= 0) {
        result = avfilter_graph_config(filter_graph, NULL);
    }
    if (result < 0) {
        return fail_ffmpeg("configure bwdif graph", result);
    }

    return 0;
}

static int drain_decoder(void) {
    for (;;) {
        const int result = avcodec_receive_frame(decoder_context, decoded_frame);
        if (result == AVERROR(EAGAIN) || result == AVERROR_EOF) {
            return 0;
        }
        if (result < 0) {
            return fail_ffmpeg("avcodec_receive_frame", result);
        }

        if (!filter_graph) {
            const int filter_result = init_filter(decoded_frame);
            if (filter_result < 0) {
                av_frame_unref(decoded_frame);
                return filter_result;
            }
        }

        decoded_frame->colorspace = filter_color_space;
        decoded_frame->color_range = filter_color_range;

        const int source_result = av_buffersrc_add_frame_flags(
            buffer_source,
            decoded_frame,
            AV_BUFFERSRC_FLAG_KEEP_REF
        );
        av_frame_unref(decoded_frame);
        if (source_result < 0) {
            return fail_ffmpeg("av_buffersrc_add_frame_flags", source_result);
        }

        const int filter_result = drain_filter();
        if (filter_result < 0) {
            return filter_result;
        }
    }
}

EMSCRIPTEN_KEEPALIVE
int mpeg2_decoder_init(void) {
    last_error[0] = '\0';
    clear_frame_queue();

    const AVCodec *decoder = avcodec_find_decoder(AV_CODEC_ID_MPEG2VIDEO);
    if (!decoder) {
        return fail_message("The MPEG-2 decoder is not included in this FFmpeg build");
    }

    decoder_context = avcodec_alloc_context3(decoder);
    decoded_frame = av_frame_alloc();
    if (!decoder_context || !decoded_frame) {
        return fail_message("Could not allocate the MPEG-2 decoder");
    }

    decoder_context->pkt_timebase = (AVRational){TIME_BASE_NUM, TIME_BASE_DEN};
    decoder_context->thread_count = 1;

    const int result = avcodec_open2(decoder_context, decoder, NULL);
    if (result < 0) {
        return fail_ffmpeg("avcodec_open2", result);
    }
    return 0;
}

EMSCRIPTEN_KEEPALIVE
int mpeg2_decoder_send(const uint8_t *data, int size, double pts, double duration) {
    if (!decoder_context || !data || size <= 0) {
        return fail_message("Invalid decoder state or packet");
    }

    AVPacket *packet = av_packet_alloc();
    if (!packet) {
        return fail_message("Could not allocate an AVPacket");
    }

    int result = av_new_packet(packet, size);
    if (result < 0) {
        av_packet_free(&packet);
        return fail_ffmpeg("av_new_packet", result);
    }
    memcpy(packet->data, data, (size_t)size);
    packet->pts = (int64_t)pts;
    packet->dts = AV_NOPTS_VALUE;
    packet->duration = (int64_t)duration;
    fallback_duration = packet->duration;

    result = avcodec_send_packet(decoder_context, packet);
    av_packet_free(&packet);
    if (result < 0) {
        return fail_ffmpeg("avcodec_send_packet", result);
    }

    return drain_decoder();
}

EMSCRIPTEN_KEEPALIVE
int mpeg2_decoder_flush(void) {
    if (!decoder_context) {
        return fail_message("Decoder is not initialized");
    }

    int result = avcodec_send_packet(decoder_context, NULL);
    if (result < 0 && result != AVERROR_EOF) {
        return fail_ffmpeg("flush MPEG-2 decoder", result);
    }

    result = drain_decoder();
    if (result < 0) {
        return result;
    }

    if (buffer_source) {
        result = av_buffersrc_add_frame_flags(buffer_source, NULL, 0);
        if (result < 0 && result != AVERROR_EOF) {
            return fail_ffmpeg("flush BWDIF", result);
        }
        return drain_filter();
    }
    return 0;
}

EMSCRIPTEN_KEEPALIVE
int mpeg2_decoder_receive(void) {
    av_frame_free(&current_frame);
    if (frame_queue_length == 0) {
        return 0;
    }

    current_frame = frame_queue[frame_queue_head];
    frame_queue[frame_queue_head] = NULL;
    frame_queue_head = (frame_queue_head + 1) % FRAME_QUEUE_CAPACITY;
    frame_queue_length--;
    return 1;
}

EMSCRIPTEN_KEEPALIVE int mpeg2_frame_width(void) { return current_frame ? current_frame->width : 0; }
EMSCRIPTEN_KEEPALIVE int mpeg2_frame_height(void) { return current_frame ? current_frame->height : 0; }
EMSCRIPTEN_KEEPALIVE int mpeg2_frame_plane_pointer(int plane) {
    return current_frame && plane >= 0 && plane < 3 ? (int)(uintptr_t)current_frame->data[plane] : 0;
}
EMSCRIPTEN_KEEPALIVE int mpeg2_frame_plane_stride(int plane) {
    return current_frame && plane >= 0 && plane < 3 ? current_frame->linesize[plane] : 0;
}
EMSCRIPTEN_KEEPALIVE double mpeg2_frame_pts(void) {
    return current_frame && current_frame->pts != AV_NOPTS_VALUE ? (double)current_frame->pts : 0.0;
}
EMSCRIPTEN_KEEPALIVE double mpeg2_frame_duration(void) {
    return current_frame ? (double)current_frame->duration : 0.0;
}
EMSCRIPTEN_KEEPALIVE int mpeg2_frame_sar_num(void) {
    return current_frame && current_frame->sample_aspect_ratio.num > 0
        ? current_frame->sample_aspect_ratio.num
        : 1;
}
EMSCRIPTEN_KEEPALIVE int mpeg2_frame_sar_den(void) {
    return current_frame && current_frame->sample_aspect_ratio.den > 0
        ? current_frame->sample_aspect_ratio.den
        : 1;
}

EMSCRIPTEN_KEEPALIVE
const char *mpeg2_decoder_error(void) {
    return last_error;
}

EMSCRIPTEN_KEEPALIVE
void mpeg2_decoder_close(void) {
    av_frame_free(&current_frame);
    clear_frame_queue();
    av_frame_free(&decoded_frame);
    avfilter_graph_free(&filter_graph);
    buffer_source = NULL;
    buffer_sink = NULL;
    avcodec_free_context(&decoder_context);
}
