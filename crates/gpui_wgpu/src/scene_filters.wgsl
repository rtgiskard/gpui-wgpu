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
    if params.mode == 1u {
        // Preserve the discrete three-sigma kernel. A linear fetch at the
        // weighted offset combines two neighboring taps, including at clamped
        // viewport edges; no downsampling or extra target is needed.
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
                let uv = (p + params.direction * offset) / params.viewport;
                color += textureSampleLevel(source, source_sampler, uv, 0.0) * weight;
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
