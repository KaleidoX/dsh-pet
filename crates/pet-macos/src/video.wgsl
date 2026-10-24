@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (any(id.xy >= textureDimensions(source))) { return; }
    let color = textureLoad(source, vec2<i32>(id.xy), 0);
    // The pinned upstream pack contains straight RGB despite its premultiplied
    // metadata (verified against WebM). Convert once, on the GPU, for egui.
    // HEVC alpha quantization raises the transparent background to 1/255.
    let alpha = select(color.a, 0.0, color.a <= 1.0 / 255.0);
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(color.rgb * alpha, alpha));
}
