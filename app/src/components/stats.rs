//! One line of numbers about the connection.
//!
//! About the link as a whole -- what we send, round trip, loss. What arrives
//! from each person is on their own tile instead, because in a room one
//! call-wide "fps in" could only ever be one of them.
//!
//! The app has always measured these and always thrown them at the log, which
//! is fine while the only person running it has a terminal open. It stops being
//! fine the moment someone else has a bad call: "it lagged" is all they can
//! tell you, and every cause looks the same from outside.
//!
//! Deliberately one line and always visible rather than a panel behind a
//! toggle. It is six short numbers, it costs a sample every half second, and a
//! diagnostic nobody can find is one nobody uses.

use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use crate::channel::use_channel;
use crate::net::Metrics;

/// How often the numbers are sampled.
///
/// Twice a second. Fast enough to watch a link get worse, slow enough to read:
/// a figure that updates faster than the eye tracks it is decoration.
const POLL: Duration = Duration::from_millis(500);

/// A sampled snapshot, so rendering never touches the atomics.
#[derive(Clone, Copy, PartialEq, Default)]
struct Sample {
    rtt: Option<u32>,
    loss: Option<f32>,
    send_kbps: Option<u32>,
    estimate_kbps: Option<u32>,
    encode_size: Option<(u32, u32)>,
}

#[derive(Clone, PartialEq, Props)]
pub struct StatsProps {
    pub metrics: Arc<Metrics>,
}

#[component]
pub fn StatsRow(props: StatsProps) -> Element {
    let mut sample = use_signal(Sample::default);

    let tx = use_channel(move |next: Sample| sample.set(next));
    use_hook({
        let metrics = props.metrics.clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("stats".into())
                .spawn(move || {
                    loop {
                        let next = Sample {
                            rtt: metrics.rtt(),
                            loss: metrics.loss(),
                            send_kbps: metrics.send_kbps(),
                            estimate_kbps: metrics.estimate_kbps(),
                            encode_size: metrics.encode_size(),
                        };
                        if tx.unbounded_send(next).is_err() {
                            return;
                        }
                        std::thread::sleep(POLL);
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the stats thread: {error}");
            }
        }
    });

    let reading = sample();
    let mut parts: Vec<String> = Vec::new();
    if let Some((width, height)) = reading.encode_size {
        parts.push(format!("{width}\u{d7}{height}"));
    }
    if let Some(kbps) = reading.send_kbps {
        // The estimate alongside the rate, because the interesting thing is the
        // gap: sending far below a healthy estimate means something other than
        // the link is the problem.
        match reading.estimate_kbps {
            Some(estimate) => parts.push(format!("{kbps} kbps up (link {estimate})")),
            None => parts.push(format!("{kbps} kbps up")),
        }
    }
    if let Some(rtt) = reading.rtt {
        parts.push(format!("{rtt} ms"));
    }
    if let Some(loss) = reading.loss {
        parts.push(format!("{loss:.2}% loss"));
    }

    // Nothing to say before a call connects, and an empty row of dashes would
    // read as broken rather than as idle.
    if parts.is_empty() {
        return rsx! {};
    }

    // Loss is the one number worth colouring: it is the difference between a
    // link that is slow and one that is failing. The threshold is where voice
    // and video start to show it rather than anywhere principled.
    let unhealthy = reading.loss.is_some_and(|loss| loss >= 2.0);
    let class = if unhealthy {
        "shrink-0 whitespace-nowrap font-mono text-xs text-amber-400"
    } else {
        "shrink-0 whitespace-nowrap font-mono text-xs text-neutral-500"
    };

    // Inline, so it can sit on the control bar's status line after the call's
    // own status.
    rsx! {
        span { class, "\u{b7}  {parts.join(\"  \u{b7}  \")}" }
    }
}
