// Source-image sampling shared by polychrome sprites and atlas preblur. The
// including module declares `t_sprite` and `AtlasBounds`.

// Interpolate in premultiplied space so transparent source RGB cannot fringe.
// Clamp before conversion to integer coordinates: no tap may leave this tile.
fn image_premultiplied_texel(position: vec2<i32>) -> vec4<f32> {
    let color = textureLoad(t_sprite, position, 0);
    return vec4<f32>(color.rgb * color.a, color.a);
}

fn image_premultiplied_sample(position: vec2<f32>, bounds: AtlasBounds) -> vec4<f32> {
    let maximum = bounds.origin + max(bounds.size, vec2<i32>(1)) - vec2<i32>(1);
    let texel = clamp(position - vec2<f32>(0.5), vec2<f32>(bounds.origin), vec2<f32>(maximum));
    let base = vec2<i32>(floor(texel));
    let next = min(base + vec2<i32>(1), maximum);
    let fraction = fract(texel);
    var top = image_premultiplied_texel(base);
    if (fraction.x > 0.0) {
        top = mix(top, image_premultiplied_texel(vec2<i32>(next.x, base.y)), fraction.x);
    }
    // Exact texel centers and clamped/one-pixel axes need no duplicate loads.
    if (fraction.y == 0.0) {
        return top;
    }
    var bottom = image_premultiplied_texel(vec2<i32>(base.x, next.y));
    if (fraction.x > 0.0) {
        bottom = mix(bottom, image_premultiplied_texel(next), fraction.x);
    }
    return mix(top, bottom, fraction.y);
}

fn image_straight_alpha(color: vec4<f32>) -> vec4<f32> {
    if (color.a > 0.0) {
        return vec4<f32>(color.rgb / color.a, color.a);
    }
    return vec4<f32>(0.0);
}

fn image_reflect(position: vec2<f32>, bounds: AtlasBounds) -> vec2<f32> {
    let minimum = vec2<f32>(bounds.origin) + vec2<f32>(0.5);
    let extent = max(vec2<f32>(bounds.size) - vec2<f32>(1.0), vec2<f32>(0.000001));
    // Triangular repeat is continuous at both boundaries, including negative UVs.
    return minimum + (vec2<f32>(1.0) - abs(fract((position - minimum) / (2.0 * extent)) * 2.0 - vec2<f32>(1.0))) * extent;
}

// Weight of tap `i` in -4..=4 of the normalized +/-3 sigma quadrature. Tap `i`
// lies `i * 0.25 * radius` texels from the center.
fn image_gaussian_weight(i: i32) -> f32 {
    let weights = array<f32, 9>(0.0033257271, 0.0238179220, 0.0971919923, 0.2259781525, 0.2993724122, 0.2259781525, 0.0971919923, 0.0238179220, 0.0033257271);
    return weights[u32(i + 4)];
}

// Fixed 9x9 Gaussian quadrature over +/-3 sigma, `radius` texels per axis, in
// premultiplied space. Bounded 324 texel reads; taps beyond the tile clamp to its
// edge texels or, with `reflect`, mirror about its edge texel centers. Weights,
// bilinear interpolation, clamping and reflection all factor per axis, so the
// atlas preblur may evaluate this as two 1D passes.
fn image_gaussian(center: vec2<f32>, radius: vec2<f32>, reflect: bool, bounds: AtlasBounds) -> vec4<f32> {
    var result = vec4<f32>(0.0);
    for (var y = -4; y <= 4; y += 1) {
        for (var x = -4; x <= 4; x += 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * 0.25;
            let weight = image_gaussian_weight(x) * image_gaussian_weight(y);
            var sample_position = center + offset * radius;
            if (reflect) {
                sample_position = image_reflect(sample_position, bounds);
            }
            result += image_premultiplied_sample(sample_position, bounds) * weight;
        }
    }
    return result;
}
