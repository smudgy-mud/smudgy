// TextEffect ABI v1. Coordinates are logical pixels relative to the text fragment.
struct SmudgyTextFrame {
    origin: vec2f, resolution: vec2f, surface: vec2f, texture_size: vec2f,
    text_size: vec2f, scale: f32, outset: f32,
    time: f32, progress: f32, seed: f32, linearize: u32,
    background: vec4f,
    effect_scale: f32, duration: f32, envelope: f32, replaces_text: u32,
    paint_offset: vec2f, capture_offset: vec2f,
}
@group(0) @binding(0) var<uniform> text: SmudgyTextFrame;
@group(0) @binding(1) var smudgy_glyphs: texture_2d<f32>;
@group(0) @binding(2) var smudgy_sampler: sampler;
// Cached visual glyphs, in local logical pixels. Zero-area ink means whitespace.
// A ligature is one shaped glyph; cluster.xy are source UTF-8 byte offsets.
struct SmudgyGlyph { advance: vec4f, ink: vec4f, cluster: vec4u }
struct SmudgyGlyphTable { info: vec4u, items: array<SmudgyGlyph, 256> }
@group(0) @binding(3) var<uniform> smudgy_geometry: SmudgyGlyphTable;
// A shared, GPU-generated tiled noise atlas with filtered mip levels.
@group(0) @binding(4) var smudgy_noise: texture_2d<f32>;
@group(0) @binding(5) var smudgy_noise_sampler: sampler;
// Each integer Z layer has an independently hashed atlas origin. Two filtered
// lookups interpolate adjacent layers; no channel encodes another layer's data.
fn noiseLayerOrigin(layer: f32) -> vec2f {
    var key = bitcast<u32>(i32(layer)) * 747796405u + 2891336453u;
    key = ((key >> ((key >> 28u) + 4u)) ^ key) * 277803737u;
    key = (key >> 22u) ^ key;
    return vec2f(f32(key & 255u), f32((key >> 8u) & 255u));
}
fn noise3D(position: vec3f, lod: f32) -> f32 {
    let cell = floor(position);
    let phase = fract(position);
    // Quintic interpolation has zero first and second derivatives at cell edges.
    let ease = phase * phase * phase * (phase * (phase * 6.0 - 15.0) + 10.0);
    let pixel = cell.xy + ease.xy + vec2f(0.5);
    let level = clamp(lod, 0.0, 8.0);
    let near = textureSampleLevel(smudgy_noise, smudgy_noise_sampler,
        (pixel + noiseLayerOrigin(cell.z)) / 256.0, level).r;
    let far = textureSampleLevel(smudgy_noise, smudgy_noise_sampler,
        (pixel + noiseLayerOrigin(cell.z + 1.0)) / 256.0, level).r;
    return mix(near, far, ease.z) * 2.0 - 1.0;
}
fn glyphCount() -> u32 { return smudgy_geometry.info.x; }
fn glyphsTruncated() -> bool { return smudgy_geometry.info.y != 0u; }
// Font baselines are captured with shaping, never estimated from the line box.
fn textBaseline() -> f32 { return bitcast<f32>(smudgy_geometry.info.z); }
fn glyphBaseline(index: u32) -> f32 {
    if index >= glyphCount() { return textBaseline(); }
    return bitcast<f32>(smudgy_geometry.items[index].cluster.w);
}
fn glyphAt(index: u32) -> SmudgyGlyph {
    if index >= glyphCount() { return SmudgyGlyph(vec4f(0.0), vec4f(0.0), vec4u(0u)); }
    return smudgy_geometry.items[index];
}

fn sampleTextLod(position: vec2f, lod: f32) -> vec4f {
    let uv = (position - text.capture_offset) * text.scale / text.texture_size;
    let level = clamp(lod, 0.0, f32(textureNumLevels(smudgy_glyphs) - 1u));
    let color = textureSampleLevel(smudgy_glyphs, smudgy_sampler, uv, level);
    return select(vec4f(0.0), color, all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0)));
}
fn sampleText(position: vec2f) -> vec4f { return sampleTextLod(position, 0.0); }
fn coverage(position: vec2f) -> f32 { return sampleText(position).a; }
// Optional instanced-quad return value; available to every imported program.
struct SmudgyFragment {
    position: vec2f, source: vec2f, perspective: f32, opacity: f32,
}
