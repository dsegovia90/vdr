//! The microphone and the speaker, as far as the interface is concerned.
//!
//! Sibling of `camera`, and the same division of labour: this module owns the
//! device list and the user's intent, and `net::audio` owns the wire. The two
//! meet at [`Controls`], which the call thread fills in and the interface reads.
//!
//! Much thinner than `camera` because there is no preview to paint and no
//! capture loop to run. `moq_audio` owns the device lifecycle -- opening,
//! replacing, retrying, releasing -- so what is left here is the choosing.

mod control;
mod devices;

pub use control::{Controls, MicStatus};
pub(crate) use control::{loudness, Speaking, Talking};
pub use devices::{
    use_microphones_provider, use_outputs_provider, use_shared_microphones, use_shared_outputs,
};
