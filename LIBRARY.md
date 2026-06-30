# Embeddable library (`ft.dll` / `libft.so`)

Drive folder transfers **straight from your own code** — .NET, C, or C++ — without spawning the `ft`
CLI. Each release ships a C‑ABI shared library: **`ft.dll`** (Windows) and **`libft.so`** (Linux),
plus a C header ([`ffi/ft.h`](rust/ffi/ft.h)) and a ready .NET binding
([`ffi/FolderTransfer.cs`](rust/ffi/FolderTransfer.cs)).

← back to the [main README](README.md).

## When to use it

When one of your own services needs to push or pull a folder to/from another machine in‑process —
e.g. a server app that seeds a replica, ships a snapshot, or pulls assets on startup. Same protocol,
same security (TLS + pinned cert + token) as the CLI; you just call functions instead of running a
command.

## .NET (P/Invoke)

The included `FolderTransfer.cs` wraps everything:

```csharp
// SOURCE — serve a folder in the background.
// token + fingerprint come back immediately, to hand to the receiver.
var srv = new FolderTransfer.Server(@"D:\data", 8722);
Console.WriteLine($"{srv.Token} {srv.Fingerprint}");
// ... give those to the receiver out-of-band ...
srv.Wait();   // blocks until the transfer finishes

// DESTINATION — pull a folder into a local directory.
FolderTransfer.Get("10.0.0.1", 8722, token, fingerprint, @"E:\incoming");
```

## C ABI

The raw entry points (UTF‑8 strings in; return `0` on success, non‑zero on error):

| Function | Purpose |
|----------|---------|
| `ft_get(server, port, token, fingerprint, to, ignore, streams)` | Pull a folder into `to`. |
| `ft_serve_start(folder, port, streams, ignore, no_compress, once, out_token, …, out_fp, …)` | Start serving in the background; the token + certificate fingerprint come back via the out‑params. Returns a handle. |
| `ft_serve_wait(handle)` | Block until the served transfer finishes. |
| `ft_last_error(buf, len)` | Last error message for the current thread. |

See [`ffi/ft.h`](rust/ffi/ft.h) for the exact signatures.

## Notes

- Everything the CLI does (single/parallel streams, adaptive compression, ignore patterns, mirror
  semantics) applies — the library is the same engine.
- Errors are per‑thread: on a non‑zero return, call `ft_last_error` to get the message.
- The fingerprint is public (share it); the token is the secret (keep it private).

---

See also: [main README](README.md) · [security model](README.md#why-its-safe).
