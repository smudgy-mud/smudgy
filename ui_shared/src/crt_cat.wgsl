// Animated CRT cat for the no-session empty state.
//
// The artwork is sized in logical points (8 pt per source pixel) so the cat
// keeps a consistent on-screen size across DPI, while the phosphor triads and
// scanline beam live in *physical* screen space — each source pixel spans
// several CRT elements instead of becoming one glowing tile.

struct Uniforms {
    // Physical size of the widget, in pixels.
    resolution: vec2<f32>,
    // Seconds since the empty state last became visible; drives all animation.
    time: f32,
    // Device scale factor (physical pixels per logical point).
    scale: f32,
    // Physical top-left of the widget within the frame, in pixels.
    origin: vec2<f32>,
    // 1.0 when the render target is an sRGB format (so the final colour is
    // converted to linear, cancelling the hardware's linear->sRGB store), else 0.0.
    linearize: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> U: Uniforms;

// The 15x12 source artwork, one i32 of column bits per row. Bit 0 is the
// leftmost pixel; rows are stored top-to-bottom.
var<private> BLACK: array<i32, 12> = array<i32, 12>(
    0x0630, 0x0F78, 0x0BD8, 0x19CC,
    0x1C66, 0x3BD6, 0x3BD3, 0x3C60,
    0x3001, 0x300C, 0x381E, 0x0FF0
);

var<private> CHARCOAL: array<i32, 12> = array<i32, 12>(
    0x0000, 0x0000, 0x0420, 0x0630,
    0x0398, 0x0428, 0x042C, 0x039C,
    0x0EFE, 0x0FF0, 0x07E0, 0x0000
);

var<private> WHISKERS: array<i32, 12> = array<i32, 12>(
    0x0000, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x4003,
    0x0000, 0x4003, 0x0000, 0x0000
);

// GLSL `mod` (always non-negative for positive y); WGSL `%` is C-style remainder.
fn gmod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn blink_open(t: f32) -> f32 {
    if (t < 3.0) { return 1.0; } // begin quietly
    let cycle = gmod(t - 3.0, 19.0);
    // Two blinks per cycle — one mid-cycle, one straddling the wrap — giving
    // alternating rests of roughly 8 and 11 seconds.
    let b = min(abs(cycle - 8.0), min(abs(cycle - 18.9), abs(cycle - 0.1)));
    // Brisk close, a beat fully shut, brisk open. The flat region below
    // 0.10 also fuses the wrap pair into one long sleepy blink; the original's
    // zero-width closed edge let the lids flicker ~10% open between the pair,
    // reading as a rapid double-blink.
    return smoothstep(0.10, 0.30, b);
}

// Time constant for a retiring tongue pixel's decay back into the fur.
// Pretend the eyes' pleasing 0.35 s lid fade is the tail of an exponential
// phosphor decay that has dropped to ~2% (e^-4) by the time it vanishes:
// tau = 0.35 / 4. The tongue rides the same phosphor, but its red starts at
// only ~0.48 of white's Rec. 601 luma — already partway down the curve — so
// its glow falls fast (half gone in ~60 ms) with only a short dim tail.
const TONGUE_TAU: f32 = 0.0875;

// A tongue frame's intensity: full through [start, end), then exponential
// decay. Onset stays hard — the lick pops out, the fade is for the way home.
fn tongue_level(lt: f32, start: f32, end: f32) -> f32 {
    if (lt < start) { return 0.0; }
    if (lt < end) { return 1.0; }
    return exp(-(lt - end) / TONGUE_TAU);
}

fn sprite(p: vec2<i32>, t: f32) -> vec4<f32> {
    if (p.x < 0 || p.x >= 15 || p.y < 0 || p.y >= 12) {
        return vec4<f32>(0.0);
    }
    let mask: i32 = 1 << u32(p.x);
    var c = vec3<f32>(0.0);
    var a = 0.0;

    // Literal source colors; the phosphor character comes from bleed, not tinting.
    if ((BLACK[p.y] & mask) != 0) {
        c = vec3<f32>(0.0);
        a = 1.0;
    }
    if ((CHARCOAL[p.y] & mask) != 0) {
        c = vec3<f32>(8.0 / 255.0);
        a = 1.0;
    }
    if ((WHISKERS[p.y] & mask) != 0) {
        c = vec3<f32>(105.0, 106.0, 106.0) / 255.0;
        a = 1.0;
    }

    // Original pink nose at source pixel (8, 8).
    if (all(p == vec2<i32>(8, 8))) {
        c = vec3<f32>(196.0, 140.0, 179.0) / 255.0;
        a = 1.0;
    }

    let open = blink_open(t);
    let eye = (p.x == 5 || p.x == 10) && (p.y == 5 || p.y == 6);
    if (eye) {
        c = mix(c, vec3<f32>(1.0), open);
    }

    // A leisurely lick every 44 seconds, delayed so the opening pose is
    // restful, lingering on the final tongue-out frame. Each frame's pixel
    // fades back into the fur when it retires (see `tongue_level`), so the
    // earlier frames trail the lick like phosphor afterglow.
    var lt = 99.0;
    if (t >= 7.0) {
        lt = gmod(t - 7.0, 44.0);
    }
    var tongue = 0.0;
    if (all(p == vec2<i32>(6, 9))) { tongue = tongue_level(lt, 0.0, 0.18); }
    if (all(p == vec2<i32>(7, 10))) { tongue = tongue_level(lt, 0.18, 0.36); }
    if (all(p == vec2<i32>(8, 10))) { tongue = tongue_level(lt, 0.36, 1.86); }
    if (tongue > 0.0) {
        c = mix(c, vec3<f32>(237.0, 74.0, 74.0) / 255.0, tongue);
        a = max(a, tongue);
    }
    return vec4<f32>(c, a);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = step(vec3<f32>(0.04045), c);
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return mix(low, high, cutoff);
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vid << 1u) & 2u);
    let y = f32(vid & 2u);
    return vec4<f32>(vec2<f32>(x, y) * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // Physical pixels, top-origin. The source rows are stored top-to-bottom,
    // so a downward y maps straight onto row indices — the original's
    // bottom-origin `11 - y` flip is already folded in.
    let res = U.resolution;
    let frag_coord = frag.xy - U.origin;
    let t = U.time;

    // Literal nearest-neighbour enlargement of the 15x12 source: 8 logical
    // points (8 * scale physical pixels) per source pixel, centred in the
    // widget. Phosphors below remain in physical screen space, so each source
    // pixel contains multiple CRT elements instead of becoming one glowing tile.
    let src_px = 8.0 * U.scale;
    let gp = (frag_coord - 0.5 * res) / src_px + vec2<f32>(7.5, 6.0);
    let cell = vec2<i32>(floor(gp));
    let within = fract(gp) - vec2<f32>(0.5);

    let core = sprite(cell, t);

    // Contiguous square source pixels, with about one physical pixel of
    // horizontal analog bandwidth softness at colour transitions.
    var col = core.rgb;
    var alpha = core.a;
    let left = sprite(cell + vec2<i32>(-1, 0), t);
    let right = sprite(cell + vec2<i32>(1, 0), t);
    let dist_left_px = (within.x + 0.5) * src_px;
    let dist_right_px = (0.5 - within.x) * src_px;
    let bleed_left = exp(-1.15 * dist_left_px * dist_left_px) * left.a;
    let bleed_right = exp(-1.15 * dist_right_px * dist_right_px) * right.a;
    col += left.rgb * bleed_left * 0.10;
    col += right.rgb * bleed_right * 0.10;
    alpha = max(alpha, max(bleed_left, bleed_right) * 0.10);

    // Beam bloom is intensity-thresholded: black and charcoal do not glow.
    let hot_core = smoothstep(0.48, 0.88, max(core.r, max(core.g, core.b)));
    let hot_left = smoothstep(0.48, 0.88, max(left.r, max(left.g, left.b)));
    let hot_right = smoothstep(0.48, 0.88, max(right.r, max(right.g, right.b)));
    col += core.rgb * hot_core * 0.055;
    col += left.rgb * hot_left * bleed_left * 0.16;
    col += right.rgb * hot_right * bleed_right * 0.16;
    alpha = max(alpha, max(hot_left * bleed_left, hot_right * bleed_right) * 0.16);

    // Filmic-ish phosphor response and display gamma.
    col = 1.0 - exp(-col * 1.45);
    col = pow(max(col, vec3<f32>(0.0)), vec3<f32>(0.90));

    // Close-up CRT shadow mask. This lives in physical screen space (raw
    // framebuffer x) rather than the 15x12 artwork grid, so connected source
    // pixels remain connected and the grille stays put as the window moves.
    let phosphor = gmod(floor(frag.x), 3.0);
    var triad = vec3<f32>(0.68, 0.68, 1.34);
    if (phosphor < 1.0) {
        triad = vec3<f32>(1.34, 0.68, 0.68);
    } else if (phosphor < 2.0) {
        triad = vec3<f32>(0.68, 1.34, 0.68);
    }
    // Stable, low-contrast beam profile — no scrolling or temporal modulation.
    let beam_y = fract(frag.y / 3.0) - 0.5;
    let beam = 0.87 + 0.13 * exp(-14.0 * beam_y * beam_y);
    col *= triad * beam;

    // Bright phosphors get a restrained emission kick without making dark fur glow.
    let excited = smoothstep(vec3<f32>(0.52), vec3<f32>(0.92), col);
    col += excited * col * 0.10;

    // A small white core keeps the source palette legible through the mask.
    col += core.rgb * core.a * 0.035;

    alpha = clamp(alpha, 0.0, 1.0);

    // Premultiplied output: `col` is the emitted light (the halo terms carry
    // their own attenuation), so the pipeline blends One / OneMinusSrcAlpha
    // and fully transparent fragments leave the UI beneath untouched.
    var outc = col;
    if (U.linearize > 0.5) {
        outc = srgb_to_linear(outc);
    }
    return vec4<f32>(outc, alpha);
}
