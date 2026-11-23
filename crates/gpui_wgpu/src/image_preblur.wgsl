// Blurs one atlas image tile with the sprite Gaussian at its native resolution.
// Concatenated before `image_sampling.wgsl`.

struct AtlasBounds {
    origin: vec2<i32>,
    size: vec2<i32>,
}

struct PreblurParams {
    source: AtlasBounds,
    radius: f32,
    reflect: u32,
}

// The source tile, or the horizontal pass output for `fs_preblur_vertical`.
@group(0) @binding(0) var t_sprite: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: PreblurParams;

@vertex
fn vs_preblur(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

// Each output pixel center maps to the matching source texel center.
@fragment
fn fs_preblur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let center = vec2<f32>(params.source.origin) + position.xy;
    let color = image_gaussian(center, vec2<f32>(params.radius), params.reflect != 0u, params.source);
    return image_straight_alpha(color);
}

// Separable form of `fs_preblur`. Taps keep the same positions, clamping and
// reflection per axis; the horizontal pass samples rows at exact texel centers.
// Writes premultiplied color to an `Rgba16Float` texture of the tile's size.
@fragment
fn fs_preblur_horizontal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let center = vec2<f32>(params.source.origin) + position.xy;
    var result = vec4<f32>(0.0);
    for (var i = -4; i <= 4; i += 1) {
        var sample_position = vec2<f32>(center.x + f32(i) * 0.25 * params.radius, center.y);
        if (params.reflect != 0u) {
            sample_position.x = image_reflect(sample_position, params.source).x;
        }
        result += image_premultiplied_sample(sample_position, params.source) * image_gaussian_weight(i);
    }
    return result;
}

// Blurs the premultiplied horizontal result vertically, interpolating between
// its rows as `image_premultiplied_sample` does between source rows.
@fragment
fn fs_preblur_vertical(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let bounds = params.source;
    let center = vec2<f32>(bounds.origin) + position.xy;
    let column = i32(position.x);
    let maximum = bounds.origin.y + max(bounds.size.y, 1) - 1;
    var result = vec4<f32>(0.0);
    for (var i = -4; i <= 4; i += 1) {
        var sample_position = vec2<f32>(center.x, center.y + f32(i) * 0.25 * params.radius);
        if (params.reflect != 0u) {
            sample_position.y = image_reflect(sample_position, bounds).y;
        }
        let texel = clamp(sample_position.y - 0.5, f32(bounds.origin.y), f32(maximum));
        let base = i32(floor(texel));
        let fraction = fract(texel);
        var color = textureLoad(t_sprite, vec2<i32>(column, base - bounds.origin.y), 0);
        if (fraction > 0.0) {
            let next = min(base + 1, maximum) - bounds.origin.y;
            color = mix(color, textureLoad(t_sprite, vec2<i32>(column, next), 0), fraction);
        }
        result += color * image_gaussian_weight(i);
    }
    return image_straight_alpha(result);
}
