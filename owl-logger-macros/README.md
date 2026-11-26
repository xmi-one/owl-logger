# owl-logger-macros

Procedural macros for [`owl-logger`](https://crates.io/crates/owl-logger), providing the `#[monitor]` attribute macro for automatic function instrumentation.

> Users should depend on `owl-logger` directly, which re-exports `#[owl_logger::monitor]`.

## Usage

```rust
use owl_logger::monitor;

#[monitor(span, slow_ms = 200, skip(password), max_len = 512)]
fn authenticate(user: &str, password: &str) -> Result<String, String> {
    let _ = password;
    if user == "admin" {
        Ok("ok".into())
    } else {
        Err("denied".into())
    }
}
```

## Supported Options

- `level = "trace" | "debug" | "info" | "warn" | "error"` — Base event/span level (default `"info"`).
- `skip(self, arg1, arg2)` — Redact specific arguments (including `self`) as `[REDACTED]` (validated at compile time against function parameters).
- `skip_all` — Do not format any arguments (compatible with non-`Debug` parameter types).
- `skip_return` — Do not format return values and disable `Result::Err` escalation.
- `slow_ms = 200` — Escalate exit log to `WARN` with `SLOW` marker when execution time reaches the threshold.
- `max_len = 1024` — Truncate individual formatted parameter/return values at `max_len` bytes with `...` (`0` disables truncation).
- `span` / `span = true` — Create and enter a `tracing::Span` named after the function around entry, execution, and exit events.
- `target = "custom_target"` — Override the `tracing` target for generated events and spans (default `"monitor"`).
- `module_target` — Use the caller's `module_path!()` as the `tracing` target so crate/module-scoped `EnvFilter` rules (e.g. `my_crate=info`) naturally apply.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
