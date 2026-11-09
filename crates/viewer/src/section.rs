//! Geometry and a dedicated unclipped shader for the opt-in Prepare cut guide.
use glam::{Vec2, Vec3};

use super::{COLOR_FORMAT, DEPTH_FORMAT, FlatVertex, SAMPLES};

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32> }
@group(0) @binding(0) var<uniform> globals: Globals;
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}
@vertex
fn vs(@location(0) position: vec3<f32>, @location(1) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = globals.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}
@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> { return in.color; }
"#;

pub(super) fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("section_plane"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("section_plane"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<FlatVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(), // No face culling: visible from both sides.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: SAMPLES,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    })
}

pub(super) fn vertices(plane: [f32; 4], (lo, hi): (Vec3, Vec3)) -> Option<Vec<FlatVertex>> {
    if !plane.iter().all(|v| v.is_finite())
        || !lo.is_finite()
        || !hi.is_finite()
        || !lo.cmple(hi).all()
    {
        return None;
    }
    // Scale before normalizing so tiny/large but equivalent normals remain valid.
    let normal = Vec3::from_array([plane[0], plane[1], plane[2]]);
    let scale = normal.abs().max_element();
    if scale == 0.0 {
        return None;
    }
    let scaled = normal / scale;
    let length = scaled.length();
    let n = scaled / length;
    let d = (plane[3] / scale) / length;
    let center = lo * 0.5 + hi * 0.5;
    let origin = center - n * (n.dot(center) + d);
    let axis = if n.x.abs() <= n.y.abs() && n.x.abs() <= n.z.abs() {
        Vec3::X
    } else if n.y.abs() <= n.z.abs() {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let u = n.cross(axis).normalize();
    let v = n.cross(u);
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        ) - center;
        let p = Vec2::new(u.dot(corner), v.dot(corner));
        min = min.min(p);
        max = max.max(p);
    }
    let margin = ((hi - lo).length() * 0.04).max(2.0);
    min -= Vec2::splat(margin);
    max += Vec2::splat(margin);
    let border = ((max - min).min_element() * 0.008).max(0.15);
    let corners = |inset: f32| {
        let a = min + Vec2::splat(inset);
        let b = max - Vec2::splat(inset);
        [a, Vec2::new(b.x, a.y), b, Vec2::new(a.x, b.y)].map(|p| origin + u * p.x + v * p.y)
    };
    let outer = corners(0.0);
    let inner = corners(border);
    let mut out = Vec::with_capacity(30);
    let mut quad = |points: [Vec3; 4], color| {
        for i in [0, 1, 2, 0, 2, 3] {
            out.push(FlatVertex {
                pos: points[i].to_array(),
                color,
            });
        }
    };
    // Disjoint fill and border avoid drawing/blending the border twice.
    quad(inner, [0.025, 0.38, 0.80, 0.22]);
    for i in 0..4 {
        let j = (i + 1) % 4;
        quad(
            [outer[i], outer[j], inner[j], inner[i]],
            [0.04, 0.65, 1.0, 0.95],
        );
    }
    out.iter()
        .all(|v| Vec3::from(v.pos).is_finite())
        .then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_lie_on_axis_and_oblique_planes_and_cover_bounds() {
        let bounds = (Vec3::new(-10., 2., -7.), Vec3::new(30., 25., 18.));
        for normal in [Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(1., -2., 3.)] {
            let center = (bounds.0 + bounds.1) * 0.5;
            let d = -normal.dot(center);
            let plane = [normal.x, normal.y, normal.z, d];
            let mesh = vertices(plane, bounds).unwrap();
            assert_eq!(mesh.len(), 30);
            for vertex in &mesh {
                assert!((normal.dot(Vec3::from(vertex.pos)) + d).abs() < 0.0001);
            }
            // Recover the outer basis and check every projected box corner is inside
            // the rectangle, with a positive margin on both axes.
            let a = Vec3::from(mesh[6].pos);
            let b = Vec3::from(mesh[7].pos);
            let c = Vec3::from(mesh[13].pos);
            let u = (b - a).normalize();
            let v = (c - b).normalize();
            for i in 0..8 {
                let p = Vec3::new(
                    if i & 1 == 0 { bounds.0.x } else { bounds.1.x },
                    if i & 2 == 0 { bounds.0.y } else { bounds.1.y },
                    if i & 4 == 0 { bounds.0.z } else { bounds.1.z },
                ) - a;
                assert!(p.dot(u) > 0. && p.dot(u) < (b - a).length());
                assert!(p.dot(v) > 0. && p.dot(v) < (c - b).length());
            }
        }
    }

    #[test]
    fn normalization_degenerate_bounds_and_invalid_inputs() {
        let bounds = (Vec3::ZERO, Vec3::splat(10.));
        let baseline = vertices([0., 0., 1., -5.], bounds).unwrap();
        for scale in [1e-20, 3., 1e20] {
            let actual = vertices([0., 0., scale, -5. * scale], bounds).unwrap();
            for (a, b) in actual.iter().zip(&baseline) {
                assert!((Vec3::from(a.pos) - Vec3::from(b.pos)).length() < 0.0001);
            }
        }
        assert!(vertices([0., 0., 1., 0.], (Vec3::ZERO, Vec3::ZERO)).is_some());
        for plane in [[0.; 4], [f32::NAN, 0., 1., 0.], [0., 0., 1., f32::INFINITY]] {
            assert!(vertices(plane, bounds).is_none());
        }
        assert!(vertices([0., 0., 1., 0.], (bounds.1, bounds.0)).is_none());
        assert!(vertices([0., 0., 1., 0.], (Vec3::NAN, bounds.1)).is_none());
    }
}
