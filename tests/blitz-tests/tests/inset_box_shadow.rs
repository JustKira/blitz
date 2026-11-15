//! Inset shadows are the padding box minus an offset, spread-adjusted hole.
//! Check device pixels, including edges where that difference has zero area.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const WIDTH: usize = 200;
const HEIGHT: usize = 240;

struct Pixels {
    data: Vec<u8>,
    scale: usize,
}

impl Pixels {
    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * WIDTH * self.scale + x) * 4;
        self.data[i..i + 3].try_into().unwrap()
    }

    fn css(&self, x: usize, y: usize) -> [u8; 3] {
        self.at(x * self.scale, y * self.scale)
    }
}

fn render(html: &str, scale: usize) -> Pixels {
    let width = (WIDTH * scale) as u32;
    let height = (HEIGHT * scale) as u32;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(
                width,
                height,
                scale as f32,
                ColorScheme::Light,
            )),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let data = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, scale as f64, width, height, 0, 0),
        width,
        height,
    );
    Pixels { data, scale }
}

fn box_html(style: &str) -> String {
    format!(
        "<!doctype html><body style='margin:20px;background:#777'>\
         <div style='width:120px;height:44px;background:black;{style}'></div></body>"
    )
}

fn box_pixels(style: &str, scale: usize) -> Pixels {
    render(&box_html(style), scale)
}

fn near(actual: [u8; 3], expected: u8, tolerance: u8) {
    assert!(
        actual.iter().all(|c| c.abs_diff(expected) <= tolerance),
        "pixel {actual:?}, expected {expected} ± {tolerance}"
    );
}

#[test]
fn fractional_offset_scales_in_device_pixels() {
    for scale in [1, 2] {
        let pixels = box_pixels("box-shadow:inset 0 1.5px 0 white", scale);
        let values: Vec<_> = (0..5 * scale)
            .map(|dy| pixels.at(80 * scale, 20 * scale + dy)[0])
            .collect();
        let expected = if scale == 1 {
            vec![255, 128, 0, 0, 0]
        } else {
            vec![255, 255, 255, 0, 0, 0, 0, 0, 0, 0]
        };
        for (&actual, &expected) in values.iter().zip(&expected) {
            assert!(actual.abs_diff(expected) <= 1, "scale {scale}: {values:?}");
        }
    }
}

#[test]
fn negative_offset_and_square_corners_keep_their_thickness() {
    for scale in [1, 2] {
        let pixels = box_pixels(
            "border-radius:16px 16px 0 0;box-shadow:inset 0 -3px 0 white",
            scale,
        );
        for x in [20 * scale, 21 * scale, 80 * scale, 140 * scale - 1] {
            for y in 50 * scale..64 * scale {
                near(pixels.at(x, y), if y >= 61 * scale { 255 } else { 0 }, 0);
            }
        }
    }
}

#[test]
fn horizontal_shadow_has_no_zero_thickness_edge_or_corner_slivers() {
    for scale in [1, 2] {
        let pixels = box_pixels("border-radius:7px;box-shadow:inset 3px 0 0 white", scale);
        let control = box_pixels("border-radius:7px", scale);
        // The translated hole contains the whole right half, even at the arcs.
        for y in 19 * scale..65 * scale {
            for x in 80 * scale..141 * scale {
                assert_eq!(
                    pixels.at(x, y),
                    control.at(x, y),
                    "scale {scale}, ({x},{y})"
                );
            }
        }
        for x in 35 * scale..125 * scale {
            for y in [20 * scale, 64 * scale - 1] {
                assert_eq!(pixels.at(x, y), [0; 3], "scale {scale}, ({x},{y})");
            }
        }
        for dx in 0..5 * scale {
            near(
                pixels.at(20 * scale + dx, 42 * scale),
                if dx < 3 * scale { 255 } else { 0 },
                0,
            );
        }
    }
}

#[test]
fn transparent_shadow_does_not_drop_later_shadows() {
    for scale in [1, 2] {
        for shadows in [
            "inset 0 0 transparent,inset 0 -3px white",
            "inset 0 -3px white,inset 0 0 transparent",
            "inset 0 0 rgb(255 0 0 / 0),inset 0 -3px white,inset 0 0 transparent",
        ] {
            let pixels = box_pixels(&format!("box-shadow:{shadows}"), scale);
            for dy in 0..5 * scale {
                near(
                    pixels.at(80 * scale, 64 * scale - 1 - dy),
                    if dy < 3 * scale { 255 } else { 0 },
                    0,
                );
            }
        }
    }
}

#[test]
fn first_shadow_is_on_top() {
    for scale in [1, 2] {
        let pixels = box_pixels(
            "box-shadow:inset 0 0 0 4px white,inset 0 0 0 8px red",
            scale,
        );
        assert_eq!(pixels.css(21, 42), [255; 3]);
        assert_eq!(pixels.css(25, 42), [255, 0, 0]);
        assert_eq!(pixels.css(29, 42), [0; 3]);
    }
}

#[test]
fn spread_scales_and_can_close_the_hole() {
    for scale in [1, 2] {
        let pixels = box_pixels("box-shadow:inset 0 0 0 1.5px white", scale);
        for dx in 0..5 * scale {
            let expected = if dx < scale {
                255
            } else if scale == 1 && dx == 1 {
                128
            } else if dx < 3 && scale == 2 {
                255
            } else {
                0
            };
            near(pixels.at(20 * scale + dx, 42 * scale), expected, 1);
        }
        let closed = box_pixels(
            "border-radius:16px;box-shadow:inset 0 0 0 30px white",
            scale,
        );
        near(closed.css(80, 42), 255, 0);
    }
}

#[test]
fn hole_uses_padding_radii_and_preserves_elliptical_corners() {
    for scale in [1, 2] {
        // The padding top-left radius is (24, 8), and spread reduces it to (21, 5).
        let pixels = box_pixels(
            "border:4px solid #777;border-radius:28px 12px 0 0 / 12px 28px 0 0;box-shadow:inset 0 0 0 3px white",
            scale,
        );
        near(pixels.css(30, 27), 255, 0);
        near(pixels.css(31, 31), 0, 0);
        near(pixels.css(24, 50), 255, 0);
        near(pixels.css(27, 50), 0, 0);
        near(pixels.css(28, 63), 0, 0);
        near(pixels.css(24, 65), 255, 0);
    }
}

#[test]
fn negative_spread_can_remove_the_shadow() {
    for scale in [1, 2] {
        let pixels = box_pixels(
            "border-radius:16px 16px 0 0;box-shadow:inset 0 0 0 -4px white",
            scale,
        );
        let control = box_pixels("border-radius:16px 16px 0 0", scale);
        let differences: Vec<_> = pixels
            .data
            .chunks_exact(4)
            .zip(control.data.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(10)
            .collect();
        assert!(
            differences.is_empty(),
            "negative spread scale {scale}: {differences:?}"
        );
    }
}

#[test]
fn blurred_shadow_scales_and_keeps_square_corners_square() {
    for scale in [1, 2] {
        let pixels = box_pixels(
            "border-radius:16px 16px 0 0;box-shadow:inset 0 -6px 2px white",
            scale,
        );
        let control = box_pixels("box-shadow:inset 0 -6px 2px white", scale);
        near(pixels.css(80, 50), 0, 2);
        // Square bottom corners must match an otherwise square control box.
        for y in 53..63 {
            let left = pixels.css(24, y)[0];
            let square = control.css(24, y)[0];
            assert!(
                left.abs_diff(square) <= 3,
                "scale {scale}, row {y}: left {left}, square control {square}"
            );
        }
        assert!(
            pixels.css(80, 57)[0] > 65 && pixels.css(80, 57)[0] < 140,
            "offset and blur must both scale"
        );
        assert!(pixels.css(80, 60)[0] > 200);
    }
    // Differing corner radii use separate analytic pieces. Their fractional
    // boundaries must not produce a cross through the unshadowed center.
    for scale in [1, 2] {
        let pixels = box_pixels(
            "width:119.5px;height:43.5px;border-radius:7px 8px 9px 10px;box-shadow:inset 0 0 3px 3px white",
            scale,
        );
        for y in 36 * scale..47 * scale {
            for x in 74 * scale..85 * scale {
                near(pixels.at(x, y), 0, 1);
            }
        }
    }
}

const APP_PAGE: &str = r#"<!doctype html>
<body style="margin:40px;background:#1b1b1b">
<div style="width:120px;height:44px;border-radius:10px;background:#2a2a2a;box-shadow:inset 0 1.5px 0 rgb(255 255 255 / .18), inset 0 -1.5px 0 rgb(0 0 0 / .45), inset 0 0 0 1.5px rgb(255 255 255 / 0)"></div>
<div style="margin-top:12px;width:120px;height:44px;border-radius:10px;background:#2a2a2a;box-shadow:inset 0 1.5px 0 rgb(255 255 255 / .0135), inset 0 -1.5px 0 rgb(0 0 0 / .03375), inset 0 0 0 1.5px rgb(255 255 255 / .055)"></div>
<div style="margin-top:12px;width:120px;height:34px;border-radius:8px 8px 0 0;background:#2a2a2a;box-shadow:inset 0 1.5px 0 rgb(255 255 255 / .18), inset 0 -1.5px 0 rgb(0 0 0 / .45)"></div>
</body>"#;

#[test]
fn app_page_has_device_pixel_bands_and_clean_square_corners() {
    for scale in [1, 2] {
        let pixels = render(APP_PAGE, scale);
        for (top, bottom, light, dark) in [(40, 84, 80, 23), (96, 140, 55, 51), (152, 186, 80, 23)]
        {
            for dy in 0..4 * scale {
                let coverage = (1.5 * scale as f64 - dy as f64).clamp(0.0, 1.0);
                let top_expected = (42.0 + (light as f64 - 42.0) * coverage).round() as u8;
                let bottom_expected = (42.0 + (dark as f64 - 42.0) * coverage).round() as u8;
                near(pixels.at(100 * scale, top * scale + dy), top_expected, 2);
                near(
                    pixels.at(100 * scale, bottom * scale - 1 - dy),
                    bottom_expected,
                    2,
                );
            }
        }
        for (top, bottom) in [(40, 84), (96, 140), (152, 186)] {
            for x in [40, 159] {
                near(pixels.css(x, top), 27, 0);
                if top != 152 {
                    near(pixels.css(x, bottom - 1), 27, 0);
                }
            }
        }
        // The last card's square corners must have the same flat bottom band as its center.
        for y in 174 * scale..186 * scale {
            for x in [40 * scale, 41 * scale, 159 * scale] {
                assert_eq!(
                    pixels.at(x, y),
                    pixels.at(100 * scale, y),
                    "scale {scale}, ({x},{y})"
                );
            }
        }
        // The side of the first card is untouched between its top/bottom bands.
        for y in 52 * scale..72 * scale {
            near(pixels.at(40 * scale, y), 42, 0);
            near(pixels.at(159 * scale, y), 42, 0);
        }
    }
}

#[test]
fn negative_spread_preserves_square_radius_axes() {
    for scale in [1, 2] {
        for radius in ["0", "0 / 12px", "12px / 0"] {
            let style = format!("height:80px;border-radius:{radius}");
            let control = box_pixels(&style, scale);
            let shadow = box_pixels(
                &format!("{style};box-shadow:inset 20px 20px 0 -20px white"),
                scale,
            );
            assert!(
                shadow.data == control.data,
                "radius {radius}, scale {scale}"
            );
        }
    }
}

#[test]
fn elliptical_blur_keeps_an_isotropic_kernel() {
    for scale in [1, 2] {
        for radius in ["40px / 8px", "8px / 40px"] {
            let pixels = box_pixels(
                &format!("height:80px;border-radius:{radius};box-shadow:inset 0 0 8px 3px white"),
                scale,
            );
            // Samples on the straight edges, away from the corner approximation.
            // Chrome is 25 at 1x and 22 at 2x at both points. Allow the existing
            // analytic blur's profile error, but not an axis-stretched kernel.
            for (x, y) in [(28, 60), (80, 28)] {
                near(pixels.css(x, y), if scale == 1 { 25 } else { 22 }, 25);
            }
        }
        let unequal = box_pixels(
            "height:80px;border-radius:55px 10px 25px 3px / 5px 35px 15px 2px;box-shadow:inset 4px -3px 7px 2px white",
            scale,
        );
        // This interior point is black in Chrome. A stretched kernel bleeds
        // about 62 levels of white into it in the regressed implementation.
        near(unequal.css(40, 40), 0, 5);
    }
}
