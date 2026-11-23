//! GPU counterpart of `RenderImage::blurred_frame` for atlas image tiles.
use bytemuck::{Pod, Zeroable};
use gpui::{AtlasTile, ImageBlurEdge};
use std::num::NonZeroU64;

const SHADER: &str = concat!(
    include_str!("image_preblur.wgsl"),
    include_str!("image_sampling.wgsl"),
);

/// Premultiplied horizontal result between the separable passes. Eight bits
/// would add a second quantization step that the CPU blur does not have.
const INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    origin: [i32; 2],
    size: [i32; 2],
    radius: f32,
    reflect: u32,
}

enum Passes {
    /// 9 horizontal and 9 vertical taps per texel, about 36 texel reads.
    Separable {
        horizontal: wgpu::RenderPipeline,
        vertical: wgpu::RenderPipeline,
    },
    /// The sprite shader's direct 9x9 quadrature, about 324 texel reads, for
    /// devices that cannot render to `INTERMEDIATE_FORMAT` (WebGL2 without
    /// `EXT_color_buffer_float`).
    Direct(wgpu::RenderPipeline),
}

pub(crate) struct ImagePreblur {
    layout: wgpu::BindGroupLayout,
    passes: Passes,
}

impl ImagePreblur {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, separable: bool) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image preblur layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Every pass uses `textureLoad`; this also admits
                        // `Rgba16Float` where it is not filterable.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<Params>() as u64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image preblur pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image preblur shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = |entry_point: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("image preblur pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_preblur"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
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
        let passes = if separable {
            Passes::Separable {
                horizontal: pipeline("fs_preblur_horizontal", INTERMEDIATE_FORMAT),
                vertical: pipeline("fs_preblur_vertical", format),
            }
        } else {
            Passes::Direct(pipeline("fs_preblur", format))
        };
        Self { layout, passes }
    }

    /// Blurs `source` into the equally sized `target` tile and submits the work, so
    /// any later submission samples the result. `target` may share a texture with,
    /// or be, `source`: every read finishes before the result is copied in.
    pub fn blur(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source_view: &wgpu::TextureView,
        target_texture: &wgpu::Texture,
        source: AtlasTile,
        target: AtlasTile,
        radius_texels: f32,
        edge: ImageBlurEdge,
    ) {
        debug_assert_eq!(source.bounds.size, target.bounds.size);
        let extent = wgpu::Extent3d {
            width: source.bounds.size.width.0 as u32,
            height: source.bounds.size.height.0 as u32,
            depth_or_array_layers: 1,
        };
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | usage,
                view_formats: &[],
            })
        };
        // A pass cannot sample its own attachment: render into a one-shot
        // texture, then copy it into the target tile.
        let scratch = texture(
            "image preblur scratch",
            target_texture.format(),
            wgpu::TextureUsages::COPY_SRC,
        );
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image preblur parameters"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &params,
            0,
            bytemuck::bytes_of(&Params {
                origin: [source.bounds.origin.x.0, source.bounds.origin.y.0],
                size: [source.bounds.size.width.0, source.bounds.size.height.0],
                radius: radius_texels,
                reflect: u32::from(edge == ImageBlurEdge::Reflect),
            }),
        );
        let bind_group = |input: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("image preblur bind group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: params.as_entire_binding(),
                    },
                ],
            })
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("image preblur"),
        });
        let scratch_view = scratch.create_view(&Default::default());
        match &self.passes {
            Passes::Separable {
                horizontal,
                vertical,
            } => {
                let intermediate = texture(
                    "image preblur intermediate",
                    INTERMEDIATE_FORMAT,
                    wgpu::TextureUsages::TEXTURE_BINDING,
                );
                let intermediate_view = intermediate.create_view(&Default::default());
                draw(
                    &mut encoder,
                    &intermediate_view,
                    horizontal,
                    &bind_group(source_view),
                );
                draw(
                    &mut encoder,
                    &scratch_view,
                    vertical,
                    &bind_group(&intermediate_view),
                );
            }
            Passes::Direct(pipeline) => {
                draw(
                    &mut encoder,
                    &scratch_view,
                    pipeline,
                    &bind_group(source_view),
                );
            }
        }
        encoder.copy_texture_to_texture(
            scratch.as_image_copy(),
            wgpu::TexelCopyTextureInfo {
                texture: target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: target.bounds.origin.x.0 as u32,
                    y: target.bounds.origin.y.0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            extent,
        );
        // Staged source uploads run first; the derived tile is complete before any
        // frame that can reference it is submitted.
        queue.submit([encoder.finish()]);
    }
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("image preblur pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}
