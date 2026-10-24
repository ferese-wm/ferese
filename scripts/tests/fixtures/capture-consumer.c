/* Native PipeWire consumer: require the selected memory type and write packed
 * BGRx frames only after the producer has completed GPU work. */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/dma-buf.h>
#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>

struct capture {
    struct pw_main_loop *loop;
    struct pw_stream *stream;
    struct spa_hook listener;
    struct spa_video_info_raw format;
    FILE *file;
    unsigned expected_type, frames, wanted;
    uint64_t last_sequence;
    int64_t last_pts;
    int failed;
    int obs_buffers;
};

static void fail(struct capture *capture, const char *error) {
    fprintf(stderr, "capture-consumer: %s\n", error);
    capture->failed = 1;
    pw_main_loop_quit(capture->loop);
}

static void state_changed(void *data, enum pw_stream_state old,
                          enum pw_stream_state state, const char *error) {
    (void)old;
    struct capture *capture = data;
    if (state == PW_STREAM_STATE_ERROR) fail(capture, error ? error : "stream error");
}

static void param_changed(void *data, uint32_t id, const struct spa_pod *param) {
    struct capture *capture = data;
    if (id != SPA_PARAM_Format || !param) return;
    if (spa_format_video_raw_parse(param, &capture->format) < 0 ||
        capture->format.format != SPA_VIDEO_FORMAT_BGRx ||
        !capture->format.size.width || !capture->format.size.height ||
        !!(capture->format.flags & SPA_VIDEO_FLAG_MODIFIER) != (capture->expected_type == SPA_DATA_DmaBuf)) {
        fail(capture, "incorrect negotiated video format or memory contract");
        return;
    }
    uint8_t bytes[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(bytes, sizeof bytes);
    const struct spa_pod *buffers;
    if (capture->obs_buffers) {
        /* OBS supplies a scalar bitmask containing multiple accepted types.
         * Keep this separate from the strict transport test's flags choice. */
        buffers = spa_pod_builder_add_object(&builder,
            SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
            SPA_PARAM_BUFFERS_dataType,
            SPA_POD_Int((1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_DmaBuf)));
    } else {
        buffers = spa_pod_builder_add_object(&builder,
            SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
            SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(4, 2, 8),
            SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int(1 << capture->expected_type));
    }
    const struct spa_pod *header = spa_pod_builder_add_object(&builder,
        SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta,
        SPA_PARAM_META_type, SPA_POD_Id(SPA_META_Header),
        SPA_PARAM_META_size, SPA_POD_Int(sizeof(struct spa_meta_header)));
    const struct spa_pod *parameters[] = {buffers, header};
    pw_stream_update_params(capture->stream, parameters, 2);
    fprintf(stderr, "capture-consumer: %ux%u %u/%u fps %s\n", capture->format.size.width,
            capture->format.size.height, capture->format.framerate.num,
            capture->format.framerate.denom, capture->expected_type == SPA_DATA_DmaBuf ? "DMA-BUF" : "SHM");
}

static void process(void *data) {
    struct capture *capture = data;
    struct pw_buffer *buffer = pw_stream_dequeue_buffer(capture->stream);
    if (!buffer) return;
    struct spa_buffer *spa = buffer->buffer;
    if (spa->n_datas != 1 || !spa->datas[0].chunk) {
        fail(capture, "invalid plane count or missing chunk");
        goto done;
    }
    struct spa_data *plane = &spa->datas[0];
    if (!plane->chunk->size) goto done;
    if (capture->obs_buffers && capture->expected_type == SPA_DATA_MemFd && !plane->data) {
        fail(capture, "OBS shared-memory input was not mapped by PipeWire");
        goto done;
    }
    const struct spa_meta_header *header = spa_buffer_find_meta_data(spa, SPA_META_Header, sizeof *header);
    if (!header || header->pts < 0 || (capture->frames && (header->seq <= capture->last_sequence || header->pts <= capture->last_pts))) {
        fprintf(stderr, "capture-consumer: header=%p frames=%u pts=%lld last_pts=%lld sequence=%llu last_sequence=%llu\n", (void *)header, capture->frames,
                (long long)(header ? header->pts : -1), (long long)capture->last_pts,
                (unsigned long long)(header ? header->seq : 0), (unsigned long long)capture->last_sequence);
        fail(capture, "missing or stale frame sequence/timestamp");
        goto done;
    }
    capture->last_sequence = header->seq;
    capture->last_pts = header->pts;
    uint32_t row = capture->format.size.width * 4;
    uint32_t height = capture->format.size.height;
    if (plane->type != capture->expected_type || plane->fd < 0 || plane->chunk->stride < (int)row ||
        (uint64_t)plane->chunk->offset + (uint64_t)(height - 1) * plane->chunk->stride + row > plane->maxsize) {
        fail(capture, "producer supplied the wrong memory type or invalid plane bounds");
        goto done;
    }
    size_t length = (size_t)plane->mapoffset + plane->maxsize;
    void *mapping = mmap(NULL, length, PROT_READ, MAP_SHARED, plane->fd, 0);
    if (mapping == MAP_FAILED) { fail(capture, "buffer mapping failed"); goto done; }
    struct dma_buf_sync sync = {.flags = DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ};
    if (plane->type == SPA_DATA_DmaBuf && ioctl(plane->fd, DMA_BUF_IOCTL_SYNC, &sync)) {
        munmap(mapping, length); fail(capture, "DMA-BUF read synchronization failed"); goto done;
    }
    const uint8_t *pixels = (const uint8_t *)mapping + plane->mapoffset + plane->chunk->offset;
    for (uint32_t y = 0; y < height; y++) {
        if (fwrite(pixels + (size_t)y * plane->chunk->stride, 1, row, capture->file) != row) {
            capture->failed = 1;
            break;
        }
    }
    if (plane->type == SPA_DATA_DmaBuf) {
        sync.flags = DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ;
        if (ioctl(plane->fd, DMA_BUF_IOCTL_SYNC, &sync)) capture->failed = 1;
    }
    munmap(mapping, length);
    fflush(capture->file);
    capture->frames++;
    if (capture->failed || capture->frames >= capture->wanted) pw_main_loop_quit(capture->loop);
 done:
    pw_stream_queue_buffer(capture->stream, buffer);
}

int main(int argc, char **argv) {
    if (argc != 5) return 2;
    pw_init(&argc, &argv);
    struct capture capture = {.expected_type = !strcmp(argv[2], "dma") ? SPA_DATA_DmaBuf : SPA_DATA_MemFd,
                              .wanted = strtoul(argv[3], NULL, 10),
                              .obs_buffers = getenv("FERESE_TEST_CAPTURE_OBS_BUFFERS") != NULL};
    capture.file = fopen(argv[4], "wb");
    capture.loop = pw_main_loop_new(NULL);
    if (!capture.file || !capture.loop || !capture.wanted) return 3;
    struct pw_context *context = pw_context_new(pw_main_loop_get_loop(capture.loop), NULL, 0);
    struct pw_core *core = pw_context_connect(context, NULL, 0);
    if (!core) return 4;
    capture.stream = pw_stream_new(core, "Ferese capture regression", pw_properties_new(PW_KEY_MEDIA_TYPE, "Video", PW_KEY_MEDIA_CATEGORY, "Capture", NULL));
    const struct pw_stream_events events = {.version = PW_VERSION_STREAM_EVENTS,
        .state_changed = state_changed, .param_changed = param_changed, .process = process};
    pw_stream_add_listener(capture.stream, &capture.listener, &events, &capture);
    uint8_t bytes[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(bytes, sizeof bytes);
    const struct spa_pod *format;
    if (capture.expected_type == SPA_DATA_DmaBuf) {
        format = spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
            SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
            SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
            SPA_FORMAT_VIDEO_format, SPA_POD_Id(SPA_VIDEO_FORMAT_BGRx),
            SPA_FORMAT_VIDEO_modifier, SPA_POD_Long(0),
            SPA_FORMAT_VIDEO_framerate, SPA_POD_Fraction(&SPA_FRACTION(60, 1)));
    } else {
        format = spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
            SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
            SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
            SPA_FORMAT_VIDEO_format, SPA_POD_Id(SPA_VIDEO_FORMAT_BGRx),
            SPA_FORMAT_VIDEO_framerate, SPA_POD_Fraction(&SPA_FRACTION(60, 1)));
    }
    enum pw_stream_flags stream_flags = PW_STREAM_FLAG_AUTOCONNECT;
    if (capture.obs_buffers) stream_flags |= PW_STREAM_FLAG_MAP_BUFFERS;
    int result = pw_stream_connect(capture.stream, PW_DIRECTION_INPUT, strtoul(argv[1], NULL, 10), stream_flags, &format, 1);
    if (result >= 0) pw_main_loop_run(capture.loop);
    else capture.failed = 1;
    pw_stream_destroy(capture.stream);
    pw_core_disconnect(core);
    pw_context_destroy(context);
    pw_main_loop_destroy(capture.loop);
    fclose(capture.file);
    return capture.failed || capture.frames != capture.wanted;
}
