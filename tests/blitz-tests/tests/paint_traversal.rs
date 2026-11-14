//! Painting with an `ElementPainter`: the app paints each element from Blitz's pieces.

use anyrender::{NullScenePainter, PaintScene, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig, local_name};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::{
    DefaultPainter, ElementPaint, ElementPainter, Piece, paint_scene, paint_scene_with,
    walk_elements,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{Circle, Point, Rect};
use peniko::{Color, Fill};
use std::cell::RefCell;
use std::sync::Arc;

const RED: Color = Color::from_rgb8(255, 0, 0);
const WHITE: [u8; 3] = [255, 255, 255];
const BLUE: [u8; 3] = [0, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];

/// A 100x100 blue box with a 10px green border at the top-left of a 200x200 white page. It
/// holds a 40x40 black child.
const PAGE: &str = r#"<html><body style="margin:0; background:#ffffff">
    <div id="box" style="box-sizing:border-box; width:100px; height:100px;
        background:#0000ff; border:10px solid #00ff00;">
        <div id="child" style="width:40px; height:40px; background:#000000"></div>
    </div>
</body></html>"#;

fn document(html: &str) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 200, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn id<'a, S: PaintScene>(el: &ElementPaint<'a, S>) -> Option<&'a str> {
    el.node().attr(local_name!("id"))
}

/// Paint `doc` with `painter` and return the whole picture.
fn render(doc: &mut BaseDocument, painter: impl for<'a> Painter<'a>) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_with(scene, doc, 1.0, 200, 200, 0, 0, &painter),
        200,
        200,
    )
}

/// An `ElementPainter` for the scene type of the CPU renderer, whatever its lifetime.
trait Painter<'a>:
    ElementPainter<<VelloCpuImageRenderer as anyrender::ImageRenderer>::ScenePainter<'a>>
{
}
impl<'a, T> Painter<'a> for T where
    T: ElementPainter<<VelloCpuImageRenderer as anyrender::ImageRenderer>::ScenePainter<'a>>
{
}

fn pixel(buffer: &[u8], at: (u32, u32)) -> [u8; 3] {
    let idx = ((at.1 * 200 + at.0) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

#[test]
fn the_default_painter_paints_what_paint_scene_paints() {
    let mut doc = document(PAGE);
    let plain = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 200, 200, 0, 0),
        200,
        200,
    );
    assert_eq!(render(&mut doc, DefaultPainter), plain);
    assert_eq!(pixel(&plain, (60, 60)), BLUE);
    assert_eq!(pixel(&plain, (5, 50)), GREEN);
    assert_eq!(pixel(&plain, (20, 20)), [0, 0, 0]);
}

/// The pieces written out by hand, in order, paint the same picture as `paint`.
#[test]
fn the_pieces_in_order_are_the_whole_element() {
    struct ByHand;
    impl<S: PaintScene> ElementPainter<S> for ByHand {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            el.with_box_layers(scene, |el, scene| {
                el.outline(scene);
                el.outer_shadow(scene);
                el.with_effect_layers(scene, |el, scene| {
                    el.background(scene);
                    el.inset_shadow(scene);
                    el.table_parts(scene);
                    el.border(scene);
                    el.with_content_clip(scene, |el, scene| {
                        el.own_content(scene);
                        el.children(scene);
                    });
                    el.scrollbars(scene);
                });
            });
        }
    }
    let page = PAGE.replace(
        "background:#0000ff;",
        "background:#0000ff; opacity:0.5; overflow:hidden; border-radius:20px;
         box-shadow: 10px 10px 4px #ff0000;",
    );
    let mut doc = document(&page);
    assert_eq!(render(&mut doc, ByHand), render(&mut doc, DefaultPainter));
}

/// An app draws one element's background itself and lets Blitz paint the rest of it: the
/// border, the content and the children stay.
#[test]
fn one_piece_can_be_replaced() {
    struct OwnBackground;
    impl<S: PaintScene> ElementPainter<S> for OwnBackground {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if id(el) != Some("box") {
                return el.paint(scene);
            }
            el.with_box_layers(scene, |el, scene| {
                el.with_effect_layers(scene, |el, scene| {
                    let shape = el.css_box().border_box;
                    scene.fill(Fill::NonZero, el.transform(), RED, None, &shape);
                    el.border(scene);
                    el.with_content_clip(scene, |el, scene| el.children(scene));
                });
            });
        }
    }
    let picture = render(&mut document(PAGE), OwnBackground);
    assert_eq!(pixel(&picture, (60, 60)), [255, 0, 0], "own background");
    assert_eq!(pixel(&picture, (5, 50)), GREEN, "Blitz's border, over it");
    assert_eq!(pixel(&picture, (20, 20)), [0, 0, 0], "the child");
    assert_eq!(pixel(&picture, (150, 150)), WHITE, "other elements");
}

/// The same replacement without writing the order out: Blitz's sequence and layers, with a say
/// in each piece. The pieces arrive in Blitz's order.
#[test]
fn one_piece_can_be_replaced_in_the_default_sequence() {
    struct OwnBackground(RefCell<Vec<Piece>>);
    impl<S: PaintScene> ElementPainter<S> for OwnBackground {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            let ours = id(el) == Some("box");
            el.paint_with(scene, |el, scene, piece| {
                if ours {
                    self.0.borrow_mut().push(piece);
                }
                if ours && piece == Piece::Background && el.paints_own_box() {
                    let shape = el.css_box().border_box;
                    scene.fill(Fill::NonZero, el.transform(), RED, None, &shape);
                } else {
                    el.piece(scene, piece);
                }
            });
        }
    }
    let painter = OwnBackground(RefCell::new(Vec::new()));
    let mut doc = document(PAGE);
    let picture = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene_with(scene, &mut doc, 1.0, 200, 200, 0, 0, &painter),
        200,
        200,
    );
    assert_eq!(pixel(&picture, (60, 60)), [255, 0, 0], "own background");
    assert_eq!(pixel(&picture, (5, 50)), GREEN, "Blitz's border, over it");
    assert_eq!(pixel(&picture, (20, 20)), [0, 0, 0], "the child");
    assert_eq!(
        *painter.0.borrow(),
        [
            Piece::Outline,
            Piece::OuterShadow,
            Piece::Background,
            Piece::InsetShadow,
            Piece::TableParts,
            Piece::Border,
            Piece::OwnContent,
            Piece::Children,
            Piece::Scrollbars,
        ]
    );
}

/// An inline element that holds only blocks paints no box. A painter can ask.
#[test]
fn an_inline_wrapper_of_blocks_paints_no_box() {
    struct Ask(RefCell<Vec<(String, bool)>>);
    impl<S: PaintScene> ElementPainter<S> for Ask {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if let Some(name) = id(el) {
                self.0
                    .borrow_mut()
                    .push((name.to_string(), el.paints_own_box()));
            }
            el.paint(scene);
        }
    }
    let page = r#"<html><body style="margin:0">
        <span id="wrapper"><div id="block" style="height:20px"></div></span>
    </body></html>"#;
    let asked = Ask(RefCell::new(Vec::new()));
    let mut doc = document(page);
    paint_scene_with(&mut NullScenePainter, &mut doc, 1.0, 200, 200, 0, 0, &asked);
    assert_eq!(
        *asked.0.borrow(),
        [("wrapper".to_string(), false), ("block".to_string(), true)]
    );
}

/// The content of an element, children included, is clipped to a shape the app chooses.
#[test]
fn content_can_be_clipped_to_a_custom_shape() {
    struct RoundContent;
    impl<S: PaintScene> ElementPainter<S> for RoundContent {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if id(el) != Some("box") {
                return el.paint(scene);
            }
            el.with_box_layers(scene, |el, scene| {
                el.background(scene);
                // A circle in the middle of the box: the child's corner at (10, 10) is cut
                let circle = Circle::new(Point::new(50.0, 50.0), 30.0);
                scene.push_clip_layer(Fill::NonZero, el.transform(), &circle);
                el.with_content_clip(scene, |el, scene| el.children(scene));
                scene.pop_layer();
            });
        }
    }
    let picture = render(&mut document(PAGE), RoundContent);
    assert_eq!(pixel(&picture, (12, 12)), BLUE, "child cut away here");
    assert_eq!(
        pixel(&picture, (40, 40)),
        [0, 0, 0],
        "child inside the circle"
    );
}

/// A subtree is wrapped in a layer of the app's own: here, one that fades it.
#[test]
fn a_subtree_can_be_wrapped_in_a_layer() {
    struct Faded;
    impl<S: PaintScene> ElementPainter<S> for Faded {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if id(el) != Some("box") {
                return el.paint(scene);
            }
            let bounds = el.css_box().border_box;
            scene.push_layer(
                Fill::NonZero,
                peniko::BlendMode::default(),
                0.5,
                el.transform(),
                &bounds,
                None,
                None,
            );
            el.paint(scene);
            scene.pop_layer();
        }
    }
    let picture = render(&mut document(PAGE), Faded);
    // The black child at half strength over white, through the blue box at half strength
    let [r, g, b] = pixel(&picture, (20, 20));
    assert!((120..=135).contains(&r) && r == g && g == b, "{r} {g} {b}");
}

/// Children are painted when the app says so, so it can draw under and over them, and a
/// child is painted by the same painter.
#[test]
fn the_app_draws_between_its_own_pieces_and_children_use_the_same_painter() {
    struct Trace(RefCell<Vec<String>>);
    impl<S: PaintScene> ElementPainter<S> for Trace {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            let name = id(el).unwrap_or("-").to_string();
            self.0.borrow_mut().push(format!("<{name}>"));
            el.with_content_clip(scene, |el, scene| el.children(scene));
            self.0.borrow_mut().push(format!("</{name}>"));
        }
    }
    let trace = Trace(RefCell::new(Vec::new()));
    let mut doc = document(PAGE);
    paint_scene_with(&mut NullScenePainter, &mut doc, 1.0, 200, 200, 0, 0, &trace);
    assert_eq!(
        trace.0.borrow().join(""),
        "<-><-><box><child></child></box></-></->"
    );

    // The root element's call encloses everything, a z-indexed and a fixed descendant included
    let page = PAGE.replace(
        "</body>",
        r#"<div id="high" style="position:relative; z-index:5"></div>
           <div id="fixed" style="position:fixed; top:0; left:0; width:5px; height:5px"></div>
           </body>"#,
    );
    let trace = Trace(RefCell::new(Vec::new()));
    paint_scene_with(
        &mut NullScenePainter,
        &mut document(&page),
        1.0,
        200,
        200,
        0,
        0,
        &trace,
    );
    let trace = trace.0.borrow().join("");
    assert!(
        trace.starts_with("<->") && trace.ends_with("</->"),
        "{trace}"
    );
    assert!(
        trace.contains("<high></high>") && trace.contains("<fixed></fixed>"),
        "{trace}"
    );
    assert_eq!(
        trace.matches("<->").count(),
        2,
        "one html, one body: {trace}"
    );
}

/// Walking without drawing gives where every element lands, before anything is painted.
#[test]
fn a_walk_into_no_scene_collects_positions() {
    struct Collect(RefCell<Vec<(String, Rect)>>);
    impl<S: PaintScene> ElementPainter<S> for Collect {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if let Some(name) = id(el) {
                let on_screen = el.transform().transform_rect_bbox(el.css_box().border_box);
                self.0.borrow_mut().push((name.to_string(), on_screen));
            }
            el.with_content_clip(scene, |el, scene| el.children(scene));
        }
    }
    let page = PAGE.replace(
        "box-sizing:border-box;",
        "box-sizing:border-box; margin:20px 0 0 50px; transform:translate(10px, 0);",
    );
    let found = Collect(RefCell::new(Vec::new()));
    let mut doc = document(&page);
    paint_scene_with(&mut NullScenePainter, &mut doc, 1.0, 200, 200, 0, 0, &found);

    // `walk_elements` is the same walk, for a document that is only read
    let walked = Collect(RefCell::new(Vec::new()));
    walk_elements(&doc, 1.0, 200, 200, 0, 0, &walked);
    assert_eq!(*walked.0.borrow(), *found.0.borrow());
    assert_eq!(
        *found.0.borrow(),
        [
            ("box".to_string(), Rect::new(60.0, 20.0, 160.0, 120.0)),
            ("child".to_string(), Rect::new(70.0, 30.0, 110.0, 70.0)),
        ]
    );
}

/// A descendant with a `z-index` is painted by the ancestor that owns its stacking context:
/// an element that leaves out its children does not leave it out.
#[test]
fn a_hoisted_descendant_is_not_one_of_its_parents_paint_children() {
    struct SkipCard(RefCell<Vec<String>>);
    impl<S: PaintScene> ElementPainter<S> for SkipCard {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            if let Some(name) = id(el) {
                self.0.borrow_mut().push(name.to_string());
            }
            if id(el) != Some("card") {
                el.with_content_clip(scene, |el, scene| el.children(scene));
            }
        }
    }
    let page = r#"<html><body style="margin:0">
        <div id="card">
            <div id="normal" style="height:10px"></div>
            <div id="badge" style="position:relative; z-index:1; height:10px"></div>
        </div>
    </body></html>"#;
    let seen = SkipCard(RefCell::new(Vec::new()));
    walk_elements(&document(page), 1.0, 200, 200, 0, 0, &seen);
    assert_eq!(*seen.0.borrow(), ["card", "badge"]);
}

/// While one element is painted, a painter can ask where any other element lands, including
/// one that is painted later, and draw there.
#[test]
fn a_painter_can_locate_other_elements_while_painting() {
    struct Wire;
    impl<S: PaintScene> ElementPainter<S> for Wire {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            el.paint(scene);
            if id(el) != Some("first") {
                return;
            }
            // `second` comes later in the document and has not been painted yet
            let second = el.document().query_selector("#second").unwrap().unwrap();
            let (transform, css_box) = el.locate(second).unwrap();
            assert_eq!(
                transform.transform_rect_bbox(css_box.border_box),
                Rect::new(110.0, 140.0, 150.0, 170.0)
            );
            // The same answer as that element gives about itself
            assert_eq!(el.locate(el.node().id).unwrap().0, el.transform());
            assert!(el.locate(el.document().root_node().id).is_none());

            scene.fill(Fill::NonZero, transform, RED, None, &css_box.border_box);
        }
    }
    let page = r#"<html><body style="margin:0; background:#ffffff">
        <div id="first" style="width:20px; height:20px"></div>
        <div id="second" style="position:absolute; left:100px; top:140px; width:40px;
            height:30px; transform:translate(10px, 0)"></div>
    </body></html>"#;
    let picture = render(&mut document(page), Wire);
    assert_eq!(pixel(&picture, (130, 155)), [255, 0, 0]);
    assert_eq!(pixel(&picture, (105, 155)), WHITE, "moved by its transform");
}

/// A painter that keeps renderer resources between paints is told when they are gone. One
/// that keeps nothing does not have to say anything about it.
#[test]
fn a_painter_is_told_that_the_renderer_dropped_its_resources() {
    struct Keeps(RefCell<Vec<u32>>);
    impl<S: PaintScene> ElementPainter<S> for Keeps {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            el.paint(scene)
        }
        fn renderer_suspended(&self) {
            self.0.borrow_mut().clear();
        }
    }
    let keeps = Keeps(RefCell::new(vec![7]));
    let painter: &dyn ElementPainter<NullScenePainter> = &keeps;
    painter.renderer_suspended();
    assert!(keeps.0.borrow().is_empty());

    let nothing: &dyn ElementPainter<NullScenePainter> = &DefaultPainter;
    nothing.renderer_suspended();
}
