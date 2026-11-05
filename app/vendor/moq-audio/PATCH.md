# vid's patch to moq-audio 0.0.22

The crate as published on crates.io (MIT OR Apache-2.0, see `Cargo.toml`),
vendored unchanged in its own commit. **One change**, in `src/aec.rs`, so that
diff is the whole patch:

**`Canceller::statistics() -> Option<sonora::AudioProcessingStats>`**, the echo
canceller's own measurements (echo-return-loss enhancement, echo return loss,
delay estimate, residual echo likelihood, divergent filter fraction), refreshed
about once a second.

## Why

vid's first real call had echo: one participant heard their own voice through
the others' microphones. sonora measures exactly what would say whose canceller
is failing and why, but 0.0.22 keeps the processor private, and the canceller
offers only on/off.

## How, and the one subtlety

The microphone callback (`Canceller::process`) holds the processor lock and
uses `try_lock`. On contention it passes its buffer through **uncancelled**. So
reading statistics from another thread would take that lock and leak about
10 ms of echo each time. Instead the callback copies the statistics into a
separate small slot once a second, while it already holds the lock, again with
`try_lock`, so it never waits. `AudioProcessingStats` is all `Option<f64>`/
`Option<i32>`, so nothing allocates on the audio thread.

## Leaving

Propose this upstream (moq-dev/moq, `rs/moq-audio`), then drop the vendored copy
and the `[patch.crates-io]` entry in `app/Cargo.toml` once a release carries it.
