//! wgpu surface + a variable-size framebuffer blit for the N64 VI output.
//!
//! Each frame the frontend uploads the produced RGBA8 framebuffer (active
//! sub-rectangle of up to 640x480) into a wgpu texture and a fullscreen-triangle
//! pass samples it with nearest filtering and an aspect-correct letterbox. The
//! egui shell is composited on top via `egui-wgpu`.
//!
//! N64 VI resolution is variable (320x240 and 640x480 are the common modes), so
//! the texture is sized for [`crate::FB_MAX_W`] x [`crate::FB_MAX_H`] and the
//! blit's UV transform crops to the active `w`/`h` reported by the frame.

use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::{FB_MAX_H, FB_MAX_W};

/// Errors during graphics init.
#[derive(Debug, thiserror::Error)]
pub enum GfxError {
    /// Failed to create a wgpu surface for the window.
    #[error("create surface: {0}")]
    Surface(String),
    /// No compatible wgpu adapter (no usable GPU).
    #[error("no compatible wgpu adapter")]
    NoAdapter,
    /// Failed to acquire a wgpu device.
    #[error("request device: {0}")]
    Device(String),
}

/// Outcome of acquiring a swapchain frame.
#[derive(Debug, thiserror::Error)]
pub enum PresentError {
    /// Surface lost/outdated; the caller should reconfigure (resize) and retry.
    #[error("surface lost/outdated; reconfiguring")]
    Reconfigure,
    /// A transient acquisition status; the frame is skipped.
    #[error("surface unavailable: {0}")]
    Other(&'static str),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    // rect.xy = the image's fraction of the surface (<=1); rect.zw = pixel offset.
    rect: [f32; 4],
    // crop.xy = active framebuffer fraction of the max texture (U,V scale).
    crop: [f32; 4],
}

const SHADER_SRC: &str = r"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Uniforms {
    rect: vec4<f32>,
    crop: vec4<f32>,
};

@group(0) @binding(0) var fb_tex: texture_2d<f32>;
@group(0) @binding(1) var fb_smp: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 3.0,  1.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>( 0.0,  2.0),
        vec2<f32>( 0.0,  0.0),
        vec2<f32>( 2.0,  0.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(pos[vid], 0.0, 1.0);
    // Letterbox in UV space: map the surface fraction (rect) back into [0,1].
    let centered = (uv[vid] - vec2<f32>(u.rect.z, u.rect.w)) / vec2<f32>(u.rect.x, u.rect.y);
    out.uv = centered;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Bars outside the image fraction are black.
    if (in.uv.x < 0.0 || in.uv.x > 1.0 || in.uv.y < 0.0 || in.uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // Crop the active framebuffer sub-rectangle out of the max-size texture.
    let sample_uv = in.uv * vec2<f32>(u.crop.x, u.crop.y);
    return textureSample(fb_tex, fb_smp, sample_uv);
}
";

/// The wgpu device + surface + the framebuffer-blit pipeline.
pub struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    uniform_buf: wgpu::Buffer,
    /// The egui-wgpu renderer the shell composites through.
    pub egui_renderer: egui_wgpu::Renderer,
}

impl Gfx {
    /// Initialize wgpu for `window`.
    ///
    /// # Errors
    /// Surface / adapter / device acquisition failures map to [`GfxError`].
    pub fn new(window: Arc<Window>) -> Result<Self, GfxError> {
        pollster::block_on(Self::new_async(window))
    }

    // One straight-line wgpu init (instance -> surface -> adapter -> device ->
    // texture -> sampler -> pipeline -> bind group); the descriptor boilerplate
    // is inherently long and reads more clearly as a single setup unit.
    #[allow(clippy::too_many_lines)]
    async fn new_async(window: Arc<Window>) -> Result<Self, GfxError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|e| GfxError::Surface(e.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| GfxError::NoAdapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rustyn64-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .map_err(|e| GfxError::Device(e.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustyn64-fb"),
            size: wgpu::Extent3d {
                width: FB_MAX_W,
                height: FB_MAX_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rustyn64-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustyn64-uniforms"),
            contents: bytemuck::bytes_of(&Uniforms {
                rect: [1.0, 1.0, 0.0, 0.0],
                crop: [1.0, 1.0, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rustyn64-blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustyn64-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rustyn64-bind"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buf.as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustyn64-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustyn64-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let egui_renderer =
            egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            texture,
            uniform_buf,
            egui_renderer,
        })
    }

    /// Borrow the wgpu device (egui-wgpu paint needs it).
    #[must_use]
    pub const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Borrow the wgpu queue.
    #[must_use]
    pub const fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The surface format (egui screen descriptor needs it).
    #[must_use]
    pub const fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Reconfigure the swapchain on a window resize.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Upload the active framebuffer sub-rectangle to the GPU texture.
    pub fn upload_framebuffer(&self, rgba: &[u8], w: u32, h: u32) {
        let w = w.min(FB_MAX_W);
        let h = h.min(FB_MAX_H);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Recompute the letterbox + crop uniforms for an active `fb_w`x`fb_h` frame
    /// presented 4:3 into the current surface.
    fn update_uniforms(&self, fb_w: u32, fb_h: u32) {
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        // N64 displays at 4:3.
        let target_aspect = 4.0 / 3.0;
        let surf_aspect = surf_w / surf_h;
        let (sx, sy) = if surf_aspect > target_aspect {
            (target_aspect / surf_aspect, 1.0)
        } else {
            (1.0, surf_aspect / target_aspect)
        };
        let ox = (1.0 - sx) * 0.5;
        let oy = (1.0 - sy) * 0.5;
        let crop_u = fb_w as f32 / FB_MAX_W as f32;
        let crop_v = fb_h as f32 / FB_MAX_H as f32;
        let u = Uniforms {
            rect: [sx, sy, ox, oy],
            crop: [crop_u, crop_v, 0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
    }

    /// Acquire a swapchain frame, blit the framebuffer, paint egui, and present.
    ///
    /// `egui_prims` + `egui_textures` are the tessellated output of the egui pass
    /// run by the shell (which must already have happened — egui state is touched
    /// outside the emu lock).
    ///
    /// # Errors
    /// [`PresentError::Reconfigure`] when the surface is lost/outdated (the
    /// caller resizes and retries); [`PresentError::Other`] for a skipped frame.
    pub fn render(
        &mut self,
        fb_w: u32,
        fb_h: u32,
        egui_prims: &[egui::ClippedPrimitive],
        egui_textures: &egui::TexturesDelta,
        screen: &egui_wgpu::ScreenDescriptor,
    ) -> Result<(), PresentError> {
        self.update_uniforms(fb_w, fb_h);

        // wgpu 29 returns the `CurrentSurfaceTexture` enum (not a `Result`): use
        // the texture on `Success`/`Suboptimal`, reconfigure on `Lost`/`Outdated`,
        // skip otherwise.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                return Err(PresentError::Reconfigure);
            }
            wgpu::CurrentSurfaceTexture::Timeout => return Err(PresentError::Other("timeout")),
            _ => return Err(PresentError::Other("surface unavailable")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustyn64-encoder"),
            });

        for (id, delta) in &egui_textures.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            egui_prims,
            screen,
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustyn64-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);

            // egui composited on top (egui-wgpu 0.34 wants a 'static-lifetime pass).
            let mut pass = pass.forget_lifetime();
            self.egui_renderer.render(&mut pass, egui_prims, screen);
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        for id in &egui_textures.free {
            self.egui_renderer.free_texture(id);
        }
        Ok(())
    }
}

impl std::fmt::Debug for Gfx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gfx")
            .field("surface_size", &(self.config.width, self.config.height))
            .field("format", &self.config.format)
            .finish_non_exhaustive()
    }
}
