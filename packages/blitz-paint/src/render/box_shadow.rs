use super::ElementCx;
use crate::color::{Color, ToColorColor as _};
use crate::kurbo_css::CssBox;
use anyrender::PaintScene;
use blitz_dom::CopyStyleGetters;
use kurbo::{Affine, BezPath, Insets, PathEl, Point, Rect, Vec2};
use peniko::{Compose, Fill, Mix};

impl ElementCx<'_, '_> {
    pub(super) fn draw_outset_box_shadow(&self, scene: &mut impl PaintScene) {
        let box_shadow = &self.style.get_effects().box_shadow.0;
        let has_outset_shadow = box_shadow.iter().any(|s| !s.inset);
        if !has_outset_shadow {
            return;
        }

        let current_color = self.style.clone_color();
        let opacity = self.style.get_effects().opacity;
        let bg_color = self
            .style
            .get_background()
            .background_color
            .resolve_to_absolute(&current_color)
            .as_srgb_color();
        let bg_is_opaque = bg_color.components[3] >= 1.0;
        let needs_clip = opacity < 1.0 || !bg_is_opaque;

        let max_shadow_rect = box_shadow.iter().fold(Rect::ZERO, |prev, shadow| {
            let x = shadow.base.horizontal.px() as f64 * self.scale;
            let y = shadow.base.vertical.px() as f64 * self.scale;
            let blur = shadow.base.blur.px() as f64 * self.scale;
            let spread = shadow.spread.px() as f64 * self.scale;
            let offset = spread + blur * 2.5;

            let rect = self.frame.border_box.inflate(offset, offset) + Vec2::new(x, y);

            prev.union(rect)
        });

        self.context.layer_manager.maybe_with_layer(
            scene,
            needs_clip,
            Mix::Normal.into(),
            1.0,
            self.transform,
            &self.frame.shadow_clip(max_shadow_rect),
            None,
            None,
            |scene| {
                for shadow in box_shadow.iter().filter(|s| !s.inset).rev() {
                    let shadow_color = shadow
                        .base
                        .color
                        .resolve_to_absolute(&current_color)
                        .as_srgb_color();

                    let alpha = shadow_color.components[3];
                    if alpha != 0.0 {
                        let transform = self.transform.then_translate(Vec2 {
                            x: shadow.base.horizontal.px() as f64 * self.scale,
                            y: shadow.base.vertical.px() as f64 * self.scale,
                        });

                        let spread = shadow.spread.px() as f64 * self.scale;
                        let blur = shadow.base.blur.px() as f64;

                        // Without blur or spread the shadow is exactly the element's shape,
                        // so fill the border box path directly (preserving individual radii)
                        if blur == 0.0 && spread == 0.0 {
                            scene.fill(
                                Fill::NonZero,
                                transform,
                                shadow_color,
                                None,
                                &self.frame.border_box_path(),
                            );
                            continue;
                        }

                        // TODO draw shadows with matching individual radii instead of averaging
                        let radius = self.frame.border_radii.average();
                        let rect = self.frame.border_box.inflate(spread, spread);

                        // Fill the color
                        scene.draw_box_shadow(transform, rect, shadow_color, radius, blur);
                    }
                }
            },
        )
    }

    pub(super) fn draw_inset_box_shadow(&self, scene: &mut impl PaintScene) {
        let current_color = self.style.clone_color();
        let box_shadow = &self.style.get_effects().box_shadow.0;
        let has_inset_shadow = box_shadow.iter().any(|s| s.inset);
        if !has_inset_shadow {
            return;
        }

        let padding_box = self.frame.padding_box_path();

        // As with outset shadows, the first shadow in the list is on top.
        for shadow in box_shadow.iter().filter(|s| s.inset).rev() {
            let shadow_color = shadow
                .base
                .color
                .resolve_to_absolute(&current_color)
                .as_srgb_color();
            if shadow_color.components[3] == 0.0 {
                continue;
            }

            let offset = Vec2::new(
                shadow.base.horizontal.px() as f64 * self.scale,
                shadow.base.vertical.px() as f64 * self.scale,
            );
            let spread = shadow.spread.px() as f64 * self.scale;
            let blur = shadow.base.blur.px() as f64 * self.scale;
            let hole_rect = self.frame.padding_box.inflate(-spread, -spread) + offset;

            // Start with padding-box radii (border radii minus the adjacent border
            // widths), then reduce each axis by spread, flooring at zero.
            let mut radii = self.frame.border_slice(1.0, 1.0).border_radii;
            for radius in [
                &mut radii.top_left,
                &mut radii.top_right,
                &mut radii.bottom_right,
                &mut radii.bottom_left,
            ] {
                radius.x = (radius.x - spread).max(0.0);
                radius.y = (radius.y - spread).max(0.0);
            }
            let hole = CssBox::new(hole_rect, Insets::ZERO, Insets::ZERO, 0.0, radii);
            let has_hole = hole_rect.width() > 0.0 && hole_rect.height() > 0.0;

            if blur == 0.0 || !has_hole {
                // Intersect the hole with the padding box before subtracting it.
                // Merely clipping their XOR would antialias the outside portion
                // of the hole back into the box at otherwise unshadowed corners.
                let ring = if has_hole {
                    inset_shadow_ring(&padding_box, &hole.border_box_path(), self.transform)
                } else {
                    padding_box.clone()
                };
                scene.fill(Fill::NonZero, self.transform, shadow_color, None, &ring);
                continue;
            }

            scene.push_layer(
                Fill::NonZero,
                Mix::Normal,
                1.0,
                self.transform,
                &padding_box,
                None,
                None,
            );
            scene.fill(
                Fill::NonZero,
                self.transform,
                shadow_color,
                None,
                &padding_box,
            );
            scene.push_layer(
                Fill::NonZero,
                Compose::DestOut,
                1.0,
                self.transform,
                &padding_box,
                None,
                None,
            );

            // The analytic renderer accepts only one circular radius. Partition
            // the hole into quadrants so each corner keeps its own radius, using
            // an axis scale for elliptical corners. The blur remains analytic
            // and approximate: that scale also stretches its Gaussian kernel.
            let center = hole_rect.center();
            let bounds = self.frame.padding_box;
            let r = hole.border_radii;
            if r.top_left == r.top_right
                && r.top_left == r.bottom_right
                && r.top_left == r.bottom_left
                && r.top_left.x == r.top_left.y
            {
                scene.draw_box_shadow(self.transform, hole_rect, Color::WHITE, r.top_left.x, blur);
            } else {
                for (radius, quadrant) in [
                    (
                        r.top_left,
                        Rect::new(bounds.x0, bounds.y0, center.x, center.y),
                    ),
                    (
                        r.top_right,
                        Rect::new(center.x, bounds.y0, bounds.x1, center.y),
                    ),
                    (
                        r.bottom_right,
                        Rect::new(center.x, center.y, bounds.x1, bounds.y1),
                    ),
                    (
                        r.bottom_left,
                        Rect::new(bounds.x0, center.y, center.x, bounds.y1),
                    ),
                ] {
                    let quadrant = quadrant.intersect(bounds);
                    if quadrant.width() <= 0.0 || quadrant.height() <= 0.0 {
                        continue;
                    }
                    let (radius, sx, sy) = if radius.x > 0.0 && radius.y > 0.0 {
                        let r = (radius.x * radius.y).sqrt();
                        (r, radius.x / r, radius.y / r)
                    } else {
                        (0.0, 1.0, 1.0)
                    };
                    let rect = Rect::new(
                        hole_rect.x0 / sx,
                        hole_rect.y0 / sy,
                        hole_rect.x1 / sx,
                        hole_rect.y1 / sy,
                    );
                    // Add the quadrant coverages rather than source-over blending
                    // them: fractional clip boundaries must not leave a seam.
                    scene.push_layer(
                        Fill::NonZero,
                        Compose::Plus,
                        1.0,
                        self.transform,
                        &quadrant,
                        None,
                        None,
                    );
                    scene.draw_box_shadow(
                        self.transform * Affine::scale_non_uniform(sx, sy),
                        rect,
                        Color::WHITE,
                        radius,
                        blur,
                    );
                    scene.pop_layer();
                }
            }
            scene.pop_layer();
            scene.pop_layer();
        }
    }
}

/// Subtract two convex rounded rectangles in geometry, before antialiasing.
/// Flatten below a hundredth of a device pixel, then intersect the hole with
/// the padding polygon using convex clipping. Shared edges cancel in the one
/// non-zero fill, including when the hole contains the entire padding box.
fn inset_shadow_ring(padding: &BezPath, hole: &BezPath, transform: Affine) -> BezPath {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let tolerance = 0.01 / (a.hypot(b) + c.hypot(d)).max(1.0);
    let polygon = |path: &BezPath| {
        let mut points = Vec::new();
        kurbo::flatten(path.iter(), tolerance, |el| match el {
            // Arc endpoints can differ from the next start by roundoff.
            // Such near-zero edges must not become clipping half-planes.
            PathEl::MoveTo(p) | PathEl::LineTo(p)
                if points
                    .last()
                    .is_none_or(|previous: &Point| previous.distance_squared(p) > 1e-16) =>
            {
                points.push(p);
            }
            _ => {}
        });
        if points
            .first()
            .zip(points.last())
            .is_some_and(|(first, last)| first.distance_squared(*last) <= 1e-16)
        {
            points.pop();
        }
        points
    };
    let outer = polygon(padding);
    let hole = polygon(hole);
    let mut intersection = outer.clone();
    let mut scratch = Vec::new();
    for (start, end) in hole.iter().zip(hole.iter().cycle().skip(1)) {
        if intersection.is_empty() {
            break;
        }
        let edge = *end - *start;
        let distance = |p: Point| edge.cross(p - *start);
        let mut previous = *intersection.last().unwrap();
        let mut previous_distance = distance(previous);
        for &point in &intersection {
            let point_distance = distance(point);
            if (point_distance >= 0.0) != (previous_distance >= 0.0) {
                let t = previous_distance / (previous_distance - point_distance);
                scratch.push(previous.lerp(point, t));
            }
            if point_distance >= 0.0 {
                scratch.push(point);
            }
            previous = point;
            previous_distance = point_distance;
        }
        std::mem::swap(&mut intersection, &mut scratch);
        scratch.clear();
    }
    if intersection == outer {
        return BezPath::new();
    }
    intersection.reverse();
    let mut ring = BezPath::new();
    for contour in [&outer, &intersection] {
        if let Some((&first, rest)) = contour.split_first() {
            ring.move_to(first);
            for &point in rest {
                ring.line_to(point);
            }
            ring.close_path();
        }
    }
    ring
}
