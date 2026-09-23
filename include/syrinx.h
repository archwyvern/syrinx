/* syrinx — a compiler for sounds: JavaScript in, PCM out.
 *
 * Every call returns an opaque SyrinxRender that is either OK (meta + samples) or an error
 * (kind, message, position). Accessors tolerate NULL and return 0/NULL for anything absent.
 * Free every result with syrinx_render_free. Calls are independent and may run concurrently
 * on different threads; each render uses its own V8 isolate.
 *
 * A SyrinxStream hands the same sound out one block at a time: syrinx_stream_open, then
 * syrinx_stream_next until it returns NULL, each block a SyrinxRender of its own. A stream is
 * used from one thread at a time; freeing it stops whatever is still computing.
 */
#ifndef SYRINX_H
#define SYRINX_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SyrinxRender SyrinxRender;
typedef struct SyrinxStream SyrinxStream;

enum SyrinxErrorKind {
    SYRINX_OK = 0,
    SYRINX_ERROR_CHECK = 1,    /* static determinism check rejected the source */
    SYRINX_ERROR_COMPILE = 2,  /* syntax error */
    SYRINX_ERROR_RUNTIME = 3,  /* the source threw */
    SYRINX_ERROR_TIMEOUT = 4,  /* the source ran past its time budget and was killed */
    SYRINX_ERROR_CONTRACT = 5, /* bad meta or bad return value */
    SYRINX_ERROR_INTERNAL = 6, /* failure inside the library */
};

/* Contract/prelude version. Key cache artefacts on it. 3 accepts sources declaring api 2 or 3. */
uint32_t syrinx_version(void);

/* Human-readable build description. Static; do not free. */
const char *syrinx_version_string(void);

/* The prelude source every sound is compiled against (the "syrinx" module). Static; do not free. */
const char *syrinx_prelude(void);

/* TypeScript declarations for the prelude and the source contract. Static; do not free. */
const char *syrinx_types(void);

/* Compile `source` (UTF-8, `source_len` bytes, not NUL-terminated) to samples. `name` is the
 * source's path (NUL-terminated, may be NULL): relative imports resolve from its directory.
 * `root` (NUL-terminated, may be NULL = unrestricted) is the directory imports may not escape.
 * `sample_rate` 0 = the source's own, else 48000. `timeout_ms` 0 = the default budget.
 * Never returns NULL. */
SyrinxRender *syrinx_render(const uint8_t *source, size_t source_len, const char *name,
                            const char *root, uint32_t sample_rate, uint32_t timeout_ms);

/* Run the static check and the module graph; returns meta and dependencies with no samples
 * (frames = 0). Never returns NULL. */
SyrinxRender *syrinx_inspect(const uint8_t *source, size_t source_len, const char *name,
                             const char *root);

/* Open `source` as a stream; arguments as for syrinx_render. `stems` is NULL or "" for the mix,
 * or a comma-separated list of layers to sum without the mix stage. Never returns NULL:
 * syrinx_stream_info says whether it opened. */
SyrinxStream *syrinx_stream_open(const uint8_t *source, size_t source_len, const char *name,
                                 const char *root, uint32_t sample_rate, uint32_t timeout_ms,
                                 const char *stems);

/* The open result: ok or the error, meta, geometry (frames = the whole sound), dependencies
 * and layers, with no samples. Owned by the stream; do not free. */
const SyrinxRender *syrinx_stream_info(const SyrinxStream *s);

/* True when the blocks are computed on demand (a layer streams and the mix stage streams);
 * false when the whole sound was rendered at open and is handed out in blocks. */
bool syrinx_stream_streaming(const SyrinxStream *s);

/* The next block, or NULL after the last. A block is a SyrinxRender: syrinx_render_frames is
 * its length (SYRINX_BLOCK_FRAMES but for the last), syrinx_render_offset its first frame,
 * syrinx_render_samples its interleaved samples. An error block reports the failure through
 * the error accessors and ends the stream. Free every block with syrinx_render_free. */
SyrinxRender *syrinx_stream_next(SyrinxStream *s);

/* Frees a stream, stopping whatever it was still computing. NULL is a no-op. */
void syrinx_stream_free(SyrinxStream *s);

/* Frames per block of a stream; the last block of a sound is shorter. */
#define SYRINX_BLOCK_FRAMES 4096

bool syrinx_render_ok(const SyrinxRender *r);
int32_t syrinx_render_error_kind(const SyrinxRender *r);     /* enum SyrinxErrorKind */
const char *syrinx_render_error(const SyrinxRender *r);      /* NULL when ok; owned by r */
const char *syrinx_render_error_file(const SyrinxRender *r); /* module the position refers to; NULL if none */
int32_t syrinx_render_error_line(const SyrinxRender *r);     /* 1-based; 0 = unknown */
int32_t syrinx_render_error_column(const SyrinxRender *r);   /* 1-based; 0 = unknown */

const char *syrinx_render_name(const SyrinxRender *r);       /* owned by r; NULL when the source declares no name */
double syrinx_render_duration(const SyrinxRender *r);        /* seconds, as declared */
uint32_t syrinx_render_seed(const SyrinxRender *r);
bool syrinx_render_loop(const SyrinxRender *r);
uint32_t syrinx_render_sample_rate(const SyrinxRender *r);   /* actual rate; from inspect: declared or 0 */
uint32_t syrinx_render_channels(const SyrinxRender *r);      /* 1 or 2 */
uint32_t syrinx_render_frames(const SyrinxRender *r);        /* the sound's, or a block's */
uint64_t syrinx_render_offset(const SyrinxRender *r);        /* a block's first frame; 0 for a render */
const float *syrinx_render_samples(const SyrinxRender *r);   /* interleaved, frames * channels; owned by r */

/* Files the source imported, transitively (canonical paths). Cache keys must include them. */
uint32_t syrinx_render_dependency_count(const SyrinxRender *r);
const char *syrinx_render_dependency(const SyrinxRender *r, uint32_t i);  /* NULL out of range; owned by r */

/* The layers the source declares, in declaration order (from render, inspect and stream_info). */
uint32_t syrinx_render_stem_count(const SyrinxRender *r);
const char *syrinx_render_stem_name(const SyrinxRender *r, uint32_t i);  /* NULL out of range; owned by r */

void syrinx_render_free(SyrinxRender *r);

#ifdef __cplusplus
}
#endif

#endif /* SYRINX_H */
