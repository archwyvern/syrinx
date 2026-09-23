/*
 * gstsyrinx: a GStreamer plugin that plays .syr sound sources.
 *
 * Two things are registered: a typefinder that recognises a sound source as `audio/x-syrinx`,
 * and `syrinxdec`, an element with that on its sink pad and interleaved F32 PCM on its source
 * pad. Any GStreamer player (GNOME's Decibels and Showtime, gst-play, anything on playbin) then
 * opens a .syr like any other audio file.
 *
 * A source is text and the sound is whatever it computes, so there is nothing to stream through:
 * the element reads the whole file, renders it once through libsyrinx (the same host the CLI
 * uses, so what plays is what `syrinx compile` writes), and then streams the rendered PCM from a
 * task on its source pad. Seeking is a matter of moving that task's position, so seeks are exact
 * and instant. The file is read in pull mode when the source allows it (a file always does), and
 * gathered from pushed buffers until EOS otherwise.
 *
 * Like the VLC module beside it, this links libsyrinx.so with an rpath of $ORIGIN: the library
 * is installed next to the plugin and found there first.
 */

#include <gst/gst.h>
#include <gst/base/gstadapter.h>
#include <gst/audio/audio.h>

#include <string.h>

#include "syrinx.h"

GST_DEBUG_CATEGORY_STATIC(syrinxdec_debug);
#define GST_CAT_DEFAULT syrinxdec_debug

/* A source is a few kilobytes; anything past this is not a sound source. */
#define MAX_SOURCE_BYTES (16 * 1024 * 1024)

/* Frames per pushed buffer. */
#define CHUNK_FRAMES 4096

#define GST_TYPE_SYRINX_DEC (gst_syrinx_dec_get_type())
G_DECLARE_FINAL_TYPE(GstSyrinxDec, gst_syrinx_dec, GST, SYRINX_DEC, GstElement)

struct _GstSyrinxDec {
    GstElement element;

    GstPad *sinkpad;
    GstPad *srcpad;

    /* Push-mode gathering of the source text until EOS. */
    GstAdapter *adapter;
    gboolean pull;

    /* The render, once made. `samples` belongs to `render`. */
    SyrinxRender *render;
    const float *samples;
    guint rate;
    guint channels;
    guint64 frames;

    /* Streaming state, protected by the source pad's stream lock. */
    GstSegment segment;
    guint64 position;   /* next frame to push */
    gboolean need_segment;
    gboolean sent_headers;
};

static GstStaticPadTemplate sink_template = GST_STATIC_PAD_TEMPLATE(
    "sink", GST_PAD_SINK, GST_PAD_ALWAYS, GST_STATIC_CAPS("audio/x-syrinx"));

static GstStaticPadTemplate src_template = GST_STATIC_PAD_TEMPLATE(
    "src", GST_PAD_SRC, GST_PAD_ALWAYS,
    GST_STATIC_CAPS("audio/x-raw, format = (string) " GST_AUDIO_NE(F32) ", "
                    "layout = (string) interleaved, rate = (int) [ 8000, 192000 ], channels = (int) [ 1, 2 ]"));

G_DEFINE_TYPE(GstSyrinxDec, gst_syrinx_dec, GST_TYPE_ELEMENT);

/* ------------------------------------------------------------------------------------------ */
/* Rendering */

static void free_render(GstSyrinxDec *dec)
{
    if (dec->render != NULL) {
        syrinx_render_free(dec->render);
        dec->render = NULL;
    }
    dec->samples = NULL;
    dec->frames = 0;
}

/** The file's path from the upstream URI, for error messages and for relative imports. */
static gchar *source_path(GstSyrinxDec *dec)
{
    GstQuery *query = gst_query_new_uri();
    gchar *path = NULL;
    if (gst_pad_peer_query(dec->sinkpad, query)) {
        gchar *uri = NULL;
        gst_query_parse_uri(query, &uri);
        if (uri != NULL) {
            path = g_filename_from_uri(uri, NULL, NULL);
            g_free(uri);
        }
    }
    gst_query_unref(query);
    return path != NULL ? path : g_strdup("source.syr");
}

/** Renders `text` (not NUL-terminated, `len` bytes). Posts an element error and returns FALSE on failure. */
static gboolean render_source(GstSyrinxDec *dec, const guint8 *text, gsize len)
{
    gchar *path = source_path(dec);
    GST_INFO_OBJECT(dec, "rendering %s (%" G_GSIZE_FORMAT " bytes)", path, len);

    SyrinxRender *render = syrinx_render(text, len, path, NULL, 0, 0);
    if (!syrinx_render_ok(render)) {
        const char *file = syrinx_render_error_file(render);
        int line = syrinx_render_error_line(render);
        if (line > 0) {
            GST_ELEMENT_ERROR(dec, STREAM, DECODE,
                ("%s:%d:%d: %s", file != NULL ? file : path, line, syrinx_render_error_column(render),
                 syrinx_render_error(render)),
                (NULL));
        } else {
            GST_ELEMENT_ERROR(dec, STREAM, DECODE, ("%s: %s", path, syrinx_render_error(render)), (NULL));
        }
        syrinx_render_free(render);
        g_free(path);
        return FALSE;
    }

    dec->render = render;
    dec->samples = syrinx_render_samples(render);
    dec->rate = syrinx_render_sample_rate(render);
    dec->channels = syrinx_render_channels(render);
    dec->frames = syrinx_render_frames(render);
    GST_INFO_OBJECT(dec, "%s: %" G_GUINT64_FORMAT " frames, %u Hz, %u ch", path, dec->frames, dec->rate, dec->channels);
    g_free(path);
    return TRUE;
}

/** Pull mode: the whole file in one range request. */
static gboolean acquire_by_pull(GstSyrinxDec *dec)
{
    gint64 length = 0;
    if (!gst_pad_peer_query_duration(dec->sinkpad, GST_FORMAT_BYTES, &length) || length <= 0) {
        GST_ELEMENT_ERROR(dec, STREAM, DECODE, ("cannot determine the source's size"), (NULL));
        return FALSE;
    }
    if (length > MAX_SOURCE_BYTES) {
        GST_ELEMENT_ERROR(dec, STREAM, DECODE, ("%" G_GINT64_FORMAT " bytes is too large for a sound source", length), (NULL));
        return FALSE;
    }
    GstBuffer *buffer = NULL;
    GstFlowReturn ret = gst_pad_pull_range(dec->sinkpad, 0, (guint) length, &buffer);
    if (ret != GST_FLOW_OK) {
        if (ret != GST_FLOW_FLUSHING) {
            GST_ELEMENT_ERROR(dec, STREAM, DECODE, ("reading the source failed: %s", gst_flow_get_name(ret)), (NULL));
        }
        return FALSE;
    }
    GstMapInfo map;
    gboolean ok = FALSE;
    if (gst_buffer_map(buffer, &map, GST_MAP_READ)) {
        ok = render_source(dec, map.data, map.size);
        gst_buffer_unmap(buffer, &map);
    }
    gst_buffer_unref(buffer);
    return ok;
}

/* ------------------------------------------------------------------------------------------ */
/* Streaming */

static GstClockTime frames_to_time(GstSyrinxDec *dec, guint64 frames)
{
    return gst_util_uint64_scale_int(frames, GST_SECOND, (gint) dec->rate);
}

static guint64 time_to_frames(GstSyrinxDec *dec, GstClockTime t)
{
    if (!GST_CLOCK_TIME_IS_VALID(t)) {
        return 0;
    }
    return gst_util_uint64_scale_int(t, (gint) dec->rate, GST_SECOND);
}

/** Caps, the segment and the title, once the render exists; then the segment on every (re)start. */
static void push_headers(GstSyrinxDec *dec)
{
    if (!dec->sent_headers) {
        if (dec->pull) {
            gchar *stream_id = gst_pad_create_stream_id(dec->srcpad, GST_ELEMENT(dec), NULL);
            gst_pad_push_event(dec->srcpad, gst_event_new_stream_start(stream_id));
            g_free(stream_id);
        }
        GstAudioInfo info;
        gst_audio_info_set_format(&info, GST_AUDIO_FORMAT_F32, (gint) dec->rate, (gint) dec->channels, NULL);
        GstCaps *caps = gst_audio_info_to_caps(&info);
        gst_pad_push_event(dec->srcpad, gst_event_new_caps(caps));
        gst_caps_unref(caps);

        const char *name = syrinx_render_name(dec->render);
        if (name != NULL && name[0] != '\0') {
            GstTagList *tags = gst_tag_list_new(GST_TAG_TITLE, name, NULL);
            gst_pad_push_event(dec->srcpad, gst_event_new_tag(tags));
        }
        dec->sent_headers = TRUE;
        dec->need_segment = TRUE;
    }
    if (dec->need_segment) {
        gst_pad_push_event(dec->srcpad, gst_event_new_segment(&dec->segment));
        dec->need_segment = FALSE;
    }
}

/** Last frame (exclusive) the current segment plays to. */
static guint64 segment_end(GstSyrinxDec *dec)
{
    guint64 end = dec->frames;
    if (GST_CLOCK_TIME_IS_VALID(dec->segment.stop)) {
        end = MIN(end, time_to_frames(dec, dec->segment.stop));
    }
    return end;
}

static void pause_with_eos(GstSyrinxDec *dec)
{
    gst_pad_push_event(dec->srcpad, gst_event_new_eos());
    gst_pad_pause_task(dec->srcpad);
}

/** The source pad task: acquire and render on the first turn, then one chunk per turn. */
static void loop(gpointer data)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(data);

    if (dec->render == NULL) {
        if (!dec->pull || !acquire_by_pull(dec)) {
            /* In push mode the render is made before the task starts; getting here without one
               is a failure that has already been posted. */
            pause_with_eos(dec);
            return;
        }
        gst_segment_init(&dec->segment, GST_FORMAT_TIME);
        dec->segment.duration = frames_to_time(dec, dec->frames);
        dec->segment.stop = dec->segment.duration;
        dec->position = 0;
    }

    push_headers(dec);

    guint64 end = segment_end(dec);
    if (dec->position >= end) {
        GST_DEBUG_OBJECT(dec, "end of the rendered sound");
        pause_with_eos(dec);
        return;
    }

    guint64 n = MIN((guint64) CHUNK_FRAMES, end - dec->position);
    gsize bytes = (gsize) n * dec->channels * sizeof(float);
    GstBuffer *buffer = gst_buffer_new_allocate(NULL, bytes, NULL);
    gst_buffer_fill(buffer, 0, dec->samples + dec->position * dec->channels, bytes);
    GST_BUFFER_PTS(buffer) = frames_to_time(dec, dec->position);
    GST_BUFFER_DURATION(buffer) = frames_to_time(dec, dec->position + n) - GST_BUFFER_PTS(buffer);
    GST_BUFFER_OFFSET(buffer) = dec->position;
    GST_BUFFER_OFFSET_END(buffer) = dec->position + n;
    dec->position += n;
    dec->segment.position = GST_BUFFER_PTS(buffer);

    GstFlowReturn ret = gst_pad_push(dec->srcpad, buffer);
    if (ret == GST_FLOW_OK) {
        return;
    }
    if (ret == GST_FLOW_FLUSHING) {
        GST_DEBUG_OBJECT(dec, "flushing; pausing");
        gst_pad_pause_task(dec->srcpad);
        return;
    }
    if (ret != GST_FLOW_EOS) {
        GST_ELEMENT_FLOW_ERROR(dec, ret);
    }
    pause_with_eos(dec);
}

/* ------------------------------------------------------------------------------------------ */
/* Seeking and queries on the source pad */

static gboolean handle_seek(GstSyrinxDec *dec, GstEvent *event)
{
    gdouble rate;
    GstFormat format;
    GstSeekFlags flags;
    GstSeekType start_type, stop_type;
    gint64 start, stop;
    gst_event_parse_seek(event, &rate, &format, &flags, &start_type, &start, &stop_type, &stop);

    if (format != GST_FORMAT_TIME) {
        GST_DEBUG_OBJECT(dec, "seek in %s refused; only time is supported", gst_format_get_name(format));
        return FALSE;
    }
    if (rate != 1.0) {
        GST_DEBUG_OBJECT(dec, "seek at rate %f refused; only 1.0 is supported", rate);
        return FALSE;
    }
    if (dec->render == NULL) {
        return FALSE;
    }

    gboolean flush = (flags & GST_SEEK_FLAG_FLUSH) != 0;
    if (flush) {
        gst_pad_push_event(dec->srcpad, gst_event_new_flush_start());
    } else {
        gst_pad_pause_task(dec->srcpad);
    }

    /* The task holds this while it works; taking it means it is between chunks. */
    GST_PAD_STREAM_LOCK(dec->srcpad);
    gboolean update = FALSE;
    gst_segment_do_seek(&dec->segment, rate, format, flags, start_type, start, stop_type, stop, &update);
    dec->position = MIN(time_to_frames(dec, dec->segment.start), dec->frames);
    dec->need_segment = TRUE;
    GST_INFO_OBJECT(dec, "seek to frame %" G_GUINT64_FORMAT " (%" GST_TIME_FORMAT ")", dec->position,
                    GST_TIME_ARGS(dec->segment.start));
    if (flush) {
        gst_pad_push_event(dec->srcpad, gst_event_new_flush_stop(TRUE));
    }
    gst_pad_start_task(dec->srcpad, loop, dec, NULL);
    GST_PAD_STREAM_UNLOCK(dec->srcpad);
    return TRUE;
}

static gboolean src_event(GstPad *pad, GstObject *parent, GstEvent *event)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(parent);
    gboolean ret;
    switch (GST_EVENT_TYPE(event)) {
        case GST_EVENT_SEEK:
            ret = handle_seek(dec, event);
            gst_event_unref(event);
            return ret;
        default:
            return gst_pad_event_default(pad, parent, event);
    }
}

static gboolean src_query(GstPad *pad, GstObject *parent, GstQuery *query)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(parent);
    GstFormat format;
    switch (GST_QUERY_TYPE(query)) {
        case GST_QUERY_DURATION:
            gst_query_parse_duration(query, &format, NULL);
            if (format != GST_FORMAT_TIME || dec->render == NULL) {
                return FALSE;
            }
            gst_query_set_duration(query, GST_FORMAT_TIME, (gint64) frames_to_time(dec, dec->frames));
            return TRUE;
        case GST_QUERY_POSITION:
            gst_query_parse_position(query, &format, NULL);
            if (format != GST_FORMAT_TIME || dec->render == NULL) {
                return FALSE;
            }
            gst_query_set_position(query, GST_FORMAT_TIME, (gint64) frames_to_time(dec, dec->position));
            return TRUE;
        case GST_QUERY_SEEKING:
            gst_query_parse_seeking(query, &format, NULL, NULL, NULL);
            if (format != GST_FORMAT_TIME || dec->render == NULL) {
                /* Unknown until rendered; a -1 end here reaches a GJS player as a u64 it warns about. */
                return FALSE;
            }
            gst_query_set_seeking(query, GST_FORMAT_TIME, TRUE, 0, (gint64) frames_to_time(dec, dec->frames));
            return TRUE;
        default:
            return gst_pad_query_default(pad, parent, query);
    }
}

/* ------------------------------------------------------------------------------------------ */
/* Sink pad: activation, and push-mode gathering */

static gboolean sink_activate(GstPad *pad, GstObject *parent)
{
    GstQuery *query = gst_query_new_scheduling();
    gboolean pull = FALSE;
    if (gst_pad_peer_query(pad, query)) {
        pull = gst_query_has_scheduling_mode_with_flags(query, GST_PAD_MODE_PULL, GST_SCHEDULING_FLAG_SEEKABLE);
    }
    gst_query_unref(query);
    GST_DEBUG_OBJECT(parent, "activating in %s mode", pull ? "pull" : "push");
    return gst_pad_activate_mode(pad, pull ? GST_PAD_MODE_PULL : GST_PAD_MODE_PUSH, TRUE);
}

static gboolean sink_activate_mode(GstPad *pad, GstObject *parent, GstPadMode mode, gboolean active)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(parent);
    (void) pad;
    dec->pull = (mode == GST_PAD_MODE_PULL);
    if (!active) {
        return gst_pad_stop_task(dec->srcpad);
    }
    if (dec->pull) {
        return gst_pad_start_task(dec->srcpad, loop, dec, NULL);
    }
    return TRUE;
}

static gboolean src_activate_mode(GstPad *pad, GstObject *parent, GstPadMode mode, gboolean active)
{
    (void) parent;
    if (mode != GST_PAD_MODE_PUSH) {
        return FALSE;
    }
    if (!active) {
        return gst_pad_stop_task(pad);
    }
    return TRUE;
}

static GstFlowReturn sink_chain(GstPad *pad, GstObject *parent, GstBuffer *buffer)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(parent);
    (void) pad;
    gst_adapter_push(dec->adapter, buffer);
    if (gst_adapter_available(dec->adapter) > MAX_SOURCE_BYTES) {
        GST_ELEMENT_ERROR(dec, STREAM, DECODE, ("the source is too large to be a sound source"), (NULL));
        return GST_FLOW_ERROR;
    }
    return GST_FLOW_OK;
}

static gboolean sink_event(GstPad *pad, GstObject *parent, GstEvent *event)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(parent);
    switch (GST_EVENT_TYPE(event)) {
        case GST_EVENT_CAPS:
        case GST_EVENT_SEGMENT:
            /* Upstream's are in bytes and describe the text; ours are made after rendering. */
            gst_event_unref(event);
            return TRUE;
        case GST_EVENT_EOS: {
            gst_event_unref(event);
            gsize len = gst_adapter_available(dec->adapter);
            const guint8 *text = gst_adapter_map(dec->adapter, len);
            gboolean ok = text != NULL && render_source(dec, text, len);
            gst_adapter_unmap(dec->adapter);
            gst_adapter_clear(dec->adapter);
            if (!ok) {
                gst_pad_push_event(dec->srcpad, gst_event_new_eos());
                return FALSE;
            }
            gst_segment_init(&dec->segment, GST_FORMAT_TIME);
            dec->segment.duration = frames_to_time(dec, dec->frames);
            dec->segment.stop = dec->segment.duration;
            dec->position = 0;
            return gst_pad_start_task(dec->srcpad, loop, dec, NULL);
        }
        case GST_EVENT_FLUSH_STOP:
            gst_adapter_clear(dec->adapter);
            return gst_pad_event_default(pad, parent, event);
        default:
            return gst_pad_event_default(pad, parent, event);
    }
}

/* ------------------------------------------------------------------------------------------ */
/* Element */

static GstStateChangeReturn change_state(GstElement *element, GstStateChange transition)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(element);
    GstStateChangeReturn ret = GST_ELEMENT_CLASS(gst_syrinx_dec_parent_class)->change_state(element, transition);
    if (transition == GST_STATE_CHANGE_PAUSED_TO_READY) {
        free_render(dec);
        gst_adapter_clear(dec->adapter);
        dec->sent_headers = FALSE;
        dec->need_segment = FALSE;
        dec->position = 0;
    }
    return ret;
}

static void gst_syrinx_dec_finalize(GObject *object)
{
    GstSyrinxDec *dec = GST_SYRINX_DEC(object);
    free_render(dec);
    g_object_unref(dec->adapter);
    G_OBJECT_CLASS(gst_syrinx_dec_parent_class)->finalize(object);
}

static void gst_syrinx_dec_class_init(GstSyrinxDecClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    GstElementClass *element_class = GST_ELEMENT_CLASS(klass);

    object_class->finalize = gst_syrinx_dec_finalize;
    element_class->change_state = change_state;

    gst_element_class_add_static_pad_template(element_class, &sink_template);
    gst_element_class_add_static_pad_template(element_class, &src_template);
    gst_element_class_set_static_metadata(element_class,
        "Syrinx sound source decoder", "Codec/Decoder/Audio",
        "Renders a .syr JavaScript sound source to PCM through libsyrinx",
        "Archwyvern <https://github.com/archwyvern/syrinx>");
}

static void gst_syrinx_dec_init(GstSyrinxDec *dec)
{
    dec->sinkpad = gst_pad_new_from_static_template(&sink_template, "sink");
    gst_pad_set_activate_function(dec->sinkpad, sink_activate);
    gst_pad_set_activatemode_function(dec->sinkpad, sink_activate_mode);
    gst_pad_set_chain_function(dec->sinkpad, sink_chain);
    gst_pad_set_event_function(dec->sinkpad, sink_event);
    gst_element_add_pad(GST_ELEMENT(dec), dec->sinkpad);

    dec->srcpad = gst_pad_new_from_static_template(&src_template, "src");
    gst_pad_set_activatemode_function(dec->srcpad, src_activate_mode);
    gst_pad_set_event_function(dec->srcpad, src_event);
    gst_pad_set_query_function(dec->srcpad, src_query);
    gst_pad_use_fixed_caps(dec->srcpad);
    gst_element_add_pad(GST_ELEMENT(dec), dec->srcpad);

    dec->adapter = gst_adapter_new();
    gst_segment_init(&dec->segment, GST_FORMAT_TIME);
}

/* ------------------------------------------------------------------------------------------ */
/* Typefinding */

/**
 * A sound source is an ES module exporting `meta` and a default function. Both appear in the
 * first few kilobytes of any real source; a JavaScript file that has both but is not a sound
 * fails at render with a located error, which is the right place for it to fail.
 */
static void syr_typefind(GstTypeFind *find, gpointer user_data)
{
    (void) user_data;
    guint64 length = gst_type_find_get_length(find);
    guint want = 8192;
    if (length > 0 && length < want) {
        want = (guint) length;
    }
    const guint8 *data = gst_type_find_peek(find, 0, want);
    if (data == NULL) {
        return;
    }
    gchar *head = g_strndup((const gchar *) data, want);
    gboolean has_meta = strstr(head, "export const meta") != NULL || strstr(head, "export let meta") != NULL;
    gboolean has_default = strstr(head, "export default") != NULL;
    g_free(head);
    if (has_meta && has_default) {
        GstCaps *caps = gst_caps_new_empty_simple("audio/x-syrinx");
        gst_type_find_suggest(find, GST_TYPE_FIND_LIKELY, caps);
        gst_caps_unref(caps);
    }
}

static gboolean plugin_init(GstPlugin *plugin)
{
    GST_DEBUG_CATEGORY_INIT(syrinxdec_debug, "syrinxdec", 0, "Syrinx sound source decoder");
    GstCaps *caps = gst_caps_new_empty_simple("audio/x-syrinx");
    gboolean ok = gst_type_find_register(plugin, "audio/x-syrinx", GST_RANK_SECONDARY, syr_typefind, "syr", caps, NULL, NULL);
    gst_caps_unref(caps);
    return ok && gst_element_register(plugin, "syrinxdec", GST_RANK_PRIMARY, GST_TYPE_SYRINX_DEC);
}

#define PACKAGE "syrinx"
GST_PLUGIN_DEFINE(GST_VERSION_MAJOR, GST_VERSION_MINOR, syrinx,
                  "Plays .syr JavaScript sound sources by rendering them through libsyrinx",
                  plugin_init, SYRINX_PLUGIN_VERSION, "MIT/X11", "syrinx", "https://github.com/archwyvern/syrinx")
