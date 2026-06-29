/* folder-transfer C API  (ft.dll / libft.so)
 *
 * All strings are UTF-8, NUL-terminated. Functions return 0 on success and a
 * non-zero code on failure; call ft_last_error() for the message (per thread).
 *
 * Model: one side SERVES (has the data), the other GETs (pulls). To move data
 * from machine A to machine B:
 *   A:  void* h = ft_serve_start("D:/data", 8722, 4, NULL, 0, 1, tok, 64, fp, 128);
 *       // send `tok` and `fp` (and A's address/port) to B, then:
 *   B:  ft_get("10.0.0.1", 8722, tok, fp, "E:/incoming", NULL, 0);
 *   A:  ft_serve_wait(h);   // blocks until B finished; frees the handle
 */
#ifndef FT_H
#define FT_H
#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Download from a server into to_folder. ignore may be NULL; streams<=0 lets the
 * server pick the mode. Returns 0 on success. */
int32_t ft_get(const char* server, uint16_t port, const char* token,
               const char* fingerprint, const char* to_folder,
               const char* ignore, int32_t streams);

/* Start serving `folder` on a background thread. Writes the freshly generated
 * token and certificate fingerprint into the caller's buffers (give them to the
 * receiver's ft_get). once != 0 => exit after one client finishes.
 * Returns an opaque handle, or NULL on error. */
void* ft_serve_start(const char* folder, uint16_t port, int32_t streams,
                     const char* ignore, int32_t no_compress, int32_t once,
                     char* out_token, size_t out_token_len,
                     char* out_fingerprint, size_t out_fingerprint_len);

/* Wait for a server (from ft_serve_start) to finish and free its handle.
 * Returns 0 on a clean finish, non-zero otherwise. */
int32_t ft_serve_wait(void* handle);

/* Copy the current thread's last error into buf (UTF-8, NUL-terminated).
 * Returns the message's byte length (may exceed len if truncated). buf may be
 * NULL to just query the length. */
int32_t ft_last_error(char* buf, size_t len);

#ifdef __cplusplus
}
#endif
#endif /* FT_H */
