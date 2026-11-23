struct FilterParams {
    viewport: vec2<f32>,
    direction: vec2<f32>,
    bounds: vec4<f32>,
    clip: vec4<f32>,
    sigma: f32,
    feather: f32,
    mode: u32,
    pad: u32,
}

// Large backdrops blur a grid of 4x4 blocks. Keep in sync with scene_filters.rs.
const DOWNSAMPLE: f32 = 4.0;

@group(0) @binding(0) var<uniform> params: FilterParams;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

// The same cached target bind group is reused for the unchanged parent.
@group(1) @binding(1) var parent: texture_2d<f32>;

@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = position.xy;
    let outside = any(p < params.clip.xy) || any(p >= params.clip.zw);
    if params.mode == 3u {
        if outside {
            return textureSampleLevel(parent, source_sampler, p / params.viewport, 0.0);
        }
        let distance = min(p.y - params.bounds.y, params.bounds.w - p.y);
        let mask = smoothstep(0.0, max(params.feather, 0.0001), distance);
        let color = textureSampleLevel(source, source_sampler, p / params.viewport, 0.0);
        if mask == 1.0 { return color; }
        let backdrop = textureSampleLevel(parent, source_sampler, p / params.viewport, 0.0);
        return mix(backdrop, color, mask);
    }
    if outside { discard; }
    if params.mode == 4u {
        // Average a 4x4 block with four bilinear fetches. The guard cells on each
        // edge hold the exact border line, so clamped blur taps replicate it.
        let cell = floor(p);
        let base = (cell - vec2<f32>(1.0)) * DOWNSAMPLE;
        let last = ceil(params.viewport / DOWNSAMPLE);
        let border_low = cell < vec2<f32>(1.0);
        let border_high = cell > last;
        let border = select(vec2<f32>(0.5), params.viewport - vec2<f32>(0.5), border_high);
        let a = select(base + vec2<f32>(1.0), border, border_low | border_high);
        let b = select(base + vec2<f32>(3.0), border, border_low | border_high);
        let color = textureSampleLevel(source, source_sampler, a / params.viewport, 0.0)
            + textureSampleLevel(source, source_sampler, vec2<f32>(b.x, a.y) / params.viewport, 0.0)
            + textureSampleLevel(source, source_sampler, vec2<f32>(a.x, b.y) / params.viewport, 0.0)
            + textureSampleLevel(source, source_sampler, b / params.viewport, 0.0);
        return color * 0.25;
    }
    if params.mode == 5u {
        // Grid cell g + 1 covers full-resolution pixels [4g, 4g + 4).
        let grid = p / DOWNSAMPLE + vec2<f32>(1.0);
        return textureSampleLevel(source, source_sampler, grid / vec2<f32>(textureDimensions(source)), 0.0);
    }
    if params.mode == 1u {
        // Preserve the discrete three-sigma kernel. A linear fetch at the
        // weighted offset combines two neighboring taps. The viewport is the
        // sampled extent, which may be smaller than the source texture.
        let size = vec2<f32>(textureDimensions(source));
        let last_center = params.viewport - vec2<f32>(0.5);
        let radius = i32(ceil(params.sigma * 3.0));
        var color = vec4<f32>(0.0);
        var total = 0.0;
        for (var i = -radius; i <= radius; i += 2) {
            let x = f32(i);
            let first = exp(-0.5 * x * x / (params.sigma * params.sigma));
            var second = 0.0;
            if i < radius {
                let next = x + 1.0;
                second = exp(-0.5 * next * next / (params.sigma * params.sigma));
            }
            let weight = first + second;
            // Very small sigmas underflow away from the center. Do not divide
            // zero by zero or issue a fetch whose contribution is zero.
            if weight > 0.0 {
                let offset = x + second / weight;
                let sample_pos = min(p + params.direction * offset, last_center);
                color += textureSampleLevel(source, source_sampler, sample_pos / size, 0.0) * weight;
                total += weight;
            }
        }
        return color / total;
    }
    var color = textureSampleLevel(source, source_sampler, p / params.viewport, 0.0);
    if params.mode == 2u {
        let distance = min(p.y - params.bounds.y, params.bounds.w - p.y);
        let mask = smoothstep(0.0, max(params.feather, 0.0001), distance);
        color *= mask;
    }
    return color;
}
