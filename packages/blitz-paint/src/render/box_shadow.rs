use super::ElementCx;
use crate::color::{Color, ToColorColor as _};
use crate::kurbo_css::{CssBox, NonUniformRoundedRectRadii};
use anyrender::PaintScene;
use blitz_dom::CopyStyleGetters;
use kurbo::{BezPath, Insets, PathEl, Point, Rect, Vec2};
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
        let [a, b, c, d, _, _] = self.transform.as_coeffs();
        let tolerance = 0.01 / (a.hypot(b) + c.hypot(d)).max(1.0);
        let mut padding_polygon = None;

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
            // widths). Apply the spread correction independently to each axis.
            let mut radii = self.frame.border_slice(1.0, 1.0).border_radii;
            for radius in [
                &mut radii.top_left,
                &mut radii.top_right,
                &mut radii.bottom_right,
                &mut radii.bottom_left,
            ] {
                radius.x = inset_spread_radius(radius.x, spread);
                radius.y = inset_spread_radius(radius.y, spread);
            }
            let hole = CssBox::new(hole_rect, Insets::ZERO, Insets::ZERO, 0.0, radii);
            let has_hole = hole_rect.width() > 0.0 && hole_rect.height() > 0.0;

            if blur == 0.0 || !has_hole {
                // Intersect the hole with the padding box before subtracting it.
                // Merely clipping their XOR would antialias the outside portion
                // of the hole back into the box at otherwise unshadowed corners.
                let key = InsetShadowKey {
                    padding_box: self.frame.padding_box,
                    border_width: self.frame.border_width,
                    border_radii: self.frame.border_radii,
                    offset,
                    spread,
                    tolerance,
                };
                let mut cache = self.context.inset_shadow_cache.borrow_mut();
                let ring = cache.get_or_insert_with(key, || {
                    if has_hole {
                        let outer = padding_polygon
                            .get_or_insert_with(|| shadow_polygon(&padding_box, tolerance));
                        inset_shadow_ring(
                            outer,
                            &shadow_polygon(&hole.border_box_path(), tolerance),
                        )
                    } else {
                        padding_box.clone()
                    }
                });
                scene.fill(Fill::NonZero, self.transform, shadow_color, None, ring);
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
            // holes with circular corners into quadrants to keep each radius.
            // Elliptical corners retain the previous analytic approximation.
            let center = hole_rect.center();
            let bounds = self.frame.padding_box;
            let r = hole.border_radii;
            if !hole.is_uniform_corner_border() {
                // The analytic primitive only supports circular corners. Keep
                // the previous averaged geometry for ellipses: scaling that
                // primitive would also stretch its isotropic Gaussian kernel.
                scene.draw_box_shadow(
                    self.transform,
                    hole_rect,
                    Color::WHITE,
                    self.frame.border_radii.average(),
                    blur,
                );
            } else if r.top_left == r.top_right
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
                    scene.draw_box_shadow(self.transform, hole_rect, Color::WHITE, radius.x, blur);
                    scene.pop_layer();
                }
            }
            scene.pop_layer();
            scene.pop_layer();
        }
    }
}

/// CSS Backgrounds 3 section 6.1.1: shrinking subtracts spread and floors
/// at zero; growing uses a cubic correction when a radius is smaller than the
/// growth distance. In particular, a square radius must remain square.
fn inset_spread_radius(radius: f64, spread: f64) -> f64 {
    let spread = if spread < 0.0 && radius < -spread {
        spread * (1.0 + (radius / -spread - 1.0).powi(3))
    } else {
        spread
    };
    (radius - spread).max(0.0)
}

/// Flatten below a hundredth of a device pixel. Reuse the padding polygon for
/// every unblurred shadow on the element.
fn shadow_polygon(path: &BezPath, tolerance: f64) -> Vec<Point> {
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
}

/// Subtract two convex rounded rectangles in geometry, before antialiasing.
/// Shared edges cancel in the one non-zero fill, including when the hole
/// contains the entire padding box.
fn inset_shadow_ring(outer: &[Point], hole: &[Point]) -> BezPath {
    let mut intersection = outer.to_vec();
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
    shadow_ring(outer, &intersection)
}

fn shadow_ring(outer: &[Point], intersection: &[Point]) -> BezPath {
    if intersection == outer {
        return BezPath::new();
    }
    let mut ring = BezPath::new();
    if let Some((&first, rest)) = outer.split_first() {
        ring.move_to(first);
        for &point in rest {
            ring.line_to(point);
        }
        ring.close_path();
    }
    if let Some((&first, rest)) = intersection.split_last() {
        ring.move_to(first);
        for &point in rest.iter().rev() {
            ring.line_to(point);
        }
        ring.close_path();
    }
    ring
}

#[derive(PartialEq)]
struct InsetShadowKey {
    padding_box: Rect,
    border_width: Insets,
    border_radii: NonUniformRoundedRectRadii,
    offset: Vec2,
    spread: f64,
    tolerance: f64,
}

/// Reuse identical shadow geometry across elements in one paint pass. Bound the
/// cache so pages with unique shapes cannot accumulate unbounded path storage.
/// Color and the element position do not affect the cached local geometry.
#[derive(Default)]
pub(super) struct InsetShadowCache {
    entries: std::collections::VecDeque<(InsetShadowKey, BezPath)>,
}

impl InsetShadowCache {
    fn get_or_insert_with(
        &mut self,
        key: InsetShadowKey,
        build: impl FnOnce() -> BezPath,
    ) -> &BezPath {
        if let Some(index) = self.entries.iter().position(|(cached, _)| *cached == key) {
            return &self.entries[index].1;
        }
        if self.entries.len() == 16 {
            self.entries.pop_front();
        }
        self.entries.push_back((key, build()));
        &self.entries.back().unwrap().1
    }
}
