#include <errno.h>
#include <stdint.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/version.h>
#include <libavformat/avformat.h>
#include <libavformat/avio.h>
#include <libavformat/version.h>
#include <libavutil/avutil.h>
#include <libavutil/version.h>
#include <libswresample/version.h>
#include <libswscale/swscale.h>
#include <libswscale/version.h>

#ifdef _WIN32
#define VT_EXPORT __declspec(dllexport)
#else
#define VT_EXPORT
#endif

VT_EXPORT int vt_libavutil_major(void) { return LIBAVUTIL_VERSION_MAJOR; }
VT_EXPORT int vt_libswresample_major(void) { return LIBSWRESAMPLE_VERSION_MAJOR; }
VT_EXPORT int vt_libavcodec_major(void) { return LIBAVCODEC_VERSION_MAJOR; }
VT_EXPORT int vt_libavformat_major(void) { return LIBAVFORMAT_VERSION_MAJOR; }
VT_EXPORT int vt_libswscale_major(void) { return LIBSWSCALE_VERSION_MAJOR; }

VT_EXPORT int vt_av_log_quiet(void) { return AV_LOG_QUIET; }
VT_EXPORT int vt_avmedia_type_video(void) { return AVMEDIA_TYPE_VIDEO; }
VT_EXPORT int vt_av_pix_fmt_bgra(void) { return AV_PIX_FMT_BGRA; }
VT_EXPORT int vt_averror_eagain(void) { return AVERROR(EAGAIN); }
VT_EXPORT int vt_averror_eof(void) { return AVERROR_EOF; }
VT_EXPORT int vt_averror_einval(void) { return AVERROR(EINVAL); }
VT_EXPORT int vt_avseek_flag_backward(void) { return AVSEEK_FLAG_BACKWARD; }
VT_EXPORT int vt_avseek_size(void) { return AVSEEK_SIZE; }
VT_EXPORT int vt_avseek_force(void) { return AVSEEK_FORCE; }
VT_EXPORT int vt_sws_fast_bilinear(void) { return SWS_FAST_BILINEAR; }

VT_EXPORT void vt_format_set_interrupt(AVFormatContext *f, int (*cb)(void *), void *opaque) {
    if (!f) return;
    f->interrupt_callback.callback = cb;
    f->interrupt_callback.opaque = opaque;
}
VT_EXPORT void vt_format_set_pb(AVFormatContext *f, AVIOContext *pb) { if (f) f->pb = pb; }
VT_EXPORT int64_t vt_format_duration(const AVFormatContext *f) { return f ? f->duration : 0; }
VT_EXPORT unsigned vt_format_nb_streams(const AVFormatContext *f) { return f ? f->nb_streams : 0; }
VT_EXPORT AVStream *vt_format_stream(AVFormatContext *f, unsigned i) {
    return (f && i < f->nb_streams) ? f->streams[i] : NULL;
}
VT_EXPORT AVCodecParameters *vt_stream_codecpar(AVStream *s) { return s ? s->codecpar : NULL; }
VT_EXPORT int vt_codecpar_width(const AVCodecParameters *p) { return p ? p->width : 0; }
VT_EXPORT int vt_codecpar_height(const AVCodecParameters *p) { return p ? p->height : 0; }
VT_EXPORT void vt_codec_set_thread_count(AVCodecContext *c, int n) { if (c) c->thread_count = n; }
VT_EXPORT int vt_packet_stream_index(const AVPacket *p) { return p ? p->stream_index : -1; }
VT_EXPORT int vt_frame_width(const AVFrame *f) { return f ? f->width : 0; }
VT_EXPORT int vt_frame_height(const AVFrame *f) { return f ? f->height : 0; }
VT_EXPORT int vt_frame_format(const AVFrame *f) { return f ? f->format : -1; }
VT_EXPORT uint8_t * const *vt_frame_data(const AVFrame *f) { return f ? (uint8_t * const *)f->data : NULL; }
VT_EXPORT const int *vt_frame_linesize(const AVFrame *f) { return f ? f->linesize : NULL; }

VT_EXPORT uint8_t *vt_avio_buffer(AVIOContext *s) { return s ? s->buffer : NULL; }
