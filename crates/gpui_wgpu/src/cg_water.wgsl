// One analytic perspective plane, not scene/depth reflection. All temporal
// frequencies are integers, so every wave closes at exactly one phase cycle.
struct Placement { bounds: vec4<f32>, clip: vec4<f32>, viewport_phase: vec4<f32>, controls: vec4<f32> }
@group(0) @binding(0) var<uniform> p: Placement;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
    let corners = array<vec2<f32>, 6>(vec2(0.,0.),vec2(1.,0.),vec2(0.,1.),vec2(0.,1.),vec2(1.,0.),vec2(1.,1.));
    let uv = corners[index];
    let xy = p.bounds.xy + uv * p.bounds.zw;
    return Vertex(vec4(xy / p.viewport_phase.xy * vec2(2.,-2.) + vec2(-1.,1.),0.,1.),uv);
}
fn sky(direction: vec3<f32>, day: f32) -> vec3<f32> {
    let elevation = clamp(direction.y, 0., 1.);
    let night = mix(vec3(.075,.12,.19),vec3(.009,.023,.065),sqrt(elevation));
    let daylight = mix(vec3(.72,.83,.86),vec3(.16,.43,.68),sqrt(elevation));
    return mix(night, daylight, day);
}
// Analytic spatial derivative of six differently oriented scales. Pixel-footprint
// attenuation suppresses high-frequency horizon aliasing without geometry grids.
fn wave_gradient(xz: vec2<f32>, footprint: f32, phase: f32) -> vec2<f32> {
    let directions = array<vec2<f32>,6>(vec2(.94,.342),vec2(-.6,.8),vec2(.28,.96),vec2(.8,-.6),vec2(-.96,.28),vec2(.53,.848));
    let frequencies = array<f32,6>(.72,1.31,2.47,4.39,8.13,13.7);
    let strengths = array<f32,6>(.16,.10,.065,.038,.022,.012);
    let speeds = array<f32,6>(1.,-2.,3.,-4.,5.,-7.);
    var gradient = vec2(0.);
    for (var i = 0u; i < 6u; i += 1u) {
        let k = frequencies[i];
        let fade = 1. - smoothstep(.5,2.5,footprint*k);
        gradient += directions[i] * strengths[i] * k * cos(dot(directions[i],xz)*k + phase*6.28318530718*speeds[i]) * fade;
    }
    return gradient;
}
// One localized impact's height and analytic gradient in aspect-correct surface
// coordinates. Sixteen slots each replace their impact twice per phase cycle;
// an epoch-dependent seed changes position, arrival, size and strength only while
// both the height and its temporal derivative are zero.
fn still_event(point: vec2<f32>, aspect: f32, phase: f32, footprint: f32, slot: u32) -> vec3<f32> {
    let offsets = array<f32, 16>(
        0., .071, .137, .263, .318, .329, .447, .506,
        .571, .598, .706, .733, .812, .884, .927, .963);
    let clock = phase * 2. + offsets[slot];
    let local_age = fract(clock);
    let event = f32(slot * 2u + u32(floor(clock)) % 2u);
    // Arithmetic hash: no texture, history, or trigonometric hashing.
    var seed = fract(vec3(event + 1.) * vec3(.1031, .1030, .0973));
    seed += vec3(dot(seed, seed.yxz + vec3(33.33)));
    let random = fract((seed.xxy + seed.yzz) * seed.zyx);
    let arrival = .02 + .16 * fract(random.x * 7.13 + random.y * 3.17);
    let lifetime = .68 + .10 * random.z;
    let age = (local_age - arrival) / lifetime;
    if age <= 0. || age >= 1. { return vec3(0.); }
    let center = vec2(.035, .055) + random.xy * vec2(.93, .89);
    let size = .75 + .60 * random.z;
    // Horizontal perspective ellipses remain complete wavefronts, not warped
    // lobes. Their scale and mild foreshortening vary with the impact depth.
    let metric = vec2(1., 1. / (.68 + .22 * center.y));
    let delta = (point - center * vec2(aspect, 1.)) * metric;
    let k = 6.28318530718 / (.056 * size);
    // Uniform pixel footprint in this ellipse's metric; remove the carrier at
    // Nyquist rather than letting distant/small rings shimmer or beat together.
    let attenuation = 1. - smoothstep(1., 3.14159265359, k * footprint * metric.y);
    if attenuation <= 0. { return vec3(0.); }
    let radius = sqrt(dot(delta, delta) + .000004);
    let front = size * (.014 + .20 * age);
    let width = .088 * size;
    let band = (radius - front) / width;
    if abs(band) >= 1. { return vec3(0.); }
    // A compact C2 packet contains about three oscillations across its
    // support. Both carrier and envelope travel outward at the same speed;
    // there is no standing breathing ring or broad Gaussian donut underneath.
    let support = 1. - band * band;
    let envelope = support * support * support;
    let envelope_gradient = -6. * band * support * support / width;
    let carrier = k * (radius - front);
    let crest = cos(carrier);
    let trough = sin(carrier);
    let core = .025 * size;
    let spreading = inverseSqrt(1. + radius / core);
    let life = smoothstep(0., .12, age) * (1. - smoothstep(.65, 1., age));
    let strength = (.0084 + .0049 * fract(random.z * 5.73 + random.y))
        * life * attenuation / (1. + 2. * age);
    let height = strength * spreading * envelope * crest;
    // Differentiate the envelope, cylindrical spreading and carrier together.
    // Summing these gradients before shading makes intersections one surface.
    let radial_gradient = strength * spreading
        * ((envelope_gradient - .5 * envelope * spreading * spreading / core) * crest
            - envelope * k * trough);
    let gradient = radial_gradient * delta / radius * metric;
    return vec3(height, gradient);
}

fn still_surface(point: vec2<f32>, aspect: f32, phase: f32, footprint: f32) -> vec3<f32> {
    var surface = vec3(0.);
    for (var slot = 0u; slot < 16u; slot += 1u) {
        surface += still_event(point, aspect, phase, footprint, slot);
    }
    return surface;
}

fn calm_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let amplitude = p.viewport_phase.w;
    // Canonical phase makes the API's exact 0/1 endpoints identical. Each event
    // has a C1 lifetime with silent margins on both sides of its epoch reset.
    let phase = select(0., fract(p.viewport_phase.z), amplitude > 0.);
    let point = uv * vec2(aspect, 1.);
    let footprint = 1. / max(p.bounds.w, 1.);
    let surface = still_surface(point, aspect, phase, footprint) * (amplitude * 2.);
    // Coherent overlaps can add slopes steeper than a shallow calm surface.
    // Bound the combined optical slope smoothly (limit .45, identity to first
    // order at zero) instead of clipping individual impacts or brightness.
    // Normal and refraction share this slope, so neither optical path can turn
    // a steep intersection into an unbounded bed displacement.
    let slope = surface.yz * inverseSqrt(1. + dot(surface.yz, surface.yz) / (.45 * .45));
    let normal = normalize(vec3(-slope.x, 1., -slope.y));
    // One disturbance drives both bed refraction and reflected light. Only this
    // procedural shallow bed is refracted, never album artwork or UI pixels.
    // A shallow optical path keeps ripple slopes from sweeping the procedural
    // bed through several caustic bands within a tiny phase advance.
    let bed = point - slope * .45 + vec2(0., surface.x * .6);
    let depth = smoothstep(-1.5, 1.5,
        sin(bed.x * 2.8 + bed.y * 1.9) + .5 * cos(bed.y * 4.1 - bed.x));
    let shallow = mix(vec3(.12,.36,.37), vec3(.38,.64,.60), day);
    let deep = mix(vec3(.035,.17,.23), vec3(.17,.40,.48), day);
    var color = mix(shallow, deep, depth * .7 + uv.y * .15);
    let incident = normalize(vec3((uv.x - .5) * .16, -1., .45 + (uv.y - .5) * .12));
    let reflected = reflect(incident, normal);
    // A soft environment horizon avoids sqrt(elevation)'s infinite derivative
    // at grazing reflection while leaving the other styles' sky unchanged.
    let sky_elevation = .12 + .88 * smoothstep(0., 1., reflected.y);
    let reflected_sky = sky(vec3(reflected.x, sky_elevation, reflected.z), day);
    color = mix(color, reflected_sky, .12 + .06 * p.controls.x);
    // A broad reflected sky opening, not emissive height/ring masks or a sharp
    // specular sun. Its angular edge lets small slopes remain visible under tint.
    let broad_light = smoothstep(-.24, .24,
        dot(reflected.xz, vec2(-.8, .6)) - .24);
    // Broad, differentiable caustic bands: a narrow abs(sin)^10 crest would
    // amplify the moving refraction field into sub-frame brightness spikes.
    let field = bed * vec2(5., 7.);
    let caustic = pow(.5 + .5 * cos(field.x + sin(field.y)), 4.)
        + .6 * pow(.5 + .5 * cos(field.y + sin(field.x)), 4.);
    let light_color = mix(vec3(.42,.65,.70), vec3(.72,.88,.80), day);
    // Amplitude already controls the normal and refraction. Applying it again
    // here would hide low-amplitude motion beneath application panel tint.
    color += light_color * (broad_light * (.22 + .38 * p.controls.x)
        + caustic * (.06 + .12 * p.controls.x));
    return clamp(color, vec3(0.), vec3(1.));
}
// Directional advection, not radial packets: long refracted filaments slide
// downstream while broad shallow-bed colors remain anchored in place.
fn stream_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let a = p.viewport_phase.w;
    let t = select(0., p.viewport_phase.z * 6.2831853, a > 0.);
    let q = uv * vec2(aspect, 1.);
    let bend = sin(q.x * 4. - t) * .16 + sin(q.x * 7. - t * 2.) * .06;
    let bed = .5 + .5 * sin(q.y * 3.5 + sin(q.x * 2.1) + a * bend);
    let deep = mix(vec3(.025,.16,.19), vec3(.12,.40,.39), day);
    let clear = mix(vec3(.10,.36,.32), vec3(.40,.65,.53), day);
    var color = mix(deep, clear, bed * .75 + .1);
    let lane = q.y * 22. + sin(q.x * 2.5 - t) * a * 2.4;
    let facets = .5 + .5 * cos(q.x * 9. - t * 3. + sin(q.y * 8.));
    let crest = pow(.5 + .5 * sin(lane + facets * a * 2.), 16.);
    let weave = pow(.5 + .5 * cos(q.y * 34. + q.x * 3. - t * 2.), 24.);
    color += vec3(.58,.95,.80) * a * (.12 + p.controls.x * .30)
        * (crest * (.4 + .6 * facets) + weave * .20);
    return color;
}

// A wide reflected sky, not a sun column. Wind breaks the reflection into thin
// horizontal marks with quieter intervals between gusts.
fn lake_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let a = p.viewport_phase.w;
    let t = select(0., p.viewport_phase.z * 6.2831853, a > 0.);
    let water = smoothstep(.22, .30, uv.y);
    let distance = max(uv.y - .24, 0.);
    let gust = .55 + .45 * pow(.5 + .5 * sin(t), 2.);
    let wave = sin(uv.y * 90. + sin(uv.x * 9. + t) * 1.5 - t * 3.);
    let reflected_y = abs(uv.y - .26) + a * wave * .055 * water * gust;
    let upper = mix(vec3(.13,.23,.33), vec3(.46,.63,.70), day);
    let horizon = mix(vec3(.28,.32,.37), vec3(.70,.73,.72), day);
    var color = mix(horizon, upper, smoothstep(0., .65, reflected_y));
    let cloud = exp(-pow((reflected_y - .11 - .025 * sin(uv.x * 4.)), 2.) * 480.);
    color += mix(vec3(.09,.07,.08), vec3(.12,.09,.08), day) * cloud;
    color = mix(color, color * vec3(.56,.80,.87), water * .60);
    let crest = pow(.5 + .5 * wave, 10.) * (.5 + .5 * sin(uv.x * 20. + t));
    color += vec3(.60,.82,.88) * a * crest * distance * gust
        * (.45 + p.controls.x * .55);
    return color;
}

fn rain_impact(uv: vec2<f32>, center: vec2<f32>, aspect: f32, age: f32) -> f32 {
    let d = (uv - center) * vec2(aspect, 2.1);
    let r = length(d);
    let front = .012 + .23 * age;
    let band = (r - front) / (.006 + .008 * age);
    let life = smoothstep(0., .10, age) * (1. - smoothstep(.35, .85, age));
    let facing = .55 + .45 * d.y / max(r, .001);
    return exp(-band * band * .15) * cos(band * 1.7) * life * facing;
}

fn rain_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let a = p.viewport_phase.w;
    let phase = select(0., p.viewport_phase.z, a > 0.);
    let impacts = rain_impact(uv, vec2(.22,.34), aspect, fract(phase * 2.))
        + rain_impact(uv, vec2(.67,.61), aspect, fract(phase * 2. + .37))
        + rain_impact(uv, vec2(.43,.78), aspect, fract(phase * 2. + .71))
        + rain_impact(uv, vec2(.83,.27), aspect, fract(phase * 2. + .19))
        + rain_impact(uv, vec2(.16,.88), aspect, fract(phase * 2. + .53));
    let overcast = exp(-pow(uv.x - .35 - uv.y * .35, 2.) * 5.);
    let deep = mix(vec3(.07,.19,.20), vec3(.27,.43,.43), day);
    let reflection = mix(vec3(.23,.34,.32), vec3(.60,.68,.63), day);
    var color = mix(deep, reflection, overcast * .65);
    color += vec3(.72,.90,.86) * max(impacts, 0.) * a * (.40 + p.controls.x * .65);
    color += vec3(.25,.40,.38) * min(impacts, 0.) * a * .16;
    return color;
}

fn leaf_shadow(q: vec2<f32>, center: vec2<f32>, angle: f32) -> f32 {
    let d = q - center;
    let rotated = vec2(dot(d, vec2(cos(angle), sin(angle))),
        dot(d, vec2(-sin(angle), cos(angle))));
    let lens = abs(rotated.x) / .16 + pow(rotated.y / .065, 2.);
    return 1. - smoothstep(.65, 1.25, lens);
}

fn garden_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let a = p.viewport_phase.w;
    let t = select(0., p.viewport_phase.z * 6.2831853, a > 0.);
    let q = uv * vec2(aspect, 1.);
    let sway = a * vec2(.065 * sin(t), .035 * cos(t * 2.));
    let bed = q + a * .025 * vec2(sin(q.y * 10. + t), cos(q.x * 9. - t));
    let pool = .5 + .5 * sin(bed.x * 3. + bed.y * 4.);
    var color = mix(mix(vec3(.06,.23,.20), vec3(.28,.49,.36), day),
        mix(vec3(.19,.40,.30), vec3(.54,.67,.44), day), pool * .8);
    // Only a few projected leaves: no texture assets, detailed tree or scenery.
    let shadow = leaf_shadow(bed, vec2(aspect * .12,.18) + sway, .6)
        + leaf_shadow(bed, vec2(aspect * .25,.26) + sway, -.6)
        + leaf_shadow(bed, vec2(aspect * .36,.10) + sway, .5)
        + leaf_shadow(bed, vec2(aspect * .79,.63) - sway, -.8)
        + leaf_shadow(bed, vec2(aspect * .89,.77) - sway, .5)
        + leaf_shadow(bed, vec2(aspect * .70,.84) - sway, -.4);
    color *= 1. - min(shadow, 1.) * .55;
    let light = pow(.5 + .5 * sin(bed.x * 18. + sin(bed.y * 13. + t)), 16.);
    color += vec3(.65,.93,.63) * light * a * (.15 + p.controls.x * .35);
    return color;
}

fn crystal_water(uv: vec2<f32>, aspect: f32, day: f32) -> vec3<f32> {
    let a = p.viewport_phase.w;
    let t = select(0., p.viewport_phase.z * 6.2831853, a > 0.);
    let q = uv * vec2(aspect, 1.);
    // Broad transparent laminae. Refraction moves only this procedural interior,
    // never the foreground; there are no chromatic fringes or metallic glints.
    let bend = a * (sin(q.x * 3.1 + t) * .50 + cos(q.y * 4.3 - t) * .35);
    let sheet = q.y + q.x * .38 + .13 * sin(q.x * 3.) + bend;
    let interior = smoothstep(.1, 1.5, sheet);
    let dark = mix(vec3(.12,.20,.25), vec3(.38,.53,.58), day);
    let clear = mix(vec3(.27,.36,.39), vec3(.68,.77,.77), day);
    var color = mix(dark, clear, interior * .85);
    let fold_a = exp(-pow((sheet - .60) / .055, 2.));
    let fold_b = exp(-pow((sheet - 1.23) / .085, 2.));
    let soft_light = .5 + .5 * sin(q.x * 2. - q.y * 3. + bend * 3.);
    color += vec3(.68,.84,.88) * (.06 + p.controls.x * .22)
        * (fold_a + fold_b * .6) * (.35 + .65 * soft_light);
    color -= vec3(.045,.055,.06) * exp(-pow((sheet - .68) / .11, 2.));
    return color;
}

@fragment fn fragment(input: Vertex) -> @location(0) vec4<f32> {
    if any(input.position.xy < p.clip.xy) || any(input.position.xy >= p.clip.xy + p.clip.zw) { discard; }
    let day = p.controls.y;
    let aspect = p.bounds.z / max(p.bounds.w,1.);
    if p.controls.w > .5 {
        var color: vec3<f32>;
        switch u32(p.controls.w) {
            case 2u: { color = stream_water(input.uv, aspect, day); }
            case 3u: { color = lake_water(input.uv, aspect, day); }
            case 4u: { color = rain_water(input.uv, aspect, day); }
            case 5u: { color = garden_water(input.uv, aspect, day); }
            case 6u: { color = crystal_water(input.uv, aspect, day); }
            default: { color = calm_water(input.uv, aspect, day); }
        }
        return vec4(color * p.controls.z, p.controls.z);
    }
    let ray = normalize(vec3((input.uv.x-.5)*aspect, .29-input.uv.y, 1.15));
    var color = sky(ray,day);
    if ray.y >= .002 { return vec4(color*p.controls.z,p.controls.z); }
    // Keep intersection finite at the horizon, including flat amplitude zero.
    let distance = 1.4 / max(-ray.y,.008);
    let xz = ray.xz * distance;
    let footprint = distance / max(p.bounds.w,1.) / max(-ray.y,.025);
    let gradient = wave_gradient(xz,footprint,p.viewport_phase.z) * p.viewport_phase.w;
    let normal = normalize(vec3(-gradient.x,1.,-gradient.y));
    let view = -ray;
    let reflected = reflect(ray,normal);
    let fresnel = .02 + .98 * pow(1.-clamp(dot(normal,view),0.,1.),5.);
    let transmission = exp(-vec3(.25,.09,.045) * (1.5 + distance*.24));
    let shallow = mix(vec3(.018,.13,.16),vec3(.06,.40,.38),day);
    let deep = mix(vec3(.004,.016,.045),vec3(.008,.075,.14),day);
    let body = mix(deep,shallow,transmission);
    let light = normalize(vec3(-.32,.34,.88));
    let half_vector = normalize(view + light);
    let glint = pow(max(dot(normal,half_vector),0.),180.) * 2.8 + pow(max(dot(reflected,light),0.),900.) * 1.8;
    let light_color = mix(vec3(.42,.56,.86),vec3(1.,.88,.61),day);
    let water = mix(body,sky(reflected,day),fresnel) + light_color * glint * p.controls.x;
    let haze = 1.-exp(-distance*.009);
    let surface = mix(water,sky(vec3(0.,0.,1.),day),haze);
    color = mix(color,surface,smoothstep(-.002,.009,-ray.y));
    let alpha = p.controls.z;
    return vec4(clamp(color,vec3(0.),vec3(1.))*alpha,alpha);
}
