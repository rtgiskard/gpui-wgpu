//! Fixed-cost procedural water: one six-vertex draw per panel and one bounded uniform upload.
use gpui::{DevicePixels, Scene, Size};
use std::num::NonZeroU64;

pub(super) const MAX_PANELS: usize = 256;
const SHADER: &str = include_str!("cg_water.wgsl");

pub(super) struct CgWaterRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    binding: wgpu::BindGroup,
    stride: u64,
}

impl CgWaterRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let stride =
            64u64.next_multiple_of(device.limits().min_uniform_buffer_offset_alignment as u64);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("CG water placement"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(64),
                },
                count: None,
            }],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded CG water placements"),
            size: stride * MAX_PANELS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("CG water"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: NonZeroU64::new(64),
                }),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("analytic CG water"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("CG water"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("CG water"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            buffer,
            binding,
            stride,
        }
    }

    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<()> {
        let draws = scene.cg_water_draws();
        let count = draws.len();
        anyhow::ensure!(
            count <= MAX_PANELS,
            "CG water exceeds {MAX_PANELS} panels per frame"
        );
        if count == 0 {
            return Ok(());
        }
        let bytes = (count as u64 - 1) * self.stride + 64;
        let mut upload = queue
            .write_buffer_with(&self.buffer, 0, NonZeroU64::new(bytes).unwrap())
            .ok_or_else(|| anyhow::anyhow!("failed to stage CG water placements"))?;
        for (index, draw) in draws.enumerate() {
            let p = draw.params.normalized();
            let data: [f32; 16] = [
                draw.bounds.origin.x.0,
                draw.bounds.origin.y.0,
                draw.bounds.size.width.0,
                draw.bounds.size.height.0,
                draw.clip.origin.x.0,
                draw.clip.origin.y.0,
                draw.clip.size.width.0,
                draw.clip.size.height.0,
                size.width.0 as f32,
                size.height.0 as f32,
                p.phase,
                p.amplitude,
                p.highlight,
                if p.light_mode { 1.0 } else { 0.0 },
                draw.opacity.clamp(0.0, 1.0),
                p.style as u8 as f32,
            ];
            let offset = index * self.stride as usize;
            upload
                .slice(offset..offset + 64)
                .copy_from_slice(bytemuck::bytes_of(&data));
            if index + 1 < count {
                // Initialize alignment gaps too; no stale staging bytes enter the GPU buffer.
                upload
                    .slice(offset + 64..offset + self.stride as usize)
                    .fill(0);
            }
        }
        Ok(())
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, index: usize) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.binding, &[(index as u64 * self.stride) as u32]);
        pass.draw(0..6, 0..1);
    }
}
