//! wgpu renderer: instanced textured quads for tiles and players, plus an egui
//! overlay drawn into the same render pass.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::sprite::{self, SpriteDef};
use crate::block::{BlockDef, BlockRegistry, TILE_TEX, render_default};
use crate::protocol::BlockId;

/// A UV rectangle (texture-space min/max) addressing one image inside the atlas.
#[derive(Clone, Copy)]
pub struct UvRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl UvRect {
    const ZERO: UvRect = UvRect {
        min: [0.0, 0.0],
        max: [0.0, 0.0],
    };

    fn from_px(x: u32, w: u32, h: u32, tex_w: u32, tex_h: u32) -> UvRect {
        let (tw, th) = (tex_w as f32, tex_h as f32);
        UvRect {
            min: [x as f32 / tw, 0.0],
            max: [(x + w) as f32 / tw, h as f32 / th],
        }
    }
}

/// What an entry in the packed atlas represents.
enum AtlasKey {
    Block(BlockId),
    White,
    Sprite(&'static str, u32),
}

/// A texture atlas packing every block tile, every entity animation frame, and
/// a solid-white cell, side by side in one row. Each entry is top-aligned and
/// addressed by its own [`UvRect`], so entries of different sizes (16x16 tiles,
/// 16x32 players, 12x12 slimes) coexist in a single texture.
pub struct Atlas {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// UV rect per block id (invisible blocks get [`UvRect::ZERO`]).
    block_uv: Vec<UvRect>,
    /// Solid-white cell, for tinted flat quads.
    white_uv: UvRect,
    /// Per-frame UV rects, keyed by [`SpriteDef::name`].
    sprite_uv: HashMap<&'static str, Vec<UvRect>>,
}

impl Atlas {
    /// Build the atlas from block textures in `blocks_dir` and entity sprite
    /// sheets in `entities_dir`. Missing files are seeded from procedural
    /// defaults so there is always real, overwritable art on disk.
    pub fn build(reg: &BlockRegistry, blocks_dir: &Path, entities_dir: &Path) -> Atlas {
        let t = TILE_TEX;

        // Gather every image to pack: (what it is, w, h, rgba).
        let mut items: Vec<(AtlasKey, u32, u32, Vec<u8>)> = Vec::new();
        for def in reg.iter() {
            if def.visible {
                items.push((AtlasKey::Block(def.id), t, t, load_cell(def, blocks_dir)));
            }
        }
        items.push((AtlasKey::White, t, t, vec![255u8; (t * t * 4) as usize]));
        for def in sprite::all() {
            for (frame, pixels) in load_sheet(def, entities_dir).into_iter().enumerate() {
                items.push((
                    AtlasKey::Sprite(def.name, frame as u32),
                    def.frame_w,
                    def.frame_h,
                    pixels,
                ));
            }
        }

        let width: u32 = items.iter().map(|(_, w, _, _)| *w).sum::<u32>().max(1);
        let height: u32 = items.iter().map(|(_, _, h, _)| *h).max().unwrap_or(1);
        let mut pixels = vec![0u8; (width * height * 4) as usize];

        let mut block_uv = vec![UvRect::ZERO; reg.len()];
        let mut white_uv = UvRect::ZERO;
        let mut sprite_uv: HashMap<&'static str, Vec<UvRect>> = HashMap::new();

        let mut x_off = 0u32;
        for (key, w, h, buf) in &items {
            for y in 0..*h {
                for x in 0..*w {
                    let src = ((y * *w + x) * 4) as usize;
                    let dst = ((y * width + x_off + x) * 4) as usize;
                    pixels[dst..dst + 4].copy_from_slice(&buf[src..src + 4]);
                }
            }
            let rect = UvRect::from_px(x_off, *w, *h, width, height);
            match key {
                AtlasKey::Block(id) => block_uv[*id as usize] = rect,
                AtlasKey::White => white_uv = rect,
                AtlasKey::Sprite(name, frame) => {
                    let frames = sprite_uv.entry(name).or_default();
                    if frames.len() <= *frame as usize {
                        frames.resize(*frame as usize + 1, UvRect::ZERO);
                    }
                    frames[*frame as usize] = rect;
                }
            }
            x_off += *w;
        }

        Atlas {
            pixels,
            width,
            height,
            block_uv,
            white_uv,
            sprite_uv,
        }
    }

    /// UV rect for a block id.
    pub fn block(&self, id: BlockId) -> UvRect {
        self.block_uv
            .get(id as usize)
            .copied()
            .unwrap_or(self.white_uv)
    }

    /// UV rect for a sprite's animation frame (clamped/fallback-safe).
    pub fn sprite_frame(&self, name: &str, frame: u32) -> UvRect {
        self.sprite_uv
            .get(name)
            .and_then(|frames| frames.get(frame as usize))
            .copied()
            .unwrap_or(self.white_uv)
    }
}

/// Load one block's 16x16 RGBA cell (row-major, length `TILE_TEX*TILE_TEX*4`).
fn load_cell(def: &BlockDef, blocks_dir: &Path) -> Vec<u8> {
    let t = TILE_TEX;
    let path = blocks_dir.join(format!("{}.png", def.name));

    if !path.exists() {
        // Seed a starter file from the procedural default so the texture is a
        // real PNG on disk that can simply be overwritten.
        if let Some(default_tex) = def.default_tex {
            let buf = render_default(default_tex);
            match write_starter_png(&path, &buf, t, t) {
                Ok(()) => log::info!("wrote starter texture {}", path.display()),
                Err(e) => log::warn!("could not write starter texture {}: {e}", path.display()),
            }
            return buf;
        }
        log::warn!("missing texture {} (using placeholder)", path.display());
        return placeholder(t, t);
    }

    match image::open(&path) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let resized = if rgba.dimensions() == (t, t) {
                rgba
            } else {
                image::imageops::resize(&rgba, t, t, image::imageops::FilterType::Nearest)
            };
            resized.into_raw()
        }
        Err(e) => {
            log::warn!(
                "failed to load texture {}: {e} (using placeholder)",
                path.display()
            );
            placeholder(t, t)
        }
    }
}

/// Load an entity's animation frames, one PNG per frame from `<name>/<frame>.png`
/// (e.g. `player/0.png`, `player/1.png`). Missing frame files are seeded from the
/// procedural default and written to disk; broken files fall back to placeholders.
fn load_sheet(def: &SpriteDef, entities_dir: &Path) -> Vec<Vec<u8>> {
    let (fw, fh, n) = (def.frame_w, def.frame_h, def.frames);
    let dir = entities_dir.join(def.name);

    (0..n)
        .map(|frame| {
            let path = dir.join(format!("{frame}.png"));

            if !path.exists() {
                let pixels = render_frame(def, frame);
                match write_starter_png(&path, &pixels, fw, fh) {
                    Ok(()) => log::info!("wrote starter sprite frame {}", path.display()),
                    Err(e) => {
                        log::warn!(
                            "could not write starter sprite frame {}: {e}",
                            path.display()
                        )
                    }
                }
                return pixels;
            }

            match image::open(&path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let resized = if rgba.dimensions() == (fw, fh) {
                        rgba
                    } else {
                        image::imageops::resize(&rgba, fw, fh, image::imageops::FilterType::Nearest)
                    };
                    resized.into_raw()
                }
                Err(e) => {
                    log::warn!(
                        "failed to load sprite frame {}: {e} (using placeholder)",
                        path.display()
                    );
                    placeholder(fw, fh)
                }
            }
        })
        .collect()
}

/// Render a single frame of a sprite's procedural default into an RGBA buffer.
fn render_frame(def: &SpriteDef, frame: u32) -> Vec<u8> {
    let (fw, fh) = (def.frame_w, def.frame_h);
    let mut buf = vec![0u8; (fw * fh * 4) as usize];
    for y in 0..fh {
        for x in 0..fw {
            let dst = ((y * fw + x) * 4) as usize;
            buf[dst..dst + 4].copy_from_slice(&(def.default)(frame, x, y));
        }
    }
    buf
}

/// Write a `w`x`h` RGBA buffer to `path` as a PNG, creating parent dirs.
fn write_starter_png(path: &Path, rgba: &[u8], w: u32, h: u32) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let img: image::RgbaImage = image::ImageBuffer::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("buffer size mismatch"))?;
    img.save(path)?;
    Ok(())
}

/// Obvious magenta/black checker for missing or broken textures.
fn placeholder(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let magenta = ((x / 4) + (y / 4)) % 2 == 0;
            buf[idx..idx + 4].copy_from_slice(if magenta {
                &[255, 0, 255, 255]
            } else {
                &[0, 0, 0, 255]
            });
        }
    }
    buf
}

/// Camera uniform mirrored by the WGSL `Camera` struct (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    pub offset: [f32; 2],
    pub viewport: [f32; 2],
    pub zoom: f32,
    pub _pad0: f32,
    pub _pad1: [f32; 2],
}

impl CameraUniform {
    pub fn new(offset: [f32; 2], viewport: [f32; 2], zoom: f32) -> Self {
        CameraUniform {
            offset,
            viewport,
            zoom,
            _pad0: 0.0,
            _pad1: [0.0, 0.0],
        }
    }
}

/// One drawn quad (a tile or a player).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TileInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub color: [f32; 4],
}

const QUAD_VERTS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_packs_blocks_and_animated_sprites() {
        // Unique empty dirs so the builder writes starter art from defaults.
        let base = std::env::temp_dir().join(format!("sc-atlas-{}", std::process::id()));
        let blocks = base.join("blocks");
        let entities = base.join("entities");

        let reg = BlockRegistry::new();
        let atlas = Atlas::build(&reg, &blocks, &entities);

        // Starter sprite sheets were written to disk as real PNGs.
        assert!(entities.join("player.png").exists());
        assert!(entities.join("slime.png").exists());

        // Texture is tall enough for the tallest sprite (the 32px player).
        assert_eq!(atlas.height, sprite::PLAYER_SPRITE.frame_h);
        assert!(atlas.width > 0);

        // Animation frames are distinct regions, so the sprite actually animates.
        let f0 = atlas.sprite_frame("player", 0);
        let f1 = atlas.sprite_frame("player", 1);
        assert_ne!(f0.min[0], f1.min[0]);

        // A player frame spans the full height; a slime frame only part of it.
        let player = atlas.sprite_frame("player", 0);
        assert!((player.max[1] - 1.0).abs() < 1e-6);
        let slime = atlas.sprite_frame("slime", 0);
        assert!(slime.max[1] < 1.0);

        // Visible blocks resolve to a non-empty UV rect.
        let stone = atlas.block(crate::block::STONE);
        assert!(stone.max[0] > stone.min[0]);

        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Everything egui needs handed to the renderer for one frame.
pub struct EguiFrame {
    pub jobs: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    pub pixels_per_point: f32,
}

pub struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,

    pipeline: wgpu::RenderPipeline,
    quad_vbuf: wgpu::Buffer,
    quad_ibuf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_cap: usize,

    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    atlas_bg: wgpu::BindGroup,

    egui_renderer: Renderer,
}

impl Gfx {
    pub async fn new(window: Arc<Window>, atlas: &Atlas) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        // --- Camera uniform ---
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bg"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        // --- Atlas texture ---
        let tex_size = wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: tex_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width * 4),
                rows_per_image: Some(atlas.height),
            },
            tex_size,
        );
        let tex_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas-layout"),
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
            ],
        });
        let atlas_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bg"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // --- Pipeline ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tiles"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        let quad_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 2]>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        };
        let instance_attrs = wgpu::vertex_attr_array![
            1 => Float32x2, // pos
            2 => Float32x2, // size
            3 => Float32x2, // uv_min
            4 => Float32x2, // uv_max
            5 => Float32x4, // color
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TileInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &instance_attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tiles-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[quad_layout, instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let quad_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-verts"),
            contents: bytemuck::cast_slice(&QUAD_VERTS),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let quad_ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-indices"),
            contents: bytemuck::cast_slice(&QUAD_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        let instance_cap = 4096;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (instance_cap * std::mem::size_of::<TileInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let egui_renderer = Renderer::new(&device, format, RendererOptions::default());

        Ok(Gfx {
            surface,
            device,
            queue,
            config,
            size,
            pipeline,
            quad_vbuf,
            quad_ibuf,
            instance_buf,
            instance_cap,
            camera_buf,
            camera_bg,
            atlas_bg,
            egui_renderer,
        })
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render(
        &mut self,
        tiles: &[TileInstance],
        camera: CameraUniform,
        mut egui_frame: EguiFrame,
    ) {
        // Grow the instance buffer if needed.
        if tiles.len() > self.instance_cap {
            self.instance_cap = (tiles.len() * 2).next_power_of_two();
            self.instance_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (self.instance_cap * std::mem::size_of::<TileInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !tiles.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(tiles));
        }
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::cast_slice(&[camera]));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            // Timeout / Occluded / Validation: skip this frame.
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        // egui texture uploads.
        for (id, delta) in &egui_frame.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let screen = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: egui_frame.pixels_per_point,
        };
        let egui_cmds = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &egui_frame.jobs,
            &screen,
        );

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("main-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.45,
                                g: 0.62,
                                b: 0.86,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();

            if !tiles.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.camera_bg, &[]);
                pass.set_bind_group(1, &self.atlas_bg, &[]);
                pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
                pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                pass.set_index_buffer(self.quad_ibuf.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..QUAD_INDICES.len() as u32, 0, 0..tiles.len() as u32);
            }

            self.egui_renderer
                .render(&mut pass, &egui_frame.jobs, &screen);
        }

        self.queue.submit(
            egui_cmds
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        frame.present();

        for id in egui_frame.textures_delta.free.drain(..) {
            self.egui_renderer.free_texture(&id);
        }
    }
}
