//! Which video codec backends this machine picks, and what they cost.
//!
//! Ignored by default: it opens real encoders and decoders, so it says
//! something only about the machine it runs on. Run it where there is no camera
//! to hand (a Linux box over SSH), to see whether VA-API or NVENC is chosen:
//!
//! ```sh
//! cargo test --release --bin vdr codec_check -- --ignored --nocapture
//! ```

#[cfg(test)]
mod tests {
    use std::time::Instant;

    const W: u32 = 640;
    const H: u32 = 360;
    const FRAMES: u32 = 150;

    /// A moving gradient, so the encoder has real work every frame.
    fn frame(n: u32) -> moq_video::Frame {
        let mut data = vec![0u8; moq_video::I420::len(moq_video::Size::new(W, H)).expect("even size")];
        let (luma, chroma) = data.split_at_mut((W * H) as usize);
        for y in 0..H {
            for x in 0..W {
                luma[(y * W + x) as usize] = ((x + y + n * 4) % 256) as u8;
            }
        }
        chroma.fill(128);
        let i420 = moq_video::I420::new(moq_video::Size::new(W, H), data).expect("valid I420");
        let at = moq_net::Timestamp::from_micros(u64::from(n) * 33_333).expect("in range");
        moq_video::Frame::new(moq_video::Surface::I420(i420), at)
    }

    async fn run(kind: moq_video::encode::Kind, decode: moq_video::decode::Kind) {
        let mut config =
            moq_video::encode::Config::new(W, H, moq_video::Rate::new(30, 1).expect("rate"));
        config.kind = kind.clone();
        let rendition = match config.probe().await {
            Ok(rendition) => rendition,
            Err(error) => {
                println!("encode {kind:?}: no encoder ({error:#})");
                return;
            }
        };
        let mut encoder = moq_video::encode::Encoder::new(&config).expect("encoder");
        let mut packets = Vec::new();
        let start = Instant::now();
        for n in 0..FRAMES {
            packets.extend(encoder.encode(&frame(n)).expect("encode"));
        }
        packets.extend(encoder.flush().expect("flush"));
        let encode_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES);

        // The decoder on the same side of the choice as the encoder.
        let mut decode_config = moq_video::decode::Config::default();
        decode_config.kind = decode.clone();
        let mut decoder = moq_video::decode::Sink::open(&rendition, &decode_config)
            .await
            .expect("decoder");
        let start = Instant::now();
        let mut decoded = 0;
        for (i, packet) in packets.into_iter().enumerate() {
            decoded += decoder
                .decode(packet.payload, packet.timestamp, i == 0)
                .await
                .expect("decode")
                .len();
        }
        let decode_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES);
        println!(
            "encode {kind:?} / decode {decode:?}: encode {} {encode_ms:.2} ms/frame, decode {} {decode_ms:.2} ms/frame ({decoded}/{FRAMES} frames)",
            encoder.name(),
            decoder.name(),
        );
    }

    #[test]
    #[ignore = "opens this machine's real codecs"]
    fn codec_check() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            use moq_video::{decode, encode};
            run(encode::Kind::Auto, decode::Kind::Auto).await;
            // Decoding on its own, whatever the encoder side did: a machine
            // whose hardware encoder fails may still decode in hardware, and
            // decoding is most of a call's cost (one encode, N-1 decodes).
            run(encode::Kind::Software, decode::Kind::Auto).await;
            run(encode::Kind::Software, decode::Kind::Software).await;
        });
    }
}
