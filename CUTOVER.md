# Live‑database mode (two‑phase cutover)

A way to copy a **live, constantly‑changing folder** — typically a running database — to another
machine **consistently**, with only a few seconds of downtime instead of stopping the service for
the whole transfer.

← back to the [main README](README.md).

## The problem

If you copy a database while it’s running, the files change *during* the copy: by the time the last
file is read, the first ones are already stale. The result is an **inconsistent snapshot** that may
not even open. The obvious fix — stop the database, copy everything, start it again — means the
service is **down for the entire copy**, which can be many minutes or hours for a large database.

## The idea: copy hot, freeze briefly, top up

`ft` splits the transfer into two passes:

```
        DB running (no downtime)            DB stopped (seconds)
   ┌───────────────────────────────┐   ┌──────────────────────────┐
   │  PHASE 1: bulk copy everything │ → │ PHASE 2: copy only what   │ → done
   │  (the slow part)               │   │ changed since phase 1     │
   └───────────────────────────────┘   └──────────────────────────┘
                                        ▲ you stop the DB here
```

- **Phase 1** copies the whole folder **while the database is up** — this is the slow, heavy part,
  and it costs **zero downtime**. The snapshot is inevitably a bit inconsistent; that’s fine, phase 2
  fixes it.
- You then **stop the database** so its files are consistent on disk.
- **Phase 2** copies only the **delta** — the handful of files that changed since phase 1 (compared
  by size + last‑write‑time, plus any deletions). Because it’s tiny, it finishes in seconds.

Total downtime ≈ **phase 2 only**, not the whole transfer.

## How to use it

Add `--cutover` on the source (or `"cutover": true` in a config). It implies `--once` and forces a
single stream (`--streams 1`).

```bat
ft D:\databases\mydb --cutover
```

Step by step:

1. **Start the source** with `--cutover`. Phase 1 begins immediately; the database stays up.
2. **On the receiver**, run the printed `ft get …` command (it asks where to save). It receives
   phase 1.
3. When phase 1 finishes, the source **pauses** and prints a banner like:
   ```
   ========================================================================
    PHASE 1 complete. Now STOP THE DATABASE so its files are consistent.
    Then create the file to signal phase 2:
      C:\tools\ft\ft-cutover.go
   ========================================================================
   ```
   While it waits, it sends keepalives (~every 15 s) so the connection never times out.
4. **Stop the database** cleanly.
5. **Create the signal file** at the exact path `ft` printed (it lives next to the `ft` binary):
   - Windows: `type nul > C:\tools\ft\ft-cutover.go`
   - Linux: `touch /opt/ft/ft-cutover.go`
6. `ft` detects the file, deletes it, runs **phase 2** (the delta), and exits. The copy on the
   receiver is now a consistent point‑in‑time image.
7. Start the database on the new machine (or wherever you need it).

## Notes & caveats

- **Consistency depends on you.** Phase 2 is only consistent if the database is actually **stopped**
  (or otherwise quiesced) before you create the signal file. `ft` copies files; it doesn’t flush the
  DB for you.
- **Single stream.** Cutover forces `--streams 1` (the two phases are sequential by nature), so it
  doesn’t use the parallel‑streams speed‑up. Phase 1 of a very large DB over a high‑latency link is
  therefore slower than a normal parallel run — but it’s downtime‑free, which is the point.
- **One shot.** `--cutover` implies `--once`: the source exits after phase 2.
- **The signal is a file, not a keypress** — so it works headless and over SSH/RDP, and can be
  scripted (your “stop DB → create flag” step can be one script).
- **Cross‑platform.** Works the same on Windows and Linux; the flag file sits next to the binary.
- **Re‑runnable.** If something goes wrong, just run the whole thing again — it’s a fresh two‑phase
  mirror.

## When *not* to use it

- If the service is small/fast to copy and a short full stop is acceptable, a **plain** transfer
  (stop → `ft D:\db` → start) is simpler.
- If the database **cannot be stopped at all**, cutover can’t give you a consistent copy — use the
  database’s own hot‑backup / replication tooling instead, and use `ft` to move the resulting
  backup files.

---

See also: [main README](README.md) · [security model](README.md#why-its-safe).
