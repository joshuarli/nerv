# Pipeline Project

A data processing pipeline with pluggable filters and scheduling.

## Structure

```
src/
  lib.rs          # Module exports
  pipeline.rs     # Pipeline struct, process(), run_scheduled()
  filters.rs      # Filter trait + 4 implementations
  scheduler.rs    # Scheduler, RateLimiter
```

## Build

```
cargo check
```
