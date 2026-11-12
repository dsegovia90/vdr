# vid's patch to moq-audio 0.1.21

The crate as published on crates.io (MIT OR Apache-2.0, see `Cargo.toml`),
vendored unchanged in its own commit. Every change is in `src/aec.rs` and
marked `vid patch`, so the diff against that commit is the whole patch.

**Moved from 0.0.22 to 0.1.21 on 2026-11-06.** Upstream renamed `Canceller`
to `Control` (with a crate-private `Attachment` for the microphone side); the
patch moved with it unchanged in substance.

## 1. The echo fix: the canceller never received what the speakers played

**The bug.** The mixer pushes everything it plays into the canceller's
reference channel (`playback/mixer.rs`). That channel is a `fixed-resample`
`resampling_channel`, which **discards every push until its consumer has read
once** (`fixed-resample` 0.12 `channel.rs`: `push_interleaved` returns
`OutputNotReady` until `output_stream_ready`, which only
`read`/`read_interleaved` set). But the capture side only reads once a whole
10 ms frame is waiting (`aec.rs`, `while reference.available_frames() >=
REFERENCE_FRAME`), and `available_frames` does not set the flag.

So neither side ever moves. The canceller runs, but against silence, and
**passes the microphone through with the room's echo in it**, on every machine.
The playback test in `mixer.rs` (`discards_writes_until_the_device_reads`)
documents the same rule for the device side, where the device's callback does
read once; the reference side has no such first read.

**Measured in vid** (loopback, speakers on, someone talking), with the counter
from §2:

| | Reference frames reaching the canceller | Canceller state |
|---|---|---|
| before | **0 frames/s**, rms 0 | ERL −30 dB, ERLE 0.2 dB: AEC3's starting values, never updated in minutes |
| after | **100–102 frames/s** | delay estimate found (12 ms), ERL adapting (−30 → −23.6 dB) within a minute of speech |

**The fix.** Mark the consumer ready when the tap is (re)built
(`Reference::rebuild`), and again on every capture callback before checking
for frames, since a channel reset clears the flag.

**Still needed on 0.1.21: measured 2026-11-06.** A loopback built with §1
removed reported **0 speaker reference frames/s, rms 0** in every `[aec]` line,
and ERL frozen at −30 dB: the same deadlock as on 0.0.22. With §1 the same
loopback reads 100–101 frames/s and ERL adapts (−30 → −16 dB in 20 s). A unit
test that builds the tap, attaches a microphone, then pushes did *not* show the
bug (the channel's startup padding survived and unlocked it), so upstream's
tests can pass while the app is deaf. Report it with the app's numbers, not a
unit test.

## 2. Diagnostics

- **`Control::statistics() -> Option<AudioProcessingStats>`**: sonora's own
  measurements (ERLE, ERL, its delay, and more), refreshed about once a second.
  `aec::AudioProcessingStats` is re-exported, so callers need no direct
  dependency on sonora. In this configuration sonora fills ERLE, ERL and
  `delay_ms`. Its median/sd delay, residual-echo and divergence fields stay
  empty (no echo detector).
- **`Control::reference_activity() -> Option<(u32, f32)>`**: reference
  frames received over the last second, and their RMS. This is what found §1.

**The one subtlety.** The microphone callback holds the processor lock and uses
`try_lock`, and on contention it passes its buffer through **uncancelled**. So
reading these from another thread would leak about 10 ms of echo each time.
Instead the callback copies them into separate small slots once a second,
while it already holds the lock, again with `try_lock`. Everything copied is
plain numbers, so nothing allocates on the audio thread.

## Leaving

**§1 is fixed on upstream `main`, not in any release** (checked 2026-11-12).
moq commit 877d6fe27 (PR #6050, 2026-10-20) marks the reference consumer
ready on every capture callback, with the regression test
`a_resampled_tap_starts_flowing`; ac44f9277 (#6865) later replaced
fixed-resample with moq-audio's own `ring.rs`, which keeps the same gate and
the same fix. Every crates.io release through 0.1.23 (`origin/release`,
2026-11-11) still has the deadlock. The way out is a backport PR of that one
line plus its test to `release`, or waiting for a release cut from `main`,
whose API is breaking and would need a migration here. §2 is on neither.
Drop the vendored copy and the `[patch.crates-io]` entry in `app/Cargo.toml`
once a release carries §1; §2 then needs its own home or an upstream PR.
