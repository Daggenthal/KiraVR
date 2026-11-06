// KiraVR: Textured quad for the in-VR performance overlay. The texture holds
// straight (non-premultiplied) alpha and is blended over the stream.

struct PushConstant {
    transform: mat4x4f,
    opacity: f32,
}
var<push_constant> pc: PushConstant;

@group(0) @binding(0) var overlay_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var result: VertexOutput;

    result.uv = vec2f(f32(vertex_index & 1), f32(vertex_index >> 1));
    result.position = pc.transform * vec4f(result.uv.x - 0.5, 0.5 - result.uv.y, 0.0, 1.0);

    return result;
}

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let color = textureSample(overlay_texture, overlay_sampler, uv);
    return vec4f(color.rgb, color.a * pc.opacity);
}
