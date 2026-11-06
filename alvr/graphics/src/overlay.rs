// KiraVR: In-VR performance overlay. A text panel fixed to the head, drawn on
// top of the stream in each eye's render pass, so it shows in every game and
// in the SteamVR menu without an extra app on the PC.

use super::{GraphicsContext, SDR_FORMAT, StreamViewParams};
use alvr_common::{
    Pose,
    glam::{Mat4, UVec2, Vec3},
};
use glyph_brush_layout::{
    FontId, GlyphPositioner, HorizontalAlign, Layout, SectionGeometry, SectionText, VerticalAlign,
    ab_glyph::{Font, FontRef, ScaleFont},
};
use std::{
    cell::{Cell, RefCell},
    mem,
    rc::Rc,
};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, BlendState, ColorTargetState, ColorWrites,
    Extent3d, FragmentState, Origin3d, PipelineCompilationOptions, PipelineLayoutDescriptor,
    PrimitiveState, PrimitiveTopology, PushConstantRange, RenderPass, RenderPipeline,
    RenderPipelineDescriptor, SamplerBindingType, SamplerDescriptor, ShaderStages,
    TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect, TextureFormat,
    TextureSampleType, TextureViewDimension, VertexState, include_wgsl,
};

const TEXTURE_WIDTH: u32 = 640;
const TEXTURE_HEIGHT: u32 = 720;
const FONT_SIZE: f32 = 34.0;
const LINE_HEIGHT: f32 = 42.0;
const PADDING: f32 = 18.0;
const BACKGROUND: [u8; 4] = [10, 10, 14, 175];
const TRANSFORM_SIZE: u32 = mem::size_of::<Mat4>() as u32;
// WGSL layout of the shader's PushConstant: transform at 0, opacity at 64,
// tint (a vec4, aligned to 16) at 80, 96 bytes in all. The range must cover
// the whole struct or wgpu rejects the pipeline.
const OPACITY_OFFSET: usize = TRANSFORM_SIZE as usize;
const TINT_OFFSET: usize = 80;
const PUSH_CONSTANTS_SIZE: u32 = 96;
// Pointer beam thickness in meters.
const BEAM_WIDTH: f32 = 0.004;

fn push_constants(transform: Mat4, opacity: f32, tint: [f32; 3]) -> Vec<u8> {
    let mut bytes = vec![0u8; PUSH_CONSTANTS_SIZE as usize];
    for (i, v) in transform.to_cols_array().iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes[OPACITY_OFFSET..OPACITY_OFFSET + 4].copy_from_slice(&opacity.to_le_bytes());
    for (i, v) in tint.iter().chain(&[1.0]).enumerate() {
        let at = TINT_OFFSET + i * 4;
        bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

// A pointer ray from a controller, in the same space as the views.
#[derive(Clone, Copy, Debug)]
pub struct OverlayBeam {
    pub origin: Vec3,
    pub direction: Vec3,
    pub length: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

// Where the panel's text area is centered, set every frame.
#[derive(Clone, Copy, Debug)]
pub enum OverlayAnchor {
    // Fixed to the view, relative to the head (meters, -Z is forward).
    Head { pose: Pose },
    // Free in the stage space the views are in, like on a wrist. The pose's
    // -Z faces away from the readable side, as for a quad at the origin.
    World { pose: Pose },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayLine {
    pub text: String,
    pub color: [u8; 3],
}

impl OverlayLine {
    pub fn new(text: impl Into<String>, color: [u8; 3]) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
}

pub struct OverlayRenderer {
    context: Rc<GraphicsContext>,
    pipeline: RenderPipeline,
    bind_group: BindGroup,
    // A plain white texture, tinted per beam.
    beam_bind_group: BindGroup,
    beams: RefCell<Vec<OverlayBeam>>,
    texture: Texture,
    font: FontRef<'static>,
    visible: Cell<bool>,
    anchor: Cell<OverlayAnchor>,
    // Panel width in meters and opacity 0 to 1.
    width: Cell<f32>,
    opacity: Cell<f32>,
    // Part of the texture that holds the panel, from the top, 0 to 1.
    used_fraction: Cell<f32>,
}

impl OverlayRenderer {
    pub fn new(context: Rc<GraphicsContext>, target_format: TextureFormat) -> Self {
        let device = &context.device;

        let texture = super::create_texture(
            device,
            UVec2::new(TEXTURE_WIDTH, TEXTURE_HEIGHT),
            SDR_FORMAT,
        );

        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("overlay_bind_group_layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let make_bind_group = |texture: &Texture| {
            device.create_bind_group(&BindGroupDescriptor {
                label: Some("overlay_bind_group"),
                layout: &bind_group_layout,
                entries: &[
                    BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::TextureView(
                            &texture.create_view(&Default::default()),
                        ),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let bind_group = make_bind_group(&texture);

        let white = super::create_texture(device, UVec2::new(4, 4), SDR_FORMAT);
        context.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &[255u8; 4 * 4 * 4],
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * 4),
                rows_per_image: Some(4),
            },
            Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        let beam_bind_group = make_bind_group(&white);

        let shader = device.create_shader_module(include_wgsl!("../resources/overlay.wgsl"));
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("overlay_pipeline"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[PushConstantRange {
                    stages: ShaderStages::VERTEX_FRAGMENT,
                    range: 0..PUSH_CONSTANTS_SIZE,
                }],
            })),
            vertex: VertexState {
                module: &shader,
                entry_point: None,
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: None,
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: target_format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Self {
            context,
            pipeline,
            bind_group,
            beam_bind_group,
            beams: RefCell::new(vec![]),
            texture,
            font: FontRef::try_from_slice(include_bytes!("../resources/Ubuntu-Medium.ttf"))
                .unwrap(),
            visible: Cell::new(false),
            anchor: Cell::new(OverlayAnchor::Head {
                pose: Pose {
                    orientation: Default::default(),
                    position: Vec3::new(0.0, 0.0, -1.0),
                },
            }),
            width: Cell::new(0.3),
            opacity: Cell::new(1.0),
            used_fraction: Cell::new(0.0),
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
    }

    // Pointer rays to draw this frame, replacing the previous ones.
    pub fn set_beams(&self, beams: Vec<OverlayBeam>) {
        *self.beams.borrow_mut() = beams;
    }

    // Call every frame before rendering, a wrist anchor moves with the hand.
    pub fn set_anchor(&self, anchor: OverlayAnchor, width_meters: f32, opacity: f32) {
        self.anchor.set(anchor);
        self.width.set(width_meters.max(0.01));
        self.opacity.set(opacity.clamp(0.0, 1.0));
    }

    // Rasterizes the lines into the panel texture. Call it when the text
    // changes (a couple of times per second), not every frame.
    pub fn update(&self, lines: &[OverlayLine]) {
        let (buffer, used_height) = rasterize(&self.font, lines);
        self.used_fraction
            .set(used_height as f32 / TEXTURE_HEIGHT as f32);

        self.context.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &buffer,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TEXTURE_WIDTH * 4),
                rows_per_image: Some(TEXTURE_HEIGHT),
            },
            Extent3d {
                width: TEXTURE_WIDTH,
                height: TEXTURE_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
    }

    // Draws the panel into a pass that already holds the stream for this eye.
    pub(crate) fn draw(
        &self,
        pass: &mut RenderPass,
        views: &[StreamViewParams; 2],
        view_idx: usize,
    ) {
        let eye = views[view_idx].output_view_params;
        let eye_mat = Mat4::from_rotation_translation(eye.pose.orientation, eye.pose.position);
        let view_proj = super::projection_from_fov(eye.fov) * eye_mat.inverse();

        pass.set_pipeline(&self.pipeline);

        // Beams: Thin strips along the ray, turned to face this eye.
        pass.set_bind_group(0, &self.beam_bind_group, &[]);
        for beam in self.beams.borrow().iter() {
            let up = beam.direction.normalize_or_zero();
            let middle = beam.origin + up * (beam.length / 2.0);
            let to_eye = (eye.pose.position - middle).normalize_or_zero();
            let right = up.cross(to_eye).normalize_or_zero();
            if up == Vec3::ZERO || right == Vec3::ZERO || beam.length <= 0.0 {
                continue;
            }
            let facing = right.cross(up);
            let model = Mat4::from_cols(
                (right * BEAM_WIDTH).extend(0.0),
                (up * beam.length).extend(0.0),
                facing.extend(0.0),
                middle.extend(1.0),
            );
            let bytes = push_constants(view_proj * model, beam.opacity, beam.color);
            pass.set_push_constants(ShaderStages::VERTEX_FRAGMENT, 0, &bytes);
            pass.draw(0..4, 0..1);
        }

        if !self.visible.get() || self.used_fraction.get() <= 0.0 || self.opacity.get() <= 0.0 {
            return;
        }

        let anchor_mat = match self.anchor.get() {
            OverlayAnchor::Head { pose } => {
                // The head sits between the eyes, facing where they face.
                let left = views[0].output_view_params.pose;
                let right = views[1].output_view_params.pose;
                Mat4::from_rotation_translation(
                    left.orientation,
                    (left.position + right.position) / 2.0,
                ) * Mat4::from_rotation_translation(pose.orientation, pose.position)
            }
            OverlayAnchor::World { pose } => {
                Mat4::from_rotation_translation(pose.orientation, pose.position)
            }
        };

        let width = self.width.get();
        let panel_height = width * TEXTURE_HEIGHT as f32 / TEXTURE_WIDTH as f32;
        let used_height = panel_height * self.used_fraction.get();
        // The quad covers the whole texture but only the top part holds the
        // panel. Shift it so the panel is centered on the anchor.
        let model = anchor_mat
            * Mat4::from_translation(Vec3::new(0.0, (used_height - panel_height) / 2.0, 0.0))
            * Mat4::from_scale(Vec3::new(width, panel_height, 1.0));

        let bytes = push_constants(view_proj * model, self.opacity.get(), [1.0; 3]);

        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_push_constants(ShaderStages::VERTEX_FRAGMENT, 0, &bytes);
        pass.draw(0..4, 0..1);
    }
}

// Returns the RGBA texture and how many rows from the top the panel uses.
fn rasterize(font: &FontRef, lines: &[OverlayLine]) -> (Vec<u8>, u32) {
    let (width, height) = (TEXTURE_WIDTH as usize, TEXTURE_HEIGHT as usize);
    let mut buffer = vec![0u8; width * height * 4];
    if lines.is_empty() {
        return (buffer, 0);
    }

    let used_height =
        ((PADDING * 2.0 + LINE_HEIGHT * lines.len() as f32).ceil() as usize).min(height);
    for pixel in buffer[..used_height * width * 4].chunks_exact_mut(4) {
        pixel.copy_from_slice(&BACKGROUND);
    }

    let scaled = font.as_scaled(FONT_SIZE);
    for (index, line) in lines.iter().enumerate() {
        let top = PADDING + LINE_HEIGHT * index as f32;
        if top + LINE_HEIGHT > height as f32 {
            break;
        }
        let glyphs = Layout::default_single_line()
            .h_align(HorizontalAlign::Left)
            .v_align(VerticalAlign::Top)
            .calculate_glyphs(
                &[font],
                &SectionGeometry {
                    screen_position: (PADDING, top),
                    ..Default::default()
                },
                &[SectionText {
                    text: &line.text,
                    scale: FONT_SIZE.into(),
                    font_id: FontId(0),
                }],
            );

        for glyph in glyphs {
            let Some(outlined) = scaled.outline_glyph(glyph.glyph) else {
                continue;
            };
            let bounds = outlined.px_bounds();
            outlined.draw(|x, y, coverage| {
                let x = x as i32 + bounds.min.x as i32;
                let y = y as i32 + bounds.min.y as i32;
                if x < 0 || y < 0 || x >= width as i32 || y >= used_height as i32 {
                    return;
                }
                let at = (y as usize * width + x as usize) * 4;
                let pixel = &mut buffer[at..at + 4];
                // Blend the glyph over the panel background.
                for c in 0..3 {
                    pixel[c] = (pixel[c] as f32 * (1.0 - coverage)
                        + line.color[c] as f32 * coverage) as u8;
                }
                pixel[3] = pixel[3].max((coverage * 255.0) as u8);
            });
        }
    }

    (buffer, used_height as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_lines_onto_the_panel() {
        let font =
            FontRef::try_from_slice(include_bytes!("../resources/Ubuntu-Medium.ttf")).unwrap();
        let (buffer, used) = rasterize(
            &font,
            &[
                OverlayLine::new("72 fps", [80, 220, 120]),
                OverlayLine::new("Latency 42 ms", [255, 255, 255]),
            ],
        );
        assert_eq!(used, (PADDING * 2.0 + LINE_HEIGHT * 2.0).ceil() as u32);
        // Some text pixels were drawn in the line color, the rest is background.
        let green = buffer
            .chunks_exact(4)
            .filter(|p| p[1] > 150 && p[0] < 120)
            .count();
        assert!(green > 50, "{green}");
        // Below the panel stays transparent.
        let below = &buffer[(used as usize + 1) * TEXTURE_WIDTH as usize * 4..];
        assert!(below.iter().all(|b| *b == 0));
    }

    // Must match the WGSL PushConstant layout, or colors and opacity come out
    // wrong only on the headset.
    #[test]
    fn push_constant_layout_matches_the_shader() {
        let bytes = push_constants(Mat4::IDENTITY, 0.5, [0.25, 0.75, 1.0]);
        assert_eq!(bytes.len(), 96);
        let f = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(f(0), 1.0, "transform first column");
        assert_eq!(f(64), 0.5, "opacity");
        assert_eq!([f(80), f(84), f(88), f(92)], [0.25, 0.75, 1.0, 1.0], "tint");
    }

    #[test]
    fn empty_overlay_draws_nothing() {
        let font =
            FontRef::try_from_slice(include_bytes!("../resources/Ubuntu-Medium.ttf")).unwrap();
        let (buffer, used) = rasterize(&font, &[]);
        assert_eq!(used, 0);
        assert!(buffer.iter().all(|b| *b == 0));
    }
}
