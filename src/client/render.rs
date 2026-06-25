//! wgpu renderer: instanced textured quads for tiles and players, plus an egui
//! overlay drawn into the same render pass.

use std::collections::HashMap;
use std::sync::Arc;

use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::sprite::{self, SpriteDef};
use crate::block::{BlockDef, BlockRegistry, TILE_TEX};
use crate::protocol::BlockId;

/// Maximum atlas row width before wrapping to a new shelf. Kept at the common
/// minimum `max_texture_dimension_2d` so the atlas uploads on older/low-end GPUs
/// (some Macs report a 2048-texel limit) as well as WebGL2 targets.
const MAX_ATLAS_WIDTH: u32 = 2048;

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

    fn from_px(x: u32, y: u32, w: u32, h: u32, tex_w: u32, tex_h: u32) -> UvRect {
        let (tw, th) = (tex_w as f32, tex_h as f32);
        UvRect {
            min: [x as f32 / tw, y as f32 / th],
            max: [(x + w) as f32 / tw, (y + h) as f32 / th],
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
/// a solid-white cell into shelves (rows). Entries are laid left to right and
/// wrap to a new shelf once a row would exceed [`MAX_ATLAS_WIDTH`], keeping both
/// dimensions within GPU texture-size limits. Each entry is addressed by its own
/// [`UvRect`], so entries of different sizes (16x16 tiles, 16x32 players, 12x12
/// slimes) coexist in a single texture.
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
    /// Build the atlas from the block tiles and entity sprite sheets embedded
    /// in the binary (see [`crate::assets`]). Any art not embedded falls back to
    /// an obvious placeholder so the game still runs.
    pub fn build(reg: &BlockRegistry) -> Atlas {
        let t = TILE_TEX;

        // Gather every image to pack: (what it is, w, h, rgba).
        let mut items: Vec<(AtlasKey, u32, u32, Vec<u8>)> = Vec::new();
        for def in reg.iter() {
            if def.visible {
                items.push((AtlasKey::Block(def.id), t, t, load_cell(def)));
            }
        }
        items.push((AtlasKey::White, t, t, vec![255u8; (t * t * 4) as usize]));
        for def in sprite::all() {
            for (frame, pixels) in load_sheet(def).into_iter().enumerate() {
                items.push((
                    AtlasKey::Sprite(def.name, frame as u32),
                    def.frame_w,
                    def.frame_h,
                    pixels,
                ));
            }
        }

        // Shelf-pack the items into rows so neither dimension blows past GPU
        // texture-size limits (a single 2607px-wide row trips the 2048 cap on
        // some hardware). First pass: assign each item an (x, y) origin, wrapping
        // to a new shelf whenever the current row would exceed MAX_ATLAS_WIDTH.
        let mut placements: Vec<(u32, u32)> = Vec::with_capacity(items.len());
        let (mut x_off, mut shelf_y, mut shelf_h) = (0u32, 0u32, 0u32);
        let (mut width, mut height) = (1u32, 1u32);
        for (_, w, h, _) in &items {
            if x_off > 0 && x_off + *w > MAX_ATLAS_WIDTH {
                shelf_y += shelf_h;
                x_off = 0;
                shelf_h = 0;
            }
            placements.push((x_off, shelf_y));
            x_off += *w;
            shelf_h = shelf_h.max(*h);
            width = width.max(x_off);
            height = height.max(shelf_y + shelf_h);
        }

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut block_uv = vec![UvRect::ZERO; reg.len()];
        let mut white_uv = UvRect::ZERO;
        let mut sprite_uv: HashMap<&'static str, Vec<UvRect>> = HashMap::new();

        for ((key, w, h, buf), (px, py)) in items.iter().zip(&placements) {
            for y in 0..*h {
                for x in 0..*w {
                    let src = ((y * *w + x) * 4) as usize;
                    let dst = (((py + y) * width + px + x) * 4) as usize;
                    pixels[dst..dst + 4].copy_from_slice(&buf[src..src + 4]);
                }
            }
            let rect = UvRect::from_px(*px, *py, *w, *h, width, height);
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

    /// UV rect of the solid-white cell, for tinted flat quads (e.g. the
    /// block-breaking overlay or entity health bars).
    pub fn white(&self) -> UvRect {
        self.white_uv
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

/// Decode an embedded PNG into a `w`x`h` RGBA buffer (row-major), nearest-resizing
/// if the source dimensions differ. Broken bytes fall back to a placeholder.
fn decode_png(label: &str, bytes: &[u8], w: u32, h: u32) -> Vec<u8> {
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let resized = if rgba.dimensions() == (w, h) {
                rgba
            } else {
                image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Nearest)
            };
            resized.into_raw()
        }
        Err(e) => {
            log::warn!("failed to decode embedded texture {label}: {e} (using placeholder)");
            placeholder(w, h)
        }
    }
}

/// Load one block's 16x16 RGBA cell (row-major, length `TILE_TEX*TILE_TEX*4`)
/// from the embedded block textures.
fn load_cell(def: &BlockDef) -> Vec<u8> {
    let t = TILE_TEX;
    match crate::assets::block_png(def.name) {
        Some(bytes) => decode_png(def.name, bytes, t, t),
        None => {
            log::warn!(
                "no embedded texture for block {} (using placeholder)",
                def.name
            );
            placeholder(t, t)
        }
    }
}

/// Load an entity's animation frames from the embedded sprite sheets, one PNG
/// per frame (e.g. `player/0.png`, `player/1.png`).
fn load_sheet(def: &SpriteDef) -> Vec<Vec<u8>> {
    let (fw, fh, n) = (def.frame_w, def.frame_h, def.frames);

    (0..n)
        .map(|frame| match crate::assets::sprite_png(def.name, frame) {
            Some(bytes) => decode_png(&format!("{}/{frame}", def.name), bytes, fw, fh),
            None => {
                log::warn!(
                    "no embedded sprite frame {}/{frame} (using placeholder)",
                    def.name
                );
                placeholder(fw, fh)
            }
        })
        .collect()
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
        let reg = BlockRegistry::new();
        let atlas = Atlas::build(&reg);

        // Shelves keep both dimensions within GPU texture-size limits.
        assert!(atlas.width > 0 && atlas.width <= MAX_ATLAS_WIDTH);
        assert!(atlas.height >= sprite::KNIGHT_HORSE_SPRITE.frame_h);

        // Animation frames are distinct regions, so the sprite actually animates.
        let f0 = atlas.sprite_frame("player", 0);
        let f1 = atlas.sprite_frame("player", 1);
        assert!(f0.min != f1.min);

        // Each sprite's UV rect spans exactly its frame size in texels.
        let px_h = |r: UvRect| ((r.max[1] - r.min[1]) * atlas.height as f32).round() as u32;
        assert_eq!(
            px_h(atlas.sprite_frame("knight/horse", 0)),
            sprite::KNIGHT_HORSE_SPRITE.frame_h
        );
        let slime = atlas.sprite_frame("slime", 0);
        assert!(px_h(slime) < sprite::KNIGHT_HORSE_SPRITE.frame_h);

        // Visible blocks resolve to a non-empty UV rect.
        let stone = atlas.block(crate::block::STONE);
        assert!(stone.max[0] > stone.min[0]);
    }
}

/// Everything egui needs handed to the renderer for one frame.
pub struct EguiFrame {
    pub jobs: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    pub pixels_per_point: f32,
}

/// A captured frame's pixels: row-major RGBA8, `width`x`height`. Produced by
/// [`Gfx::render`] when a screenshot is requested.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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

    /// Render one frame. When `capture` is set, the world is also rendered into
    /// an offscreen target (sky + tiles, but no egui HUD) and read back, so the
    /// returned [`CapturedFrame`] is a clean screenshot of the scene only.
    pub fn render(
        &mut self,
        tiles: &[TileInstance],
        camera: CameraUniform,
        sky: [f32; 4],
        mut egui_frame: EguiFrame,
        capture: bool,
    ) -> Option<CapturedFrame> {
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

        // Read back the world (no HUD) for a screenshot before drawing the
        // visible frame. Done up front so it succeeds even if the surface frame
        // below is skipped (e.g. an outdated swapchain).
        let captured = if capture {
            self.capture_scene(tiles, sky)
        } else {
            None
        };

        // Apply egui texture uploads BEFORE acquiring the surface texture.
        // egui emits each delta exactly once; if we drop this frame after
        // get_current_texture() fails (common on macOS during initial swapchain
        // setup), the delta is gone forever — leaving the font atlas Managed(0)
        // permanently absent and spamming "Missing texture: Managed(0)".
        for (id, delta) in &egui_frame.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let freed = std::mem::take(&mut egui_frame.textures_delta.free);
        let free_textures = |r: &mut Renderer| {
            for id in &freed {
                r.free_texture(id);
            }
        };

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                free_textures(&mut self.egui_renderer);
                return captured;
            }
            // Timeout / Occluded / Validation: skip this frame.
            _ => {
                free_textures(&mut self.egui_renderer);
                return captured;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

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
                                r: sky[0] as f64,
                                g: sky[1] as f64,
                                b: sky[2] as f64,
                                a: sky[3] as f64,
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

        free_textures(&mut self.egui_renderer);
        captured
    }

    /// Render the world (sky + tiles, no HUD) into an offscreen texture at the
    /// current surface size and read it back as RGBA8. Returns `None` if the
    /// surface is zero-sized or the GPU read-back fails.
    ///
    /// The camera and instance buffers are assumed already populated for this
    /// frame by [`Gfx::render`]; the queued writes are ordered before this
    /// pass, so the offscreen image matches what the player sees.
    fn capture_scene(&self, tiles: &[TileInstance], sky: [f32; 4]) -> Option<CapturedFrame> {
        let (width, height) = (self.config.width, self.config.height);
        if width == 0 || height == 0 {
            return None;
        }
        const BPP: u32 = 4;

        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Buffer rows must be a multiple of COPY_BYTES_PER_ROW_ALIGNMENT, so the
        // staging buffer is padded and rows are un-padded after read-back.
        let unpadded = width * BPP;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("screenshot-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screenshot-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: sky[0] as f64,
                            g: sky[1] as f64,
                            b: sky[2] as f64,
                            a: sky[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !tiles.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.camera_bg, &[]);
                pass.set_bind_group(1, &self.atlas_bg, &[]);
                pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
                pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                pass.set_index_buffer(self.quad_ibuf.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..QUAD_INDICES.len() as u32, 0, 0..tiles.len() as u32);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        // Map the staging buffer and block until the GPU is done.
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        if self
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
            return None;
        }
        if !matches!(rx.recv(), Ok(Ok(()))) {
            return None;
        }

        // Un-pad rows and, if the surface stores BGRA, swizzle to RGBA.
        let data = slice.get_mapped_range();
        let row_bytes = (width * BPP) as usize;
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut rgba = vec![0u8; row_bytes * height as usize];
        for y in 0..height as usize {
            let src = &data[y * padded as usize..y * padded as usize + row_bytes];
            let dst = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            if bgra {
                for px in 0..width as usize {
                    let i = px * 4;
                    dst[i] = src[i + 2];
                    dst[i + 1] = src[i + 1];
                    dst[i + 2] = src[i];
                    dst[i + 3] = src[i + 3];
                }
            } else {
                dst.copy_from_slice(src);
            }
        }
        drop(data);
        buffer.unmap();

        Some(CapturedFrame {
            width,
            height,
            rgba,
        })
    }
}
