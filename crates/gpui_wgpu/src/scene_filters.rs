use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use gpui::{Bounds, DevicePixels, PrimitiveBatch, ScaledPixels, Scene, SceneFilter, Size};
use std::num::NonZeroU64;

const MAX_DEPTH: usize = gpui::SceneFilterLimits::MAX_DEPTH;
const MAX_FILTERS: usize = 64;
const PARAM_SLOTS: u64 = (MAX_FILTERS * 2 + 1) as u64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    viewport: [f32; 2],
    direction: [f32; 2],
    bounds: [f32; 4],
    clip: [f32; 4],
    sigma: f32,
    feather: f32,
    mode: u32,
    pad: u32,
}

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// Allocation-free validation shared by lazy filter setup and water preparation.
#[derive(Debug)]
pub(super) struct ScenePreparation {
    // Only admitted effects are prepared and drawn. Structural failures remain errors.
    pub has_filters: bool,
    pub has_water: bool,
    has_backdrop: bool,
    max_depth: usize,
    target_count: usize,
    snapshot_groups: u64,
}

impl ScenePreparation {
    pub fn scan(
        scene: &Scene,
        size: Size<DevicePixels>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        // Ordinary scenes need no ordered effect validation. Use the recorded
        // collections so clear and cached replay cannot leave stale presence.
        if !scene.has_scene_effects() {
            return Ok(Self {
                has_filters: false,
                has_water: false,
                has_backdrop: false,
                max_depth: 0,
                target_count: 0,
                snapshot_groups: 0,
            });
        }
        let mut depth = 0usize;
        let mut max_depth = 0;
        let mut count = 0;
        let mut water_count = 0;
        let mut has_backdrop = false;
        let mut group_markers = [0; MAX_DEPTH];
        let mut snapshot_groups = 0u64;
        let mut within_filter_limits = true;
        for batch in scene.batches() {
            match batch {
                PrimitiveBatch::CgWater(_) => water_count += 1,
                PrimitiveBatch::Filter(filter) => {
                    count += 1;
                    within_filter_limits &= count <= MAX_FILTERS;
                    match filter {
                        SceneFilter::BeginEdgeFade { .. } => {
                            if depth < MAX_DEPTH && count <= MAX_FILTERS {
                                group_markers[depth] = count - 1;
                            }
                            depth += 1;
                            max_depth = max_depth.max(depth);
                            within_filter_limits &=
                                depth <= scene.scene_filter_limits.max_depth.min(MAX_DEPTH);
                        }
                        SceneFilter::EndEdgeFade => {
                            ensure!(depth > 0, "unmatched scene filter group end");
                            depth -= 1;
                        }
                        SceneFilter::BackdropBlur { .. } => {
                            has_backdrop = true;
                            if within_filter_limits {
                                for &marker in &group_markers[..depth] {
                                    snapshot_groups |= 1u64 << marker;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        ensure!(depth == 0, "unclosed scene filter group");
        let target_count = max_depth + 2 * usize::from(has_backdrop);
        if count != 0 {
            ensure!(
                size.width.0 > 0 && size.height.0 > 0,
                "scene filters require a nonempty viewport"
            );
            let dimensions = [size.width.0 as u32, size.height.0 as u32];
            within_filter_limits &= dimensions
                .iter()
                .all(|&dimension| dimension <= device.limits().max_texture_dimension_2d);
            if within_filter_limits {
                let bytes = u64::from(dimensions[0])
                    .checked_mul(u64::from(dimensions[1]))
                    .and_then(|bytes| {
                        bytes.checked_mul(format.block_copy_size(None).unwrap_or(16) as u64)
                    })
                    .and_then(|bytes| bytes.checked_mul(target_count as u64));
                within_filter_limits =
                    bytes.is_some_and(|bytes| bytes <= scene.scene_filter_limits.max_target_bytes);
            }
        }
        Ok(Self {
            has_filters: count != 0 && within_filter_limits,
            has_water: water_count != 0 && water_count <= super::cg_water::MAX_PANELS,
            has_backdrop: has_backdrop && within_filter_limits,
            max_depth: if within_filter_limits { max_depth } else { 0 },
            target_count: if within_filter_limits {
                target_count
            } else {
                0
            },
            snapshot_groups: if within_filter_limits {
                snapshot_groups
            } else {
                0
            },
        })
    }
}

#[derive(Clone, Copy)]
enum Composite {
    Replace,
    Over,
    Mix(f32),
}

/// Per-device, renderer-owned filter resources. Nothing here owns application
/// images, scene nodes or CPU pixel data. Targets are reused until size changes.
pub(super) struct SceneFilters {
    layout: wgpu::BindGroupLayout,
    replace: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
    mix: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    stride: u64,
    next_slot: u32,
    targets: Vec<Target>,
    size: [u32; 2],
    // Only backdrop scenes need a sampled root; None denotes the final output.
    depth_targets: [Option<usize>; MAX_DEPTH + 1],
    scratch_index: usize,
    snapshot_groups: u64,
    format: wgpu::TextureFormat,
}

impl SceneFilters {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let stride =
            64u64.next_multiple_of(device.limits().min_uniform_buffer_offset_alignment as u64);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene filter parameters"),
            size: stride * PARAM_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene filter layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene filter pipeline layout"),
            bind_group_layouts: &[Some(&layout), Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene filter shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene_filters.wgsl").into()),
        });
        let pipeline = |blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("scene filter pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let replace = pipeline(None);
        let over = pipeline(Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING));
        let component = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::OneMinusConstant,
            operation: wgpu::BlendOperation::Add,
        };
        let mix = pipeline(Some(wgpu::BlendState {
            color: component,
            alpha: component,
        }));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene filter sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            layout,
            replace,
            over,
            mix,
            sampler,
            uniforms,
            stride,
            next_slot: 0,
            targets: Vec::with_capacity(MAX_DEPTH + 2),
            size: [0; 2],
            format,
            depth_targets: [None; MAX_DEPTH + 1],
            scratch_index: 0,
            snapshot_groups: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        preparation: &ScenePreparation,
        size: Size<DevicePixels>,
    ) {
        debug_assert!(preparation.has_filters);
        self.next_slot = 0;
        self.depth_targets = std::array::from_fn(|depth| {
            if preparation.has_backdrop {
                Some(depth)
            } else {
                depth.checked_sub(1)
            }
        });
        self.scratch_index = preparation.max_depth + 1;
        self.snapshot_groups = preparation.snapshot_groups;
        let dimensions = [size.width.0 as u32, size.height.0 as u32];
        let target_count = preparation.target_count;
        if self.size != dimensions || self.targets.len() != target_count {
            if self.size != dimensions {
                // Drop ownership, never destroy textures referenced by queued work.
                self.targets.clear();
                self.size = dimensions;
            }
            self.targets.truncate(target_count);
            for _ in self.targets.len()..target_count {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("reusable scene filter target"),
                    size: wgpu::Extent3d {
                        width: dimensions[0],
                        height: dimensions[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("scene filter source"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.uniforms,
                                offset: 0,
                                size: NonZeroU64::new(64),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                self.targets.push(Target {
                    _texture: texture,
                    view,
                    bind_group,
                });
            }
        }
    }

    pub fn retire_targets(&mut self) {
        self.targets.clear();
        self.size = [0; 2];
        self.depth_targets = [None; MAX_DEPTH + 1];
        self.scratch_index = 0;
        self.snapshot_groups = 0;
        self.next_slot = 0;
    }

    pub fn view(&self, depth: usize, output: &wgpu::TextureView) -> wgpu::TextureView {
        match self.depth_targets[depth] {
            Some(index) => self.targets[index].view.clone(),
            None => {
                debug_assert_eq!(depth, 0);
                output.clone()
            }
        }
    }

    pub fn requires_snapshot(&self, marker: usize) -> bool {
        self.snapshot_groups & (1u64 << marker) != 0
    }

    fn params(&self, bounds: Bounds<ScaledPixels>, clip: Bounds<ScaledPixels>) -> Params {
        let rect =
            |b: Bounds<ScaledPixels>| [b.origin.x.0, b.origin.y.0, b.right().0, b.bottom().0];
        Params {
            viewport: [self.size[0] as f32, self.size[1] as f32],
            direction: [0.; 2],
            bounds: rect(bounds),
            clip: rect(clip),
            sigma: 0.,
            feather: 0.,
            mode: 0,
            pad: 0,
        }
    }

    fn full_params(&self) -> Params {
        Params {
            viewport: [self.size[0] as f32, self.size[1] as f32],
            direction: [0.; 2],
            bounds: [0.; 4],
            clip: [0., 0., self.size[0] as f32, self.size[1] as f32],
            sigma: 0.,
            feather: 0.,
            mode: 0,
            pad: 0,
        }
    }

    fn write_params(&mut self, queue: &wgpu::Queue, params: Params) -> Option<(u32, [u32; 4])> {
        // Snapshot groups replace a complete target: pixels outside their clip
        // must retain the parent, never the spare target's previous contents.
        let clip = if params.mode == 3 {
            self.full_params().clip
        } else {
            params.clip
        };
        let x = clip[0].floor().max(0.).min(params.viewport[0]) as u32;
        let y = clip[1].floor().max(0.).min(params.viewport[1]) as u32;
        let right = clip[2].ceil().max(0.).min(params.viewport[0]) as u32;
        let bottom = clip[3].ceil().max(0.).min(params.viewport[1]) as u32;
        if right <= x || bottom <= y {
            return None;
        }
        let offset = self.next_slot as u64 * self.stride;
        self.next_slot += 1;
        assert!((self.next_slot as u64) <= PARAM_SLOTS);
        queue.write_buffer(&self.uniforms, offset, bytemuck::bytes_of(&params));
        Some((offset as u32, [x, y, right - x, bottom - y]))
    }

    fn draw_into(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        source: usize,
        parent: usize,
        offset: u32,
        scissor: [u32; 4],
        composite: Composite,
    ) {
        pass.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]);
        let pipeline = match composite {
            Composite::Replace | Composite::Mix(1.0) => &self.replace,
            Composite::Over => &self.over,
            Composite::Mix(opacity) => {
                let opacity = f64::from(opacity);
                pass.set_blend_constant(wgpu::Color {
                    r: opacity,
                    g: opacity,
                    b: opacity,
                    a: opacity,
                });
                &self.mix
            }
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.targets[source].bind_group, &[offset]);
        // Reusing each target's bind group also avoids per-group bind groups.
        pass.set_bind_group(1, &self.targets[parent].bind_group, &[offset]);
        pass.draw(0..3, 0..1);
    }

    fn draw(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        source: usize,
        parent: usize,
        destination: &wgpu::TextureView,
        params: Params,
        composite: Composite,
    ) {
        let Some((offset, scissor)) = self.write_params(queue, params) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene filter/composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        self.draw_into(&mut pass, source, parent, offset, scissor, composite);
    }

    pub fn snapshot(&mut self, queue: &wgpu::Queue, depth: usize, pass: &mut wgpu::RenderPass<'_>) {
        let (offset, scissor) = self.write_params(queue, self.full_params()).unwrap();
        let source = self.depth_targets[depth - 1].expect("snapshot parent target");
        self.draw_into(pass, source, source, offset, scissor, Composite::Replace);
    }

    pub fn blur(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        depth: usize,
        bounds: Bounds<ScaledPixels>,
        clip: Bounds<ScaledPixels>,
        sigma: f32,
        opacity: f32,
    ) {
        // Groups containing a backdrop operation already hold the visible
        // parent snapshot. Blur it directly, without capturing ancestors again.
        let source = self.depth_targets[depth].expect("backdrop source target");
        let mut params = self.params(bounds, clip);
        params.sigma = sigma.clamp(0.01, 32.0);
        params.mode = 1;
        params.direction = [1., 0.];
        // Vertical pass needs horizontally blurred neighbors above/below panel.
        let halo = (params.sigma * 3.0).ceil() + 1.0;
        params.clip[1] = (params.clip[1] - halo).max(0.);
        params.clip[3] = (params.clip[3] + halo).min(params.viewport[1]);
        let scratch = self.targets[self.scratch_index].view.clone();
        self.draw(
            encoder,
            queue,
            source,
            source,
            &scratch,
            params,
            Composite::Replace,
        );
        params.clip = self.params(bounds, clip).clip;
        params.direction = [0., 1.];
        let destination = self.targets[source].view.clone();
        // Blend against the unchanged destination, including its alpha. This
        // avoids sampling the render attachment or allocating another target.
        self.draw(
            encoder,
            queue,
            self.scratch_index,
            self.scratch_index,
            &destination,
            params,
            Composite::Mix(opacity),
        );
    }

    pub fn composite(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        depth: usize,
        filter: SceneFilter,
        snapshot: bool,
        output: &wgpu::TextureView,
    ) {
        let SceneFilter::BeginEdgeFade {
            bounds,
            clip,
            width,
        } = filter
        else {
            unreachable!()
        };
        let mut params = self.params(bounds, clip);
        params.mode = 2;
        params.feather = width;
        let source = self.depth_targets[depth].expect("isolated group target");
        if snapshot {
            let parent = self.depth_targets[depth - 1].expect("snapshot parent target");
            // A blurred backdrop can lower alpha; source-over cannot express
            // that replacement. Interpolate against the unchanged parent once.
            params.mode = 3;
            let destination = self.targets[self.scratch_index].view.clone();
            self.draw(
                encoder,
                queue,
                source,
                parent,
                &destination,
                params,
                Composite::Replace,
            );
            std::mem::swap(
                self.depth_targets[depth - 1]
                    .as_mut()
                    .expect("snapshot parent target"),
                &mut self.scratch_index,
            );
        } else {
            let destination = self.view(depth - 1, output);
            self.draw(
                encoder,
                queue,
                source,
                source,
                &destination,
                params,
                Composite::Over,
            );
        }
    }

    pub fn present(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        destination: &wgpu::TextureView,
    ) {
        let Some(source) = self.depth_targets[0] else {
            return;
        };
        let params = self.full_params();
        self.draw(
            encoder,
            queue,
            source,
            source,
            destination,
            params,
            Composite::Replace,
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::super::WgpuHeadlessRenderer;
    use super::*;
    use gpui::{ContentMask, Hsla, PlatformHeadlessRenderer, Point, Quad};

    fn size(width: i32, height: i32) -> Size<DevicePixels> {
        Size {
            width: DevicePixels(width),
            height: DevicePixels(height),
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: Point {
                x: x.into(),
                y: y.into(),
            },
            size: Size {
                width: width.into(),
                height: height.into(),
            },
        }
    }

    fn quad(bounds: Bounds<ScaledPixels>, color: Hsla) -> Quad {
        Quad {
            bounds,
            content_mask: ContentMask { bounds },
            background: color.into(),
            ..Default::default()
        }
    }

    fn split_scene() -> Scene {
        let mut scene = Scene::default();
        scene.insert_primitive(quad(rect(0., 0., 32., 64.), gpui::red()));
        scene.insert_primitive(quad(rect(32., 0., 32., 64.), gpui::blue()));
        scene
    }

    #[test]
    fn backdrop_gaussian_matches_discrete_reference_at_edges_and_small_sigma() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let mut source = Scene::default();
        for y in 0..8 {
            for x in 0..8 {
                let mut color = gpui::rgb(if (x + y) % 2 == 0 { 0xe13a79 } else { 0x167dc4 });
                color.a = if x % 3 == 0 { 0.4 } else { 0.8 };
                source.insert_primitive(quad(
                    rect(x as f32 * 8., y as f32 * 8., 8., 8.),
                    color.into(),
                ));
            }
        }
        source.finish();
        let baseline = render_transparent(&mut renderer, &source)?;
        for sigma in [0.01_f32, 0.2, 0.6, 1., 4., 7.25, 32.] {
            let radius = (3. * sigma).ceil() as i32;
            let weights: Vec<f64> = (-radius..=radius)
                .map(|i| (-0.5 * (i as f64 / sigma as f64).powi(2)).exp())
                .collect();
            let total: f64 = weights.iter().sum();
            let mut reference = baseline.clone();
            for horizontal in [true, false] {
                let previous = reference.clone();
                for y in 0..64_i32 {
                    for x in 0..64_i32 {
                        for channel in 0..4 {
                            let value: f64 = (-radius..=radius)
                                .zip(&weights)
                                .map(|(i, weight)| {
                                    let sx = (x + if horizontal { i } else { 0 }).clamp(0, 63);
                                    let sy = (y + if horizontal { 0 } else { i }).clamp(0, 63);
                                    previous.get_pixel(sx as u32, sy as u32).0[channel] as f64
                                        * weight
                                })
                                .sum();
                            reference.get_pixel_mut(x as u32, y as u32).0[channel] =
                                (value / total).round() as u8;
                        }
                    }
                }
            }
            let mut scene = Scene::default();
            scene.replay(0..source.len(), &source);
            let bounds = rect(0., 0., 64., 64.);
            scene.insert_filter(SceneFilter::BackdropBlur {
                bounds,
                clip: bounds,
                sigma,
                opacity: 1.0,
            });
            scene.finish();
            let actual = render_transparent(&mut renderer, &scene)?;
            for (index, (actual, expected)) in
                actual.as_raw().iter().zip(reference.as_raw()).enumerate()
            {
                assert!(
                    actual.abs_diff(*expected) <= 2,
                    "sigma {sigma}, byte {index}: {actual} != {expected}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn backdrop_captures_painted_geometry_and_preserves_clip_and_foreground() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let mut scene = split_scene();
        let panel = rect(16., 8., 32., 48.);
        scene.insert_filter(SceneFilter::BackdropBlur {
            bounds: panel,
            clip: panel,
            sigma: 4.,
            opacity: 1.0,
        });
        scene.insert_primitive(quad(rect(31., 8., 2., 48.), gpui::rgb(0x00ff00).into()));
        scene.finish();
        let image = renderer.render_scene_to_image(&scene, size(64, 64))?;
        let blurred = image.get_pixel(29, 32).0;
        assert!(
            blurred[0] > 20 && blurred[2] > 20,
            "painted red/blue edge was not blurred: {blurred:?}"
        );
        assert_eq!(image.get_pixel(29, 2).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(32, 32).0, [0, 255, 0, 255]);

        let first = renderer
            .core
            .resources
            .scene_filters
            .as_ref()
            .unwrap()
            .targets[0]
            ._texture
            .clone();
        renderer.render_scene(&scene, size(64, 64))?;
        assert_eq!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets[0]
                ._texture,
            first
        );
        let mut plain = split_scene();
        plain.finish();
        renderer.render_scene(&plain, size(64, 64))?;
        assert!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn edge_fade_masks_the_overlapping_group_once_not_each_child() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let mut scene = Scene::default();
        scene.insert_primitive(quad(bounds, gpui::white()));
        // A surrounding GPUI batching layer must not move descendants across
        // the transparent-group boundaries.
        scene.push_layer(bounds);
        scene.insert_filter(SceneFilter::BeginEdgeFade {
            bounds,
            clip: rect(8., 0., 48., 64.),
            width: 16.,
        });
        scene.insert_primitive(quad(bounds, gpui::red()));
        scene.insert_primitive(quad(rect(16., 0., 32., 64.), gpui::blue()));
        scene.insert_filter(SceneFilter::EndEdgeFade);
        scene.pop_layer();
        scene.finish();
        let image = renderer.render_scene_to_image(&scene, size(64, 64))?;
        let overlap = image.get_pixel(32, 8).0;
        assert!(
            overlap[0].abs_diff(overlap[1]) <= 1,
            "red leaked through separately faded blue child: {overlap:?}"
        );
        assert!(overlap[0] > 30 && overlap[0] < 230 && overlap[2] == 255);
        assert_eq!(image.get_pixel(32, 32).0, [0, 0, 255, 255]);
        assert_eq!(image.get_pixel(4, 32).0, [255, 255, 255, 255]);
        assert_eq!(image.get_pixel(32, 0).0, image.get_pixel(32, 63).0);
        Ok(())
    }

    #[test]
    fn nested_backdrop_sees_root_scene_and_cached_replay_keeps_barriers() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let mut source = split_scene();
        let bounds = rect(0., 0., 64., 64.);
        source.insert_filter(SceneFilter::BeginEdgeFade {
            bounds,
            clip: bounds,
            width: 8.,
        });
        let panel = rect(16., 16., 32., 32.);
        source.insert_filter(SceneFilter::BackdropBlur {
            bounds: panel,
            clip: panel,
            sigma: 4.,
            opacity: 1.0,
        });
        source.insert_filter(SceneFilter::EndEdgeFade);
        source.finish();
        let mut replay = Scene::default();
        replay.replay(0..source.len(), &source);
        replay.finish();
        let image = renderer.render_scene_to_image(&replay, size(64, 64))?;
        let pixel = image.get_pixel(29, 32).0;
        assert!(
            pixel[0] > 20 && pixel[2] > 20,
            "nested blur sampled an empty group instead of root scene: {pixel:?}"
        );
        assert_eq!(image.get_pixel(29, 2).0, [255, 0, 0, 255]);
        Ok(())
    }

    fn render_transparent(
        renderer: &mut WgpuHeadlessRenderer,
        scene: &Scene,
    ) -> Result<image::RgbaImage> {
        let size = size(64, 64);
        renderer.ensure_render_target(size)?;
        let view = renderer.render_target.as_ref().unwrap().view.clone();
        renderer
            .core
            .render_frame(scene, &view, size, false, wgpu::Color::TRANSPARENT)?;
        renderer.read_image()
    }

    fn translucent_scene() -> Scene {
        let mut scene = Scene::default();
        let mut red = gpui::red();
        red.a = 0.5;
        scene.insert_primitive(quad(rect(0., 0., 32., 64.), red));
        scene
    }

    #[test]
    fn backdrop_opacity_interpolates_premultiplied_color_and_alpha_through_replay() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let panel = rect(16., 8., 32., 48.);
        for transparent in [false, true] {
            let mut source = if transparent {
                translucent_scene()
            } else {
                split_scene()
            };
            source.finish();
            let original = render_transparent(&mut renderer, &source)?;
            let mut full = Scene::default();
            full.replay(0..source.len(), &source);
            full.insert_filter(SceneFilter::BackdropBlur {
                bounds: panel,
                clip: panel,
                sigma: 4.,
                opacity: 1.,
            });
            full.finish();
            let blurred = render_transparent(&mut renderer, &full)?;

            for opacity in [0., 0.01, 0.25, 0.5, 1.] {
                for nested in [false, true] {
                    let mut scene = Scene::default();
                    scene.replay(0..source.len(), &source);
                    if nested {
                        for inset in [8., 12.] {
                            scene.insert_filter(SceneFilter::BeginEdgeFade {
                                bounds,
                                clip: rect(inset, 0., 64. - 2. * inset, 64.),
                                width: 16.,
                            });
                        }
                    }
                    scene.insert_filter(SceneFilter::BackdropBlur {
                        bounds: panel,
                        clip: panel,
                        sigma: 4.,
                        opacity,
                    });
                    if nested {
                        scene.insert_filter(SceneFilter::EndEdgeFade);
                        scene.insert_filter(SceneFilter::EndEdgeFade);
                    }
                    scene.finish();
                    let mut replay = Scene::default();
                    replay.replay(0..scene.len(), &scene);
                    replay.finish();
                    for scene in [&scene, &replay] {
                        let image = render_transparent(&mut renderer, scene)?;
                        for (index, (&actual, (&original, &blurred))) in image
                            .as_raw()
                            .iter()
                            .zip(original.as_raw().iter().zip(blurred.as_raw()))
                            .enumerate()
                        {
                            let opacity = if nested {
                                let y = (index / 4 / 64) as f32 + 0.5;
                                let t = (y.min(64. - y) / 16.).clamp(0., 1.);
                                let mask = t * t * (3. - 2. * t);
                                opacity * mask * mask
                            } else {
                                opacity
                            };
                            let expected = (f32::from(original) * (1. - opacity)
                                + f32::from(blurred) * opacity)
                                .round() as u8;
                            assert!(
                                actual.abs_diff(expected) <= 2,
                                "transparent={transparent}, opacity={opacity}, nested={nested}, byte {index}: {actual} != {expected}"
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn nested_backdrop_can_lower_alpha_without_repainting_the_translucent_parent() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let panel = rect(16., 8., 32., 48.);
        let blur = SceneFilter::BackdropBlur {
            bounds: panel,
            clip: panel,
            sigma: 4.,
            opacity: 1.0,
        };
        let outer = SceneFilter::BeginEdgeFade {
            bounds,
            clip: rect(8., 0., 48., 64.),
            width: 8.,
        };
        let inner = SceneFilter::BeginEdgeFade {
            bounds,
            clip: rect(12., 0., 40., 64.),
            width: 12.,
        };
        let mut direct = translucent_scene();
        direct.insert_filter(blur);
        direct.insert_filter(blur);
        direct.finish();
        let expected = render_transparent(&mut renderer, &direct)?;

        let mut nested = translucent_scene();
        nested.insert_filter(outer);
        nested.insert_filter(inner);
        nested.insert_filter(blur);
        nested.insert_filter(SceneFilter::EndEdgeFade);
        nested.insert_filter(SceneFilter::EndEdgeFade);
        // A sibling snapshot group must use the newly swapped parent target.
        nested.insert_filter(outer);
        nested.insert_filter(blur);
        nested.insert_filter(SceneFilter::EndEdgeFade);
        nested.finish();
        let mut replay = Scene::default();
        replay.replay(0..nested.len(), &nested);
        replay.finish();
        for scene in [&nested, &replay] {
            let image = render_transparent(&mut renderer, scene)?;
            let alpha = image.get_pixel(31, 32).0[3];
            assert!(
                alpha > 0 && alpha < 128,
                "blur did not lower alpha: {alpha}"
            );
            for x in [24, 29, 31, 32, 36, 39] {
                for (actual, expected) in image
                    .get_pixel(x, 32)
                    .0
                    .into_iter()
                    .zip(expected.get_pixel(x, 32).0)
                {
                    assert!(
                        actual.abs_diff(expected) <= 2,
                        "nested replacement differs at {x}"
                    );
                }
            }
            assert_eq!(image.get_pixel(8, 32).0, [128, 0, 0, 128]);
            assert_eq!(image.get_pixel(29, 2).0, [128, 0, 0, 128]);
            assert_eq!(image.get_pixel(56, 32).0, [0, 0, 0, 0]);
        }
        // With two active groups and blur, root + groups + one spare, not
        // another full-frame ancestor capture, are sufficient.
        assert_eq!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets
                .len(),
            4
        );

        let mut plain = translucent_scene();
        plain.finish();
        let image = render_transparent(&mut renderer, &plain)?;
        assert_eq!(image.get_pixel(31, 63).0, [128, 0, 0, 128]);
        assert_eq!(image.get_pixel(32, 63).0, [0, 0, 0, 0]);
        let filters = renderer.core.resources.scene_filters.as_ref().unwrap();
        assert!(filters.targets.is_empty());
        assert_eq!(filters.snapshot_groups, 0);
        Ok(())
    }

    #[test]
    fn fractional_clip_and_filter_removal_preserve_unaffected_pixels() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        assert!(renderer.core.resources.scene_filters.is_none());
        let mut plain = split_scene();
        plain.finish();
        let baseline = renderer.render_scene_to_image(&plain, size(64, 64))?;
        assert!(renderer.core.resources.scene_filters.is_none());
        let mut blurred = split_scene();
        let panel = rect(16.6, 8.6, 30.8, 46.8);
        blurred.insert_filter(SceneFilter::BackdropBlur {
            bounds: panel,
            clip: panel,
            sigma: 4.,
            opacity: 1.0,
        });
        blurred.finish();
        let image = renderer.render_scene_to_image(&blurred, size(64, 64))?;
        let pipeline = renderer
            .core
            .resources
            .scene_filters
            .as_ref()
            .unwrap()
            .replace
            .clone();
        for y in [8, 55, 63] {
            assert_eq!(image.get_pixel(29, y), baseline.get_pixel(29, y));
        }
        for y in [9, 54] {
            assert!(image.get_pixel(29, y).0[2] > 20);
        }
        let mut faded = split_scene();
        let bounds = rect(0., 0., 64., 64.);
        faded.insert_filter(SceneFilter::BeginEdgeFade {
            bounds,
            clip: bounds,
            width: 16.,
        });
        faded.insert_primitive(quad(bounds, gpui::white()));
        faded.insert_filter(SceneFilter::EndEdgeFade);
        faded.finish();
        renderer.render_scene(&faded, size(64, 64))?;
        let mut cached = Scene::default();
        cached.replay(0..blurred.len(), &blurred);
        cached.finish();
        assert_eq!(
            renderer.render_scene_to_image(&cached, size(64, 64))?,
            image,
            "cached effects changed sampled pixels"
        );
        cached.clear();
        cached.replay(0..plain.len(), &plain);
        cached.finish();
        let restored = renderer.render_scene_to_image(&cached, size(64, 64))?;
        assert_eq!(
            restored.as_raw(),
            baseline.as_raw(),
            "filter removal left stale pixels or a bottom stripe"
        );
        assert!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets
                .is_empty()
        );
        cached.clear();
        cached.replay(0..blurred.len(), &blurred);
        cached.finish();
        let replay = renderer.render_scene_to_image(&cached, size(64, 64))?;
        assert_eq!(replay, image, "filter reactivation changed sampled pixels");
        assert_eq!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .replace,
            pipeline,
            "reactivation rebuilt a reusable pipeline"
        );
        let mut invalid = Scene::default();
        invalid.insert_filter(SceneFilter::EndEdgeFade);
        invalid.finish();
        assert!(renderer.render_scene(&invalid, size(64, 64)).is_err());
        assert!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets
                .is_empty(),
            "preflight failure retained targets from the previous frame"
        );
        assert_eq!(
            renderer.render_scene_to_image(&blurred, size(64, 64))?,
            image,
            "rejected filter frame poisoned subsequent sampling"
        );
        let capacity = renderer.core.instance_data_capacity;
        let maximum = renderer.core.max_instance_data_size;
        renderer.core.instance_data_capacity = 16;
        renderer.core.max_instance_data_size = 16;
        let error = renderer.render_scene(&blurred, size(64, 64)).unwrap_err();
        assert!(
            format!("{error:#}").contains("instance"),
            "unexpected error: {error:#}"
        );
        assert!(
            renderer
                .core
                .resources
                .scene_filters
                .as_ref()
                .unwrap()
                .targets
                .is_empty(),
            "instance rejection retained newly prepared filter targets"
        );
        renderer.core.instance_data_capacity = capacity;
        renderer.core.max_instance_data_size = maximum;
        assert_eq!(
            renderer.render_scene_to_image(&blurred, size(64, 64))?,
            image,
            "instance rejection prevented filter target recovery"
        );
        Ok(())
    }

    #[test]
    fn fade_budget_counts_only_groups_after_backdrop_and_recovers_from_rejection() -> Result<()> {
        let bounds = rect(0., 0., 64., 64.);
        for srgb in [false, true] {
            for transparent in [false, true] {
                let mut renderer = if srgb {
                    WgpuHeadlessRenderer::new_for_msaa_comparison_srgb(transparent)?
                } else {
                    WgpuHeadlessRenderer::new_for_msaa_comparison(transparent)?
                };
                let mut backdrop = split_scene();
                backdrop.insert_filter(SceneFilter::BackdropBlur {
                    bounds,
                    clip: bounds,
                    sigma: 4.,
                    opacity: 1.,
                });
                backdrop.finish();
                renderer.render_scene_to_image(&backdrop, size(64, 64))?;
                let target_bytes =
                    64 * 64 * u64::from(renderer.core.target_format.block_copy_size(None).unwrap());
                for depth in [1, 2, 1] {
                    let mut plain = Scene::default();
                    let mut background = gpui::blue();
                    background.a = 0.35;
                    plain.insert_primitive(quad(bounds, background));
                    let mut source = Scene::default();
                    source.insert_primitive(quad(bounds, background));
                    for _ in 0..depth {
                        source.insert_filter(SceneFilter::BeginEdgeFade {
                            bounds,
                            clip: rect(5.25, 2.75, 51.5, 58.),
                            width: 16.,
                        });
                    }
                    let mut foreground = gpui::red();
                    foreground.a = 0.6;
                    source.insert_primitive(quad(bounds, foreground));
                    plain.insert_primitive(quad(bounds, foreground));
                    for _ in 0..depth {
                        source.insert_filter(SceneFilter::EndEdgeFade);
                    }
                    source.finish();
                    plain.finish();
                    let filtered = renderer.render_scene_to_image(&source, size(64, 64))?;
                    let unfiltered = renderer.render_scene_to_image(&plain, size(64, 64))?;
                    assert_ne!(filtered.get_pixel(32, 8), unfiltered.get_pixel(32, 8));
                    let required = target_bytes * depth;
                    for budget in [required, required - 1, required] {
                        source.scene_filter_limits.max_target_bytes = budget;
                        let actual = renderer.render_scene_to_image(&source, size(64, 64))?;
                        assert_eq!(
                            actual.as_raw(),
                            if budget == required {
                                filtered.as_raw()
                            } else {
                                unfiltered.as_raw()
                            },
                            "srgb={srgb}, transparent={transparent}, depth={depth}, budget={budget}"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn maximum_marker_count_has_distinct_aligned_uniform_slots() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let mut scene = Scene::default();
        let mut reference = Scene::default();
        for scene in [&mut scene, &mut reference] {
            scene.insert_primitive(quad(rect(0., 0., 4., 8.), gpui::red()));
            scene.insert_primitive(quad(rect(4., 0., 4., 8.), gpui::blue()));
        }
        let bounds = rect(0., 0., 8., 8.);
        for _ in 0..MAX_FILTERS - 1 {
            scene.insert_filter(SceneFilter::BackdropBlur {
                bounds,
                clip: bounds,
                sigma: 0.01,
                opacity: 1.0,
            });
        }
        let final_blur = SceneFilter::BackdropBlur {
            bounds,
            clip: rect(4., 0., 4., 8.),
            sigma: 4.,
            opacity: 1.0,
        };
        scene.insert_filter(final_blur);
        reference.insert_filter(final_blur);
        reference.finish();
        scene.finish();
        let actual = renderer.render_scene_to_image(&scene, size(8, 8))?;
        let expected = renderer.render_scene_to_image(&reference, size(8, 8))?;
        assert_eq!(
            actual.as_raw(),
            expected.as_raw(),
            "ordered filter uniforms overwrote one another"
        );
        Ok(())
    }

    #[test]
    fn filter_limits_preserve_content_and_recover() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let mut plain = split_scene();
        plain.finish();
        let expected = renderer.render_scene_to_image(&plain, size(64, 64))?;
        let mut valid = split_scene();
        valid.insert_filter(SceneFilter::BackdropBlur {
            bounds,
            clip: bounds,
            sigma: 4.,
            opacity: 1.0,
        });
        valid.finish();
        let filtered = renderer.render_scene_to_image(&valid, size(64, 64))?;
        assert_ne!(filtered.as_raw(), expected.as_raw());
        for (depth, blurs) in [(MAX_DEPTH + 1, 1), (1, MAX_FILTERS)] {
            let mut scene = Scene::default();
            for _ in 0..depth {
                scene.insert_filter(SceneFilter::BeginEdgeFade {
                    bounds,
                    clip: bounds,
                    width: 8.,
                });
            }
            scene.replay(0..plain.len(), &plain);
            for _ in 0..blurs {
                scene.insert_filter(SceneFilter::BackdropBlur {
                    bounds,
                    clip: bounds,
                    sigma: 4.,
                    opacity: 1.0,
                });
            }
            for _ in 0..depth {
                scene.insert_filter(SceneFilter::EndEdgeFade);
            }
            scene.finish();
            assert_eq!(
                renderer
                    .render_scene_to_image(&scene, size(64, 64))?
                    .as_raw(),
                expected.as_raw(),
                "limit fallback lost content at depth {depth} with {blurs} blurs"
            );
            assert!(
                renderer
                    .core
                    .resources
                    .scene_filters
                    .as_ref()
                    .unwrap()
                    .targets
                    .is_empty()
            );
            assert_eq!(
                renderer
                    .render_scene_to_image(&valid, size(64, 64))?
                    .as_raw(),
                filtered.as_raw(),
                "filter admission did not recover"
            );
        }
        Ok(())
    }

    #[test]
    fn window_filter_limits_survive_replay_and_recover_at_the_budget_boundary() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let mut plain = split_scene();
        plain.finish();
        let expected = renderer.render_scene_to_image(&plain, size(64, 64))?;
        let mut source = split_scene();
        source.insert_filter(SceneFilter::BackdropBlur {
            bounds,
            clip: bounds,
            sigma: 4.,
            opacity: 1.0,
        });
        source.finish();
        let filtered = renderer.render_scene_to_image(&source, size(64, 64))?;
        let required =
            64 * 64 * u64::from(renderer.core.target_format.block_copy_size(None).unwrap()) * 2;
        let mut replay = Scene::default();
        for budget in [required, required - 1, required] {
            replay.scene_filter_limits.max_target_bytes = budget;
            replay.clear();
            replay.replay(0..source.len(), &source);
            replay.finish();
            let actual = renderer.render_scene_to_image(&replay, size(64, 64))?;
            assert_eq!(
                actual.as_raw(),
                if budget == required {
                    filtered.as_raw()
                } else {
                    expected.as_raw()
                }
            );
        }
        source.clear();
        for _ in 0..2 {
            source.insert_filter(SceneFilter::BeginEdgeFade {
                bounds,
                clip: bounds,
                width: 16.,
            });
        }
        source.replay(0..plain.len(), &plain);
        for _ in 0..2 {
            source.insert_filter(SceneFilter::EndEdgeFade);
        }
        source.finish();
        let faded = renderer.render_scene_to_image(&source, size(64, 64))?;
        assert_ne!(faded.as_raw(), expected.as_raw());
        for depth in [1, 2, 1] {
            replay.scene_filter_limits.max_target_bytes = required * 2;
            replay.scene_filter_limits.max_depth = depth;
            replay.clear();
            replay.replay(0..source.len(), &source);
            replay.finish();
            let actual = renderer.render_scene_to_image(&replay, size(64, 64))?;
            assert_eq!(
                actual.as_raw(),
                if depth == 1 {
                    expected.as_raw()
                } else {
                    faded.as_raw()
                }
            );
        }
        Ok(())
    }

    #[test]
    fn water_and_filter_limits_degrade_independently() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let water_bounds = rect(0., 0., 16., 64.);
        let water = gpui::CgWaterDraw {
            bounds: water_bounds,
            clip: water_bounds,
            opacity: 1.,
            params: gpui::CgWaterParams {
                phase: 0.3,
                amplitude: 0.8,
                highlight: 0.8,
                light_mode: true,
                style: gpui::CgWaterStyle::Perspective,
            },
        };
        let mut filtered = split_scene();
        filtered.insert_filter(SceneFilter::BackdropBlur {
            bounds,
            clip: bounds,
            sigma: 4.,
            opacity: 1.0,
        });
        filtered.finish();
        let without_water = renderer.render_scene_to_image(&filtered, size(64, 64))?;
        let mut scene = Scene::default();
        scene.replay(0..filtered.len(), &filtered);
        scene.insert_cg_water(water);
        scene.finish();
        let with_water = renderer.render_scene_to_image(&scene, size(64, 64))?;
        assert_ne!(with_water.as_raw(), without_water.as_raw());
        for count in [
            super::super::cg_water::MAX_PANELS,
            super::super::cg_water::MAX_PANELS + 1,
            1,
        ] {
            scene.clear();
            scene.replay(0..filtered.len(), &filtered);
            for _ in 0..count {
                scene.insert_cg_water(water);
            }
            scene.finish();
            let actual = renderer.render_scene_to_image(&scene, size(64, 64))?;
            assert_eq!(
                actual.as_raw(),
                if count > super::super::cg_water::MAX_PANELS {
                    without_water.as_raw()
                } else {
                    with_water.as_raw()
                }
            );
        }
        let mut plain_water = split_scene();
        plain_water.insert_cg_water(water);
        plain_water.finish();
        let expected = renderer.render_scene_to_image(&plain_water, size(64, 64))?;
        scene.scene_filter_limits.max_target_bytes = 1;
        assert_eq!(
            renderer
                .render_scene_to_image(&scene, size(64, 64))?
                .as_raw(),
            expected.as_raw()
        );
        Ok(())
    }

    #[test]
    fn malformed_groups_still_fail_beyond_resource_limits() -> Result<()> {
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let bounds = rect(0., 0., 64., 64.);
        let mut unclosed = Scene::default();
        for _ in 0..=MAX_DEPTH {
            unclosed.insert_filter(SceneFilter::BeginEdgeFade {
                bounds,
                clip: bounds,
                width: 8.,
            });
        }
        unclosed.finish();
        assert!(renderer.render_scene(&unclosed, size(64, 64)).is_err());
        let mut unmatched = Scene::default();
        for _ in 0..=MAX_FILTERS {
            unmatched.insert_filter(SceneFilter::BackdropBlur {
                bounds,
                clip: bounds,
                sigma: 1.,
                opacity: 1.0,
            });
        }
        unmatched.insert_filter(SceneFilter::EndEdgeFade);
        unmatched.finish();
        assert!(renderer.render_scene(&unmatched, size(64, 64)).is_err());
        assert!(renderer.core.resources.scene_filters.is_none());
        Ok(())
    }
}
