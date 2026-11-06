# vid's patch to moq-audio 0.0.22

The crate as published on crates.io (MIT OR Apache-2.0, see `Cargo.toml`),
vendored unchanged in its own commit. Every change is in `src/aec.rs` and
marked `vid patch`, so the diff against that commit is the whole patch.

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

## 2. Diagnostics

- **`Canceller::statistics() -> Option<AudioProcessingStats>`**: sonora's own
  measurements (ERLE, ERL, its delay, and more), refreshed about once a second.
  `aec::AudioProcessingStats` is re-exported, so callers need no direct
  dependency on sonora. In this configuration sonora fills ERLE, ERL and
  `delay_ms`. Its median/sd delay, residual-echo and divergence fields stay
  empty (no echo detector).
- **`Canceller::reference_activity() -> Option<(u32, f32)>`**: reference
  frames received over the last second, and their RMS. This is what found §1.

**The one subtlety.** The microphone callback holds the processor lock and uses
`try_lock`, and on contention it passes its buffer through **uncancelled**. So
reading these from another thread would leak about 10 ms of echo each time.
Instead the callback copies them into separate small slots once a second,
while it already holds the lock, again with `try_lock`. Everything copied is
plain numbers, so nothing allocates on the audio thread.

## Leaving

Report §1 upstream first (moq-dev/moq, `rs/moq-audio`): it breaks echo
cancellation for every user, and the fix is two lines. Then propose §2. Drop the
vendored copy and the `[patch.crates-io]` entry in `app/Cargo.toml` once a
release carries both.
