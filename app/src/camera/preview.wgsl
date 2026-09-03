// Scales a camera frame into the canvas-sized target texture.
//
// Vello paints the texture we hand back at its natural size with no transform,
// so it has to already match the canvas. This pass does the fit, the letterbox
// and the mirror in one go, and sampling a Bgra8Unorm source also gets us the
// channel swap for free.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// x, y: canvas-to-video uv scale. z: 1.0 to mirror horizontally.
struct Uniforms {
    fit: vec4<f32>,
};

@group(0) @binding(0) var frame_texture: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // One oversized triangle covering the target; cheaper than a quad.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOutput;
    out.position = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let scale = uniforms.fit.xy;
    // Expand around the centre so the video is centred in the canvas.
    var uv = in.uv * scale - (scale - vec2<f32>(1.0, 1.0)) * 0.5;

    if (uniforms.fit.z > 0.5) {
        uv.x = 1.0 - uv.x;
    }

    // Outside the video is letterbox, not edge-clamped smear.
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    return textureSample(frame_texture, frame_sampler, uv);
}
