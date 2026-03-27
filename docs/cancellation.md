# Cancellation

Three threads coordinate cancellation through an atomic flag and channels.

## Threads

```
┌─────────────┐    stdin_tx/rx     ┌─────────────┐    cmd_tx/rx     ┌─────────────┐
│  stdin       │ ────────────────> │  main loop   │ ──────────────> │  session     │
│  (blocking   │                   │  (select!)   │ <────────────── │  (Agent +    │
│   read)      │                   │              │    event_tx/rx   │   Provider)  │
└─────────────┘                    └─────────────┘                  └─────────────┘
                                         │
                                    cancel_flag
                                    (AtomicBool)
                                         │
                                         v
                                   shared with Agent
                                   and Provider
```

## The cancel_flag

`cancel_flag` is an `Arc<AtomicBool>` created from `agent.cancel`. The
main loop sets it; the provider checks it between SSE chunks.

## ^C while streaming

1. User presses ^C. Terminal is in raw mode (cfmakeraw), so byte 0x03
   arrives on stdin (no SIGINT — ISIG is disabled).
2. stdin thread sends the byte over `stdin_tx`.
3. Main loop parses 0x03 into a `ctrl+c` key event.
4. Handler checks `cancel_flag`:
   - **Not set** (first ^C): set flag, update statusbar, stay in loop.
   - **Already set** (second ^C): `tui.stop()`, `should_quit = true`, break.

## ^C while idle

Same flow, but `!interactive.is_streaming` is true, so the first ^C
quits immediately.

## Instant cancellation via reader thread

Both providers read SSE lines in a background thread that sends lines
through a crossbeam channel:

```rust
std::thread::spawn(move || {
    for line in reader.lines() {
        if line_tx.send(line).is_err() {
            break; // receiver dropped → body dropped → TCP closed
        }
    }
});

loop {
    if cancel.load(Relaxed) {
        drop(line_rx);  // reader thread exits → connection closed
        emit Aborted;
        return;
    }
    match line_rx.recv_timeout(50ms) {
        Ok(line) => process(line),
        Timeout => continue,
        Disconnected => return, // EOF
    }
}
```

Cancellation latency is at most 50ms (the poll interval). Dropping the
receiver closes the TCP connection, stopping server-side generation.

## The should_quit flag

`break` inside `for event in events` (inside a `select!` arm) only
breaks the inner loop. `should_quit` breaks the outer loop.

## SIGINT

Registered via `signal_hook`. In raw mode ^C doesn't generate SIGINT
(ISIG disabled). SIGINT only arrives from external `kill -INT`. The
handler mirrors the ^C logic.

## Terminal cleanup

`tui.stop()` uses `TCSAFLUSH` (not `TCSANOW`) to discard pending input,
preventing stray `^C` echo.

## Cancel flag lifecycle

```
prompt start   → agent.reset_cancel() clears flag
^C             → main loop sets flag
stream end     → AgentEnd event clears flag
next prompt    → reset again
```
