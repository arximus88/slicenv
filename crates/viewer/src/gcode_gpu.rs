use crate::{FlatVertex, ToolpathGpuData, ToolpathInstance};

use gcode::Toolpath;
use gcode::{ColorScheme, ViewOptions, filament_colors, gradient};

/// GPU-дані траєкторії плюс відповідність інстансів і переміщень сегментам —
/// для повзунка рухів у шарі.
pub struct BuiltToolpath {
    pub gpu: ToolpathGpuData,
    /// Індекс сегмента для кожного інстансу екструзії (зростає).
    pub instance_segment: Vec<u32>,
    /// Індекс сегмента для кожної лінії переміщення (пари вершин).
    pub travel_segment: Vec<u32>,
    /// Межі градієнтної схеми, якщо вона градієнтна.
    pub range: Option<(f32, f32)>,
}

pub trait ToolpathGpuExt {
    fn to_gpu_data(&self, device: &wgpu::Device) -> ToolpathGpuData;
    fn build(&self, device: &wgpu::Device, options: &ViewOptions) -> BuiltToolpath;
    /// Буфери для режиму «Фото»: колір інстансу — слот філаменту й прапорці
    /// (`photo::bead`), усі ролі екструзії, шви як горбики.
    fn build_photo(&self, device: &wgpu::Device) -> BuiltToolpath;
}

impl ToolpathGpuExt for Toolpath {
    fn build_photo(&self, device: &wgpu::Device) -> BuiltToolpath {
        let p = crate::photo::bead::instances(self);
        let gpu = ToolpathGpuData::new(
            device,
            &p.instances,
            p.layer_ranges,
            Some(&p.travel_vertices),
            p.travel_layer_ranges,
            self.bounds(),
        );
        BuiltToolpath {
            gpu,
            instance_segment: p.instance_segment,
            travel_segment: p.travel_segment,
            range: None,
        }
    }

    /// Створює готові GPU буфери для відображення траєкторії у рендерері `viewer`.
    fn to_gpu_data(&self, device: &wgpu::Device) -> ToolpathGpuData {
        self.build(device, &ViewOptions::default()).gpu
    }

    /// Буфери з кольорами вибраної схеми; приховані типи ліній не потрапляють у буфер.
    fn build(&self, device: &wgpu::Device, options: &ViewOptions) -> BuiltToolpath {
        let times = self.layer_times();
        let range = self.value_range_with_times(options, &times);
        let filaments = filament_colors(self);
        let color = |seg: &gcode::Segment, time: f32| -> [f32; 4] {
            let rgb = match options.scheme {
                ColorScheme::LineType => {
                    let [r, g, b, _] = seg.role.color_srgb();
                    [r, g, b]
                }
                ColorScheme::Filament => filaments
                    .get(usize::from(seg.tool))
                    .copied()
                    .unwrap_or(gradient(0.5)),
                scheme => {
                    let (lo, hi) = range.unwrap_or((0.0, 1.0));
                    let t = if hi - lo > 1e-6 {
                        (scheme.value(seg, time) - lo) / (hi - lo)
                    } else {
                        0.5
                    };
                    gradient(t)
                }
            };
            [rgb[0], rgb[1], rgb[2], 1.0]
        };

        let mut instances = Vec::with_capacity(self.segments.len());
        let mut instance_segment = Vec::with_capacity(self.segments.len());
        let mut layer_ranges = Vec::with_capacity(self.layers.len());
        let mut travel_vertices = Vec::new();
        let mut travel_segment = Vec::new();
        let mut travel_layer_ranges = Vec::with_capacity(self.layers.len());

        let mut markers = self.markers.iter().peekable();
        for (layer, time) in self.layers.iter().zip(&times) {
            let start_inst = instances.len() as u32;
            let start_travel = travel_vertices.len() as u32;

            for (index, seg) in self.segments[layer.segment_range.clone()]
                .iter()
                .enumerate()
            {
                let index = (layer.segment_range.start + index) as u32;
                if seg.role == gcode::Role::Wipe {
                    // Витирання — тонка лінія з власним перемикачем, як у Bambu.
                    if options.is_visible(seg.role) {
                        let [r, g, b, _] = seg.role.color_srgb();
                        instances.push(ToolpathInstance::new(
                            seg.from,
                            seg.to,
                            [r, g, b, 1.0],
                            0.12,
                            0.12,
                        ));
                        instance_segment.push(index);
                    }
                } else if seg.is_extrusion {
                    if options.is_visible(seg.role) {
                        instances.push(
                            ToolpathInstance::new(
                                seg.from,
                                seg.to,
                                color(seg, *time),
                                seg.width,
                                seg.height,
                            )
                            .wall(matches!(
                                seg.role,
                                gcode::Role::Perimeter
                                    | gcode::Role::ExternalPerimeter
                                    | gcode::Role::OverhangPerimeter
                                    | gcode::Role::Skirt
                            )),
                        );
                        instance_segment.push(index);
                    }
                } else {
                    let col = seg.role.color_linear();
                    travel_vertices.push(FlatVertex {
                        pos: seg.from.to_array(),
                        color: col,
                    });
                    travel_vertices.push(FlatVertex {
                        pos: seg.to.to_array(),
                        color: col,
                    });
                    travel_segment.push(index);
                }
                // Шви й втягування — точки після свого сегмента (порядок інстансів не ламається).
                while let Some(m) = markers.next_if(|m| m.segment <= index) {
                    if options.is_visible(m.kind) {
                        let [r, g, b, _] = m.kind.color_srgb();
                        // Куля трохи вужча за валик, з центром на його осі: шейдер
                        // виносить її на радіус до камери, тож вона лягає поверх поверхні
                        // акуратною точкою, а не бородавкою.
                        let size = (m.width * 0.85).max(0.3);
                        let center = m.pos - glam::Vec3::Z * (m.height * 0.5);
                        instances.push(ToolpathInstance::dot(center, [r, g, b, 1.0], size));
                        instance_segment.push(index);
                    }
                }
            }

            layer_ranges.push(start_inst..instances.len() as u32);
            travel_layer_ranges.push(start_travel..travel_vertices.len() as u32);
        }

        ToolpathInstance::join(&mut instances);
        let gpu = ToolpathGpuData::new(
            device,
            &instances,
            layer_ranges,
            Some(&travel_vertices),
            travel_layer_ranges,
            self.bounds(),
        );
        BuiltToolpath {
            gpu,
            instance_segment,
            travel_segment,
            range,
        }
    }
}

impl BuiltToolpath {
    /// Видимі інстанси й вершини переміщень для шарів `layers` з обмеженням
    /// «до сегмента `until` (не включно)» — повзунок рухів у верхньому шарі.
    pub fn visible_ranges(
        &self,
        toolpath: &Toolpath,
        layers: std::ops::Range<usize>,
        until: Option<usize>,
    ) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
        let (a, b) = (
            layers.start.min(toolpath.layers.len()),
            layers.end.min(toolpath.layers.len()),
        );
        if a >= b {
            return (0..0, 0..0);
        }
        let first = toolpath.layers[a].segment_range.start as u32;
        let last = until.map_or(toolpath.layers[b - 1].segment_range.end, |u| {
            u.min(toolpath.layers[b - 1].segment_range.end)
        }) as u32;
        let inst = self.instance_segment.partition_point(|&s| s < first) as u32
            ..self.instance_segment.partition_point(|&s| s < last) as u32;
        let travel = self.travel_segment.partition_point(|&s| s < first) as u32 * 2
            ..self.travel_segment.partition_point(|&s| s < last) as u32 * 2;
        (inst, travel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seam_visibility_preserves_move_slider_ranges() {
        let Some(renderer) = crate::test_renderer("seam_visibility") else {
            return;
        };
        let t = gcode::parse("; CHANGE_LAYER\nG1 Z0.2\n; FEATURE: Outer wall\nG1 X10 E1\nG1 Y10 E2\nG1 X0 E3\nG1 Y0 E4\nG1 E3\nG1 X20\n").unwrap();
        let seam = t
            .markers
            .iter()
            .find(|m| m.kind == gcode::Role::Seam)
            .unwrap();
        let mut options = ViewOptions::default();
        let visible = t.build(renderer.device(), &options);
        options.hidden.push(gcode::Role::Seam);
        let hidden = t.build(renderer.device(), &options);
        assert_eq!(
            visible.instance_segment.len(),
            hidden.instance_segment.len() + 1
        );
        assert!(visible.instance_segment.windows(2).all(|s| s[0] <= s[1]));
        // До останнього руху петлі шов ще не видно; після нього — один додатковий інстанс.
        let before = Some(seam.segment as usize);
        let after = Some(seam.segment as usize + 1);
        assert_eq!(
            visible.visible_ranges(&t, 0..1, before),
            hidden.visible_ranges(&t, 0..1, before)
        );
        let (shown, _) = visible.visible_ranges(&t, 0..1, after);
        let (off, _) = hidden.visible_ranges(&t, 0..1, after);
        assert_eq!(shown.len(), off.len() + 1);
    }
}
