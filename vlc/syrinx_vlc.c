/* VLC demux module for syrinx sound sources (.syr).
 *
 * A .syr file is not a bitstream, it is a program: the whole sound is compiled with
 * libsyrinx when the file is opened, and then handed to VLC as raw 32-bit float PCM
 * (VLC's own `araw` decoder takes it from there). Seeking, pausing and the progress bar
 * work because everything is in memory. Compile errors are reported through VLC's
 * message log and the open fails.
 */

#ifdef HAVE_CONFIG_H
# include "config.h"
#endif

#define VLC_MODULE_COPYRIGHT "Copyright (c) 2026 Dominic Grostate"
#define VLC_MODULE_LICENSE "MIT"

#include <vlc_common.h>
#include <vlc_plugin.h>
#include <vlc_demux.h>
#include <vlc_block.h>
#include <vlc_meta.h>

#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "syrinx.h"

/* Refuse to read a "source" bigger than this: a real one is a few KB. */
#define MAX_SOURCE_BYTES (4u << 20)
/* Frames per block handed to the decoder. ~21 ms at 48 kHz. */
#define BLOCK_FRAMES 1024

static int Open(vlc_object_t *);
static void Close(vlc_object_t *);

vlc_module_begin()
    set_shortname("syrinx")
    set_description("syrinx sound source (.syr)")
    set_category(CAT_INPUT)
    set_subcategory(SUBCAT_INPUT_DEMUX)
    /* Above every byte-stream demuxer: we only accept our own extension, so a high score
     * just means we get asked first for .syr and never for anything else. */
    set_capability("demux", 300)
    set_callbacks(Open, Close)
    add_shortcut("syrinx", "syr")
vlc_module_end()

struct demux_sys_t
{
    SyrinxRender *render;
    const float *samples;
    es_out_id_t *es;
    uint32_t rate;
    uint32_t channels;
    uint64_t frames;
    uint64_t position; /* next frame to send */
};

static int Demux(demux_t *);
static int Control(demux_t *, int, va_list);

static bool HasExtension(const char *path, const char *ext)
{
    if (path == NULL) {
        return false;
    }
    size_t n = strlen(path), m = strlen(ext);
    return n >= m && strcasecmp(path + n - m, ext) == 0;
}

/* Reads the whole input into a malloc'd buffer. */
static uint8_t *ReadAll(demux_t *demux, size_t *out_len)
{
    size_t cap = 64 * 1024, len = 0;
    uint8_t *buf = malloc(cap);
    if (buf == NULL) {
        return NULL;
    }
    for (;;) {
        if (len == cap) {
            if (cap >= MAX_SOURCE_BYTES) {
                msg_Err(demux, "source larger than %u bytes; not a sound source", MAX_SOURCE_BYTES);
                free(buf);
                return NULL;
            }
            cap *= 2;
            uint8_t *grown = realloc(buf, cap);
            if (grown == NULL) {
                free(buf);
                return NULL;
            }
            buf = grown;
        }
        ssize_t got = vlc_stream_Read(demux->s, buf + len, cap - len);
        if (got <= 0) {
            break;
        }
        len += (size_t)got;
    }
    *out_len = len;
    return buf;
}

static int Open(vlc_object_t *obj)
{
    demux_t *demux = (demux_t *)obj;

    bool forced = demux->psz_demux != NULL
        && (strcmp(demux->psz_demux, "syrinx") == 0 || strcmp(demux->psz_demux, "syr") == 0);
    const char *path = demux->psz_file != NULL ? demux->psz_file : demux->psz_location;
    if (!forced && !HasExtension(path, ".syr")) {
        return VLC_EGENERIC;
    }

    size_t len = 0;
    uint8_t *source = ReadAll(demux, &len);
    if (source == NULL) {
        return VLC_EGENERIC;
    }
    /* Cheap sanity check so a stray .syr that is not a sound source fails fast and clearly.
     * A sound is one or more named layers (`export const stems`); the default export is the
     * optional mix stage, so either marks a source. */
    if (!forced && memmem(source, len, "export const stems", 18) == NULL
            && memmem(source, len, "export default", 14) == NULL) {
        msg_Err(demux, "%s has neither `export const stems` nor `export default` and so is not a sound source", path);
        free(source);
        return VLC_EGENERIC;
    }

    SyrinxRender *render = syrinx_render(source, len, path, NULL, 0, 0);
    free(source);
    if (!syrinx_render_ok(render)) {
        int line = syrinx_render_error_line(render);
        const char *file = syrinx_render_error_file(render);
        if (line > 0) {
            msg_Err(demux, "%s:%d:%d: %s", file != NULL ? file : path, line, syrinx_render_error_column(render), syrinx_render_error(render));
        } else {
            msg_Err(demux, "%s: %s", path, syrinx_render_error(render));
        }
        syrinx_render_free(render);
        return VLC_EGENERIC;
    }

    demux_sys_t *sys = calloc(1, sizeof(*sys));
    if (sys == NULL) {
        syrinx_render_free(render);
        return VLC_ENOMEM;
    }
    sys->render = render;
    sys->samples = syrinx_render_samples(render);
    sys->rate = syrinx_render_sample_rate(render);
    sys->channels = syrinx_render_channels(render);
    sys->frames = syrinx_render_frames(render);
    sys->position = 0;

    es_format_t fmt;
    es_format_Init(&fmt, AUDIO_ES, VLC_CODEC_FL32);
    fmt.audio.i_rate = sys->rate;
    fmt.audio.i_channels = (uint8_t)sys->channels;
    fmt.audio.i_physical_channels = sys->channels == 2 ? (AOUT_CHAN_LEFT | AOUT_CHAN_RIGHT) : AOUT_CHAN_CENTER;
    fmt.audio.i_bitspersample = 32;
    fmt.audio.i_blockalign = 4 * sys->channels;
    fmt.i_bitrate = (unsigned)(sys->rate * sys->channels * 32);
    sys->es = es_out_Add(demux->out, &fmt);
    if (sys->es == NULL) {
        syrinx_render_free(render);
        free(sys);
        return VLC_EGENERIC;
    }

    msg_Dbg(demux, "syrinx: %s, %" PRIu64 " frames, %u Hz, %u ch (%s)",
            syrinx_render_name(render), sys->frames, sys->rate, sys->channels, syrinx_version_string());

    demux->pf_demux = Demux;
    demux->pf_control = Control;
    demux->p_sys = sys;
    return VLC_SUCCESS;
}

static void Close(vlc_object_t *obj)
{
    demux_t *demux = (demux_t *)obj;
    demux_sys_t *sys = demux->p_sys;
    syrinx_render_free(sys->render);
    free(sys);
}

static int64_t FrameToTime(const demux_sys_t *sys, uint64_t frame)
{
    return (int64_t)(frame * CLOCK_FREQ / sys->rate);
}

static int Demux(demux_t *demux)
{
    demux_sys_t *sys = demux->p_sys;
    if (sys->position >= sys->frames) {
        return VLC_DEMUXER_EOF;
    }
    uint64_t count = sys->frames - sys->position;
    if (count > BLOCK_FRAMES) {
        count = BLOCK_FRAMES;
    }
    size_t bytes = (size_t)count * sys->channels * sizeof(float);
    block_t *block = block_Alloc(bytes);
    if (block == NULL) {
        return VLC_DEMUXER_EGENERIC;
    }
    memcpy(block->p_buffer, sys->samples + sys->position * sys->channels, bytes);
    block->i_pts = block->i_dts = VLC_TS_0 + FrameToTime(sys, sys->position);
    block->i_length = FrameToTime(sys, count);

    es_out_SetPCR(demux->out, block->i_pts);
    es_out_Send(demux->out, sys->es, block);
    sys->position += count;
    return VLC_DEMUXER_SUCCESS;
}

static int Seek(demux_t *demux, uint64_t frame)
{
    demux_sys_t *sys = demux->p_sys;
    if (frame > sys->frames) {
        frame = sys->frames;
    }
    msg_Dbg(demux, "seek to frame %" PRIu64 " (%.3f s)", frame, (double)frame / sys->rate);
    sys->position = frame;
    es_out_Control(demux->out, ES_OUT_RESET_PCR);
    return VLC_SUCCESS;
}

static int Control(demux_t *demux, int query, va_list args)
{
    demux_sys_t *sys = demux->p_sys;
    switch (query) {
        case DEMUX_CAN_SEEK:
        case DEMUX_CAN_PAUSE:
        case DEMUX_CAN_CONTROL_PACE:
            *va_arg(args, bool *) = true;
            return VLC_SUCCESS;
        case DEMUX_SET_PAUSE_STATE:
            return VLC_SUCCESS;
        case DEMUX_GET_PTS_DELAY:
            *va_arg(args, int64_t *) = INT64_C(1000) * var_InheritInteger(demux, "file-caching");
            return VLC_SUCCESS;
        case DEMUX_GET_LENGTH:
            *va_arg(args, int64_t *) = FrameToTime(sys, sys->frames);
            return VLC_SUCCESS;
        case DEMUX_GET_TIME:
            *va_arg(args, int64_t *) = FrameToTime(sys, sys->position);
            return VLC_SUCCESS;
        case DEMUX_SET_TIME: {
            int64_t time = va_arg(args, int64_t);
            if (time < 0) {
                time = 0;
            }
            return Seek(demux, (uint64_t)time * sys->rate / CLOCK_FREQ);
        }
        case DEMUX_GET_POSITION:
            *va_arg(args, double *) = sys->frames == 0 ? 0.0 : (double)sys->position / (double)sys->frames;
            return VLC_SUCCESS;
        case DEMUX_SET_POSITION: {
            double pos = va_arg(args, double);
            if (pos < 0.0) {
                pos = 0.0;
            }
            if (pos > 1.0) {
                pos = 1.0;
            }
            return Seek(demux, (uint64_t)(pos * (double)sys->frames));
        }
        case DEMUX_GET_META: {
            vlc_meta_t *meta = va_arg(args, vlc_meta_t *);
            vlc_meta_Set(meta, vlc_meta_Title, syrinx_render_name(sys->render));
            vlc_meta_Set(meta, vlc_meta_EncodedBy, syrinx_version_string());
            return VLC_SUCCESS;
        }
        default:
            return VLC_EGENERIC;
    }
}
