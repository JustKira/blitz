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
            // Without borders or spread, the hole is a translated copy of the
            // outline. Avoid rebuilding and flattening it for each shadow.
            if blur == 0.0 && spread == 0.0 && self.frame.border_width == Insets::ZERO {
                if offset != Vec2::ZERO {
                    let outer = padding_polygon.get_or_insert_with(|| {
                        rounded_shadow_polygon(
                            &padding_box,
                            self.frame.padding_box,
                            self.frame.border_radii,
                            tolerance,
                        )
                    });
                    let ring = translated_shadow_ring(outer, offset);
                    scene.fill(Fill::NonZero, self.transform, shadow_color, None, &ring);
                }
                continue;
            }
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
                let ring = if has_hole {
                    let outer = padding_polygon.get_or_insert_with(|| {
                        if self.frame.border_width == Insets::ZERO {
                            rounded_shadow_polygon(
                                &padding_box,
                                self.frame.padding_box,
                                self.frame.border_radii,
                                tolerance,
                            )
                        } else {
                            shadow_polygon(&padding_box, tolerance)
                        }
                    });
                    let hole_polygon = rounded_shadow_polygon(
                        &hole.border_box_path(),
                        hole_rect,
                        hole.border_radii,
                        tolerance,
                    );
                    // A centered, shrunken hole is contained by the outline.
                    if offset == Vec2::ZERO
                        && spread > 0.0
                        && self.frame.border_width == Insets::ZERO
                    {
                        shadow_ring(outer, &hole_polygon)
                    } else {
                        inset_shadow_ring(outer, &hole_polygon)
                    }
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

/// Equal circular corners are reflections of the same flattened quarter arc.
/// Flatten it once for this outline; no geometry is retained across elements.
fn rounded_shadow_polygon(
    path: &BezPath,
    bounds: Rect,
    radii: NonUniformRoundedRectRadii,
    tolerance: f64,
) -> Vec<Point> {
    let top = radii.top_left;
    let uniform = radii.bottom_left == top && radii.bottom_right == top;
    let square_bottom = radii.bottom_left == Vec2::ZERO && radii.bottom_right == Vec2::ZERO;
    if top.x <= 0.0 || top.x != top.y || radii.top_right != top || !(uniform || square_bottom) {
        return shadow_polygon(path, tolerance);
    }
    let end = path
        .elements()
        .iter()
        .skip(1)
        .position(|el| matches!(el, PathEl::LineTo(_)))
        .map_or(path.elements().len(), |i| i + 1);
    let corner = BezPath::from_vec(path.elements()[..end].to_vec());
    let quarter = shadow_polygon(&corner, tolerance);
    let mut points = Vec::with_capacity(quarter.len() * 4);
    let sx = bounds.x0 + bounds.x1;
    let sy = bounds.y0 + bounds.y1;
    points.extend(quarter.iter().copied());
    points.extend(quarter.iter().rev().map(|p| Point::new(sx - p.x, p.y)));
    if uniform {
        points.extend(quarter.iter().map(|p| Point::new(sx - p.x, sy - p.y)));
        points.extend(quarter.iter().rev().map(|p| Point::new(p.x, sy - p.y)));
    } else {
        points.push(Point::new(bounds.x1, bounds.y1));
        points.push(Point::new(bounds.x0, bounds.y1));
    }
    // Pill corners can meet without an intervening straight edge.
    points.dedup_by(|a, b| a.distance_squared(*b) <= 1e-16);
    if points
        .first()
        .zip(points.last())
        .is_some_and(|(a, b)| a.distance_squared(*b) <= 1e-16)
    {
        points.pop();
    }
    points
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

/// Intersect a convex outline with its translated copy in linear time.
/// Each pair of corresponding edges is parallel: retain the stricter half-plane.
/// The edges are already in angular order, so a deque removes each redundant
/// edge at most once. Subtract the resulting intersection in the same fill to
/// cancel shared edges before antialiasing.
fn translated_shadow_ring(outer: &[Point], offset: Vec2) -> BezPath {
    use std::collections::VecDeque;

    if offset == Vec2::ZERO {
        return BezPath::new();
    }
    let mut boundary: VecDeque<ShadowEdge> = VecDeque::with_capacity(outer.len());
    let mut vertices: VecDeque<Point> = VecDeque::with_capacity(outer.len());
    for (&start, &end) in outer.iter().zip(outer.iter().cycle().skip(1)) {
        let direction = Vec2::new(end.x - start.x, end.y - start.y);
        let edge = if cross(direction, offset) > 0.0 {
            ShadowEdge {
                start: Point::new(start.x + offset.x, start.y + offset.y),
                end: Point::new(end.x + offset.x, end.y + offset.y),
            }
        } else {
            ShadowEdge { start, end }
        };
        while vertices.back().is_some_and(|&p| !edge.contains(p)) {
            boundary.pop_back();
            vertices.pop_back();
        }
        while vertices.front().is_some_and(|&p| !edge.contains(p)) {
            boundary.pop_front();
            vertices.pop_front();
        }
        if let Some(previous) = boundary.back() {
            let Some(point) = previous.intersection(edge) else {
                let hole: Vec<_> = outer
                    .iter()
                    .map(|p| Point::new(p.x + offset.x, p.y + offset.y))
                    .collect();
                return inset_shadow_ring(outer, &hole);
            };
            vertices.push_back(point);
        }
        boundary.push_back(edge);
    }
    while vertices.back().is_some_and(|&p| !boundary[0].contains(p)) {
        boundary.pop_back();
        vertices.pop_back();
    }
    while vertices
        .front()
        .is_some_and(|&p| !boundary.back().unwrap().contains(p))
    {
        boundary.pop_front();
        vertices.pop_front();
    }
    if boundary.len() < 3 {
        return shadow_ring(outer, &[]);
    }
    let Some(point) = boundary.back().unwrap().intersection(boundary[0]) else {
        let hole: Vec<_> = outer
            .iter()
            .map(|p| Point::new(p.x + offset.x, p.y + offset.y))
            .collect();
        return inset_shadow_ring(outer, &hole);
    };
    vertices.push_back(point);
    shadow_ring(outer, vertices.make_contiguous())
}

#[derive(Clone, Copy)]
struct ShadowEdge {
    start: Point,
    end: Point,
}

impl ShadowEdge {
    fn direction(self) -> Vec2 {
        Vec2::new(self.end.x - self.start.x, self.end.y - self.start.y)
    }

    fn contains(self, point: Point) -> bool {
        cross(
            self.direction(),
            Vec2::new(point.x - self.start.x, point.y - self.start.y),
        ) >= 0.0
    }

    fn intersection(self, other: Self) -> Option<Point> {
        if self.end == other.start {
            return Some(self.end);
        }
        let d = self.direction();
        let denominator = cross(d, other.direction());
        (denominator != 0.0).then(|| {
            let t = cross(
                Vec2::new(other.start.x - self.start.x, other.start.y - self.start.y),
                other.direction(),
            ) / denominator;
            Point::new(self.start.x + d.x * t, self.start.y + d.y * t)
        })
    }
}

fn cross(a: Vec2, b: Vec2) -> f64 {
    a.x * b.y - a.y * b.x
}

/// General clipping for spread-adjusted holes and padding boxes with borders.
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
    let mut ring = Vec::with_capacity(outer.len() + intersection.len() + 4);
    if let Some((&first, rest)) = outer.split_first() {
        ring.push(PathEl::MoveTo(first));
        ring.extend(rest.iter().copied().map(PathEl::LineTo));
        ring.push(PathEl::ClosePath);
    }
    if let Some((&first, rest)) = intersection.split_last() {
        ring.push(PathEl::MoveTo(first));
        ring.extend(rest.iter().rev().copied().map(PathEl::LineTo));
        ring.push(PathEl::ClosePath);
    }
    BezPath::from_vec(ring)
}
