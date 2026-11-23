//! The wgpu renderer: dots in, finished paper out.
//!
//! One entry point does the work, `Renderer::render_into`: it renders a pixel rectangle of the *displayed* sheet at a given
//! pixels-per-inch. The on-screen view calls it for the visible rectangle (at the zoom level), the exporters call it for
//! bands / tiles of the page at the export dpi. Everything is defined in inches, so a tile is identical to the same
//! rectangle cut out of a full-page render.
use super::dots::{GpuDot, SheetDots};
use crate::emu::settings::RenderSettings;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const COV_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const INK_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const OUT_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub const SHADER_SRC: &str = include_str!("render.wgsl");

/// Uniforms (matches `Params` in render.wgsl: fourteen vec4).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub reg: [f32; 4],
    pub out_reg: [f32; 4],
    pub pg: [f32; 4],
    pub pgpx: [f32; 4],
    pub kern: [f32; 4],
    pub inkp: [f32; 4],
    pub paperp: [f32; 4],
    pub flags: [u32; 4],
    pub seeds_a: [u32; 4],
    pub seeds_b: [u32; 4],
    pub tone0: [f32; 4],
    pub tone1: [f32; 4],
    pub tone2: [f32; 4],
    pub tone3: [f32; 4],
}

/// A rectangle of the displayed sheet, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// A sheet uploaded to the GPU.
pub struct SheetBuf {
    pub sheet: SheetDots,
    buf: wgpu::Buffer,
}

struct Scratch {
    key: (u32, u32, u32),
    cov: wgpu::TextureView,
    ink: wgpu::TextureView,
    blur_h: wgpu::TextureView,
    blur_v: wgpu::TextureView,
}

pub struct Renderer {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    bgl_params: wgpu::BindGroupLayout,
    bgl_cov: wgpu::BindGroupLayout,
    bgl_src: wgpu::BindGroupLayout,
    bgl_final: wgpu::BindGroupLayout,
    pl_cov: wgpu::RenderPipeline,
    pl_ink: wgpu::RenderPipeline,
    pl_blur_h: wgpu::RenderPipeline,
    pl_blur_v: wgpu::RenderPipeline,
    pl_final: wgpu::RenderPipeline,
    scratch: Option<Scratch>,
    /// Largest output tile per axis (tests lower it to exercise the tiling).
    pub tile_limit: u32,
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Seconds-free displayed <-> upright coordinate maps (identical to the WGSL ones).
pub fn disp_to_up(q: i32, wu: f32, hu: f32, d: (f32, f32)) -> (f32, f32) {
    match q {
        1 => (d.1, hu - d.0),
        2 => (wu - d.0, hu - d.1),
        3 => (wu - d.1, d.0),
        _ => d,
    }
}

/// Size in pixels of the sheet as displayed (after the view rotation) at `ppi`.
pub fn displayed_size(sheet: &SheetDots, rotation: i32, ppi: f32) -> (u32, u32) {
    let wu = (sheet.paper_w_in * ppi as f64).round().max(1.0) as u32;
    let hu = (sheet.paper_h_in * ppi as f64).round().max(1.0) as u32;
    if (rotation / 90).rem_euclid(2) == 1 {
        (hu, wu)
    } else {
        (wu, hu)
    }
}

pub fn supersample(ppi: f32) -> u32 {
    if ppi <= 200.0 {
        2
    } else {
        1
    }
}

/// Pixels of margin a region needs around it so the blurs and dot kernels are complete inside it.
pub fn margin_px(sheet: &SheetDots, s: &RenderSettings, ppi: f32) -> u32 {
    let ss = supersample(ppi) as f64;
    let r = (sheet.dot_diam_in * s.dot_size * ppi as f64 * ss / 2.0).max(0.6);
    let half = (r * 1.9).ceil() + 1.0;
    let half_out = (half / ss).ceil();
    let blur = !s.clean && s.impact > 0.0;
    let sigma = (0.7f64)
        .max(ppi as f64 / 150.0)
        .max(if blur { 1.2 } else { 0.0 })
        .max(if blur { ppi as f64 / 60.0 } else { 0.0 });
    (half_out + (4.0 * sigma).ceil() + 4.0) as u32
}

impl Renderer {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("retroimpact.wgsl"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(SHADER_SRC)),
        });
        let bgl_params = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mk_bgl = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        let bgl_cov = mk_bgl("cov", &[tex_entry(0)]);
        let bgl_src = mk_bgl("src", &[tex_entry(1)]);
        let bgl_final = mk_bgl("final", &[tex_entry(2), tex_entry(3)]);

        let layout = |label: &str, bgls: &[&wgpu::BindGroupLayout]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: bgls,
                push_constant_ranges: &[],
            })
        };
        let l_cov = layout("cov", &[&bgl_params]);
        let l_ink = layout("ink", &[&bgl_params, &bgl_cov]);
        let l_src = layout("blur", &[&bgl_params, &bgl_src]);
        let l_fin = layout("final", &[&bgl_params, &bgl_final]);

        const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Uint32];
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let pl_cov = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("coverage"),
            layout: Some(&l_cov),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_cov"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuDot>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRS,
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_cov"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COV_FMT,
                    blend: Some(wgpu::BlendState { color: add, alpha: add }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let full = |label: &str, l: &wgpu::PipelineLayout, fs: &str, fmt: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(l),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: fmt, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let pl_ink = full("ink", &l_ink, "fs_ink", INK_FMT);
        let pl_blur_h = full("blur_h", &l_src, "fs_blur_h", INK_FMT);
        let pl_blur_v = full("blur_v", &l_src, "fs_blur_v", INK_FMT);
        let pl_final = full("final", &l_fin, "fs_final", OUT_FMT);
        Renderer { device, queue, bgl_params, bgl_cov, bgl_src, bgl_final, pl_cov, pl_ink, pl_blur_h, pl_blur_v, pl_final, scratch: None, tile_limit: 4096 }
    }

    /// A renderer with its own device (tests, command line, export without a window). None if there is no adapter.
    pub fn headless() -> Option<Renderer> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("retroimpact headless"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some(Renderer::new(Arc::new(device), Arc::new(queue)))
    }

    pub fn max_dim(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// Largest output tile (per axis) that fits the texture limit at this resolution.
    pub fn max_tile(&self, sheet: &SheetDots, s: &RenderSettings, ppi: f32) -> u32 {
        let ss = supersample(ppi);
        let m = margin_px(sheet, s, ppi);
        (self.max_dim() / ss).saturating_sub(2 * m).clamp(64, self.tile_limit.max(64))
    }

    pub fn upload(&self, sheet: SheetDots) -> SheetBuf {
        let bytes: &[u8] = if sheet.dots.is_empty() { &[0u8; 16] } else { bytemuck::cast_slice(&sheet.dots) };
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dots"),
            contents: bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });
        SheetBuf { sheet, buf }
    }

    pub fn out_texture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("page"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUT_FMT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn work_texture(&self, label: &str, w: u32, h: u32, fmt: wgpu::TextureFormat) -> wgpu::TextureView {
        self.device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn make_params(&self, sheet: &SheetDots, s: &RenderSettings, ppi: f32, region: Region, ext: Region, ss: u32) -> Params {
        let q = ((s.rotation / 90).rem_euclid(4)) as f32;
        let wu = (sheet.paper_w_in * ppi as f64).round().max(1.0) as f32;
        let hu = (sheet.paper_h_in * ppi as f64).round().max(1.0) as f32;
        let r_cov = (sheet.dot_diam_in * s.dot_size * ppi as f64 * ss as f64 / 2.0) as f32;
        let rmax = r_cov.max(0.6);
        let (softness, rim, shape) = if s.clean {
            (0.0, 0.0, 0.0)
        } else {
            let shape = match s.pin_shape.as_str() {
                "oval" => 1.0,
                "square" => 2.0,
                "ring" => 3.0,
                _ => 0.0,
            };
            (
                (0.55 - 0.25 * s.saturation / 1.5) as f32,
                (0.10 + 0.25 * (1.0 - s.saturation).max(0.0)) as f32,
                shape,
            )
        };
        let blur = !s.clean && s.impact > 0.0;
        let sigma1 = (0.7f64).max(ppi as f64 / 150.0) as f32;
        let sigma2 = (1.2f64).max(ppi as f64 / 60.0) as f32;
        let sd = (s.seed as u32).wrapping_add((sheet.page_index as u32).wrapping_mul(31));
        let tone = |t: (u8, u8, u8)| [t.0 as f32 / 255.0, t.1 as f32 / 255.0, t.2 as f32 / 255.0, 1.0];
        Params {
            reg: [ext.x as f32, ext.y as f32, ext.w as f32, ext.h as f32],
            out_reg: [region.x as f32, region.y as f32, region.w as f32, region.h as f32],
            pg: [sheet.paper_w_in as f32, sheet.paper_h_in as f32, ppi, ss as f32],
            pgpx: [wu, hu, q, if sheet.multi_ink { 1.0 } else { 0.0 }],
            kern: [r_cov, softness * rmax, rim, shape],
            inkp: [
                (0.9 + 3.6 * s.saturation) as f32,
                s.ribbon_wear as f32,
                if blur { s.impact as f32 } else { 0.0 },
                sigma1,
            ],
            paperp: [s.paper_texture as f32, if blur { sigma2 } else { sigma1 }, (0.006 * s.misalign) as f32, if s.clean { 1.0 } else { 0.0 }],
            flags: [sheet.paper_kind, sheet.tractor as u32, s.show_holes as u32, (s.pin_shape == "rough") as u32],
            seeds_a: [sd.wrapping_add(11), sd.wrapping_add(12), sd.wrapping_add(13), 0],
            seeds_b: [(s.seed as u32).wrapping_add(21), (s.seed as u32).wrapping_add(22), (s.seed as u32).wrapping_add(23), 0],
            tone0: tone(s.ribbon_rgb()),
            tone1: tone(s.ink_rgb(1)),
            tone2: tone(s.ink_rgb(2)),
            tone3: tone(s.ink_rgb(4)),
        }
    }

    /// Render `region` of the displayed sheet into `out` (which must be at least as large as the region).
    /// Returns the region actually rendered (clipped to the sheet).
    pub fn render_into(&mut self, sb: &SheetBuf, s: &RenderSettings, ppi: f32, region: Region, out: &wgpu::Texture) -> Region {
        let sheet = &sb.sheet;
        let (wd, hd) = displayed_size(sheet, s.rotation, ppi);
        let rx = region.x.clamp(0, wd as i32 - 1);
        let ry = region.y.clamp(0, hd as i32 - 1);
        let rw = region.w.min(wd - rx as u32).max(1);
        let rh = region.h.min(hd - ry as u32).max(1);
        let region = Region { x: rx, y: ry, w: rw, h: rh };

        let ss = supersample(ppi);
        let m = margin_px(sheet, s, ppi) as i32;
        let ex0 = (rx - m).max(0);
        let ey0 = (ry - m).max(0);
        let ex1 = (rx + rw as i32 + m).min(wd as i32);
        let ey1 = (ry + rh as i32 + m).min(hd as i32);
        let ext = Region { x: ex0, y: ey0, w: (ex1 - ex0) as u32, h: (ey1 - ey0) as u32 };
        let params = self.make_params(sheet, s, ppi, region, ext, ss);

        // which dots can touch the extended region (in upright inches)
        let q = (s.rotation / 90).rem_euclid(4);
        let (wu, hu) = (params.pgpx[0], params.pgpx[1]);
        let corners = [
            (ext.x as f32, ext.y as f32),
            ((ext.x + ext.w as i32) as f32, ext.y as f32),
            (ext.x as f32, (ext.y + ext.h as i32) as f32),
            ((ext.x + ext.w as i32) as f32, (ext.y + ext.h as i32) as f32),
        ];
        let (mut ymin, mut ymax) = (f32::MAX, f32::MIN);
        for c in corners {
            let u = disp_to_up(q, wu, hu, c);
            ymin = ymin.min(u.1);
            ymax = ymax.max(u.1);
        }
        let r_cov = params.kern[0].max(0.6);
        let pad = ((r_cov * 1.9).ceil() + 3.0) / (ppi * ss as f32);
        let (lo, hi) = sheet.y_range(ymin / ppi - pad, ymax / ppi + pad);

        // scratch textures
        let key = (ext.w, ext.h, ss);
        if self.scratch.as_ref().map(|s| s.key) != Some(key) {
            self.scratch = Some(Scratch {
                key,
                cov: self.work_texture("coverage", ext.w * ss, ext.h * ss, COV_FMT),
                ink: self.work_texture("ink", ext.w, ext.h, INK_FMT),
                blur_h: self.work_texture("blur_h", ext.w, ext.h, INK_FMT),
                blur_v: self.work_texture("blur_v", ext.w, ext.h, INK_FMT),
            });
        }
        let sc = self.scratch.as_ref().unwrap();

        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg_params = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("params"),
            layout: &self.bgl_params,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }],
        });
        let tex_bg = |layout: &wgpu::BindGroupLayout, entries: &[(u32, &wgpu::TextureView)]| {
            let e: Vec<wgpu::BindGroupEntry> =
                entries.iter().map(|(b, v)| wgpu::BindGroupEntry { binding: *b, resource: wgpu::BindingResource::TextureView(v) }).collect();
            self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &e })
        };
        let bg_cov = tex_bg(&self.bgl_cov, &[(0, &sc.cov)]);
        let bg_blur_h_src = tex_bg(&self.bgl_src, &[(1, &sc.ink)]);
        let bg_blur_v_src = tex_bg(&self.bgl_src, &[(1, &sc.blur_h)]);
        let blur = !s.clean && s.impact > 0.0;
        let bg_final = if blur {
            tex_bg(&self.bgl_final, &[(2, &sc.ink), (3, &sc.blur_v)])
        } else {
            tex_bg(&self.bgl_final, &[(2, &sc.ink), (3, &sc.ink)])
        };
        let out_view = out.create_view(&wgpu::TextureViewDescriptor::default());

        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("retroimpact") });
        {
            // 1. coverage
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("coverage"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &sc.cov,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if hi > lo {
                p.set_pipeline(&self.pl_cov);
                p.set_bind_group(0, &bg_params, &[]);
                p.set_vertex_buffer(0, sb.buf.slice(..));
                p.draw(0..6, lo as u32..hi as u32);
            }
        }
        let mut full = |enc: &mut wgpu::CommandEncoder, label: &str, target: &wgpu::TextureView, pl: &wgpu::RenderPipeline, bg1: &wgpu::BindGroup, scissor: (u32, u32)| {
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            p.set_pipeline(pl);
            p.set_bind_group(0, &bg_params, &[]);
            p.set_bind_group(1, bg1, &[]);
            p.set_scissor_rect(0, 0, scissor.0, scissor.1);
            p.draw(0..3, 0..1);
        };
        // 2. ink transfer
        full(&mut enc, "ink", &sc.ink, &self.pl_ink, &bg_cov, (ext.w, ext.h));
        // 3. blurs for the impact shading
        if blur {
            full(&mut enc, "blur_h", &sc.blur_h, &self.pl_blur_h, &bg_blur_h_src, (ext.w, ext.h));
            full(&mut enc, "blur_v", &sc.blur_v, &self.pl_blur_v, &bg_blur_v_src, (ext.w, ext.h));
        }
        // 4. paper + ink + shading
        full(&mut enc, "final", &out_view, &self.pl_final, &bg_final, (rw, rh));
        self.queue.submit(Some(enc.finish()));
        region
    }

    /// Copy the top-left `w` x `h` pixels of an Rgba8 texture back to the CPU.
    pub fn read_rgba(&self, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
        let bpr = (w * 4 + 255) / 256 * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::Wait);
        rx.recv().expect("readback channel").expect("map readback buffer");
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            let a = row * bpr as usize;
            out.extend_from_slice(&data[a..a + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        out
    }

    /// Render one region and return it as RGBA bytes (w, h, pixels).
    pub fn render_rgba(&mut self, sb: &SheetBuf, s: &RenderSettings, ppi: f32, region: Region) -> (u32, u32, Vec<u8>) {
        let tex = self.out_texture(region.w, region.h);
        let r = self.render_into(sb, s, ppi, region, &tex);
        let px = self.read_rgba(&tex, r.w, r.h);
        (r.w, r.h, px)
    }

    /// Render the whole sheet in horizontal bands of `band_h` rows (tiled across if it is wider than the GPU allows) and hand
    /// every band to `f(y, rows, rgba)`; memory stays bounded however large the page is.
    pub fn render_bands<F: FnMut(u32, u32, &[u8])>(&mut self, sb: &SheetBuf, s: &RenderSettings, ppi: f32, band_h: u32, mut f: F) {
        let (w, h) = displayed_size(&sb.sheet, s.rotation, ppi);
        let tile = self.max_tile(&sb.sheet, s, ppi);
        let band_h = band_h.clamp(1, tile);
        let tex = self.out_texture(tile.min(w), band_h);
        let mut y = 0u32;
        while y < h {
            let rows = band_h.min(h - y);
            let mut band = vec![0u8; (w * rows * 4) as usize];
            let mut x = 0u32;
            while x < w {
                let cols = tile.min(w - x);
                let r = self.render_into(sb, s, ppi, Region { x: x as i32, y: y as i32, w: cols, h: rows }, &tex);
                let px = self.read_rgba(&tex, r.w, r.h);
                for row in 0..r.h as usize {
                    let dst = (row * w as usize + x as usize) * 4;
                    let src = row * r.w as usize * 4;
                    band[dst..dst + r.w as usize * 4].copy_from_slice(&px[src..src + r.w as usize * 4]);
                }
                x += cols;
            }
            f(y, rows, &band);
            y += rows;
        }
    }
}

