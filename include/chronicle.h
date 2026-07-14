#ifndef OPEN_CHRONICLE_H
#define OPEN_CHRONICLE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Open Chronicle v1 application ABI.
 *
 * Threading and ownership:
 * - Every input pointer must be valid for the supplied length for the duration
 *   of the call. Rust copies all input before returning and retains no borrowed
 *   pointer.
 * - Handles are opaque, nonzero process-local tokens. Calls on one handle are
 *   serialized internally. A close races safely with calls already entering
 *   the registry; after close wins, later calls return CHRONICLE_INVALID_HANDLE.
 * - Every nonempty output buffer is owned by Chronicle. Copy its bytes, then
 *   pass the exact token/pointer/length triple to chronicle_buffer_free once.
 *   Successful free zeroes the struct. Unknown or repeated frees are rejected
 *   without dereferencing freed storage.
 * - Functions never unwind across this boundary. On failure, request-bearing
 *   entries return a versioned UTF-8 JSON error in `out_response` whenever that
 *   output pointer itself is valid. `chronicle_image_read` returns raw image
 *   bytes on success and a versioned JSON error on failure.
 */

typedef uint64_t ChronicleHandle;

typedef struct ChronicleBuffer {
  uint64_t token;
  const uint8_t *ptr;
  size_t len;
} ChronicleBuffer;

typedef uint32_t ChronicleStatus;

enum {
  CHRONICLE_OK = 0,
  CHRONICLE_INVALID_ARGUMENT = 1,
  CHRONICLE_INVALID_HANDLE = 2,
  CHRONICLE_CONTRACT_ERROR = 3,
  CHRONICLE_STALE_GENERATION = 4,
  CHRONICLE_NOT_FOUND = 5,
  CHRONICLE_NOT_RETAINED = 6,
  CHRONICLE_TOO_LARGE = 7,
  CHRONICLE_IO_ERROR = 8,
  CHRONICLE_PANIC = 9,
  CHRONICLE_INTERNAL_ERROR = 10,
  CHRONICLE_INVALID_BUFFER = 11,
  CHRONICLE_CAPTURE_OWNER_ACTIVE = 12
};

ChronicleStatus chronicle_open(const uint8_t *request_ptr,
                               size_t request_len,
                               ChronicleHandle *out_handle,
                               ChronicleBuffer *out_response);

ChronicleStatus chronicle_call(ChronicleHandle handle,
                               const uint8_t *request_ptr,
                               size_t request_len,
                               ChronicleBuffer *out_response);

ChronicleStatus chronicle_ingest(ChronicleHandle handle,
                                 const uint8_t *request_ptr,
                                 size_t request_len,
                                 const uint8_t *encoded_image_ptr,
                                 size_t encoded_image_len,
                                 ChronicleBuffer *out_response);

ChronicleStatus chronicle_image_read(ChronicleHandle handle,
                                     const uint8_t *request_ptr,
                                     size_t request_len,
                                     ChronicleBuffer *out_response);

ChronicleStatus chronicle_close(ChronicleHandle handle,
                                ChronicleBuffer *out_response);

ChronicleStatus chronicle_schema_version(ChronicleBuffer *out_response);

ChronicleStatus chronicle_buffer_free(ChronicleBuffer *buffer);

#ifdef __cplusplus
}
#endif

#endif
