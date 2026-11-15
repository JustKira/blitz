//! Out-of-flow positioning follows the containing block, but painting follows
//! the nearest stacking context on the layout-parent chain.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const SIZE: u32 = 200;

fn document(position: &str, parent: &str, child: &str, extra_css: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><head><style>
        body {{ margin:0; background:white; height:1000px }}
        #cb {{ position:relative; left:30px; top:40px; width:100px; height:100px }}
        #parent {{ margin-left:60px; padding-top:30px; width:20px; height:20px; {parent} }}
        #child {{ position:{position}; left:10px; top:10px; width:20px; height:20px;
                  background:red; {child} }}
        #control {{ position:absolute; left:150px; top:150px; width:20px; height:20px; background:red }}
        {extra_css}
        </style></head><body><div id=cb><div id=parent><div id=middle><div id=child></div></div></div></div>
        <div><div id=control></div></div></body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(SIZE, SIZE, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            incremental: Some(true),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn render(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, SIZE, SIZE, 0, 0),
        SIZE,
        SIZE,
    )
}

fn pixel(buffer: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * SIZE as usize + x) * 4;
    [buffer[i], buffer[i + 1], buffer[i + 2]]
}

fn assert_child(doc: &mut HtmlDocument, position: &str, expected: [u8; 3]) {
    let (x, y) = if position == "fixed" {
        (15, 15)
    } else {
        (45, 55)
    };
    let buffer = render(doc);
    assert_eq!(pixel(&buffer, x, y), expected, "{position}");
    // Neither the new painter's margin nor its padding changes positioning.
    assert_eq!(pixel(&buffer, x - 10, y), [255; 3]);
    assert_eq!(pixel(&buffer, x + 20, y), [255; 3]);
    let child = doc.query_selector("#child").unwrap().unwrap();
    assert_eq!(doc.hit(x as f32, y as f32).unwrap().node_id, child);
    // The old containing-block ownership remains correct without an
    // intervening stacking context.
    assert_eq!(pixel(&buffer, 155, 155), [255, 0, 0]);
    let control = doc.query_selector("#control").unwrap().unwrap();
    assert_eq!(doc.hit(155.0, 155.0).unwrap().node_id, control);
}

#[test]
fn absolute_child_in_half_opacity_parent() {
    for (parent, css) in [
        ("opacity:0.5", ""),
        ("opacity:0.5; overflow:hidden", ""),
        ("overflow:hidden", "#middle { opacity:0.5 }"),
        ("opacity:0.5", "#middle { overflow:hidden; height:0 }"),
    ] {
        let mut doc = document("absolute", parent, "", css);
        assert_child(&mut doc, "absolute", [255, 127, 127]);
    }
}

#[test]
fn fixed_child_in_half_opacity_parent() {
    let mut doc = document("fixed", "opacity:0.5", "", "");
    assert_child(&mut doc, "fixed", [255, 127, 127]);
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 25.0 });
    let buffer = render(&mut doc);
    assert_eq!(pixel(&buffer, 15, 15), [255, 127, 127]);
    let child = doc.query_selector("#child").unwrap().unwrap();
    // hit() takes document coordinates; paint_scene applies viewport scroll.
    assert_eq!(doc.hit(15.0, 40.0).unwrap().node_id, child);
}

#[test]
fn absolute_child_in_zero_opacity_parent() {
    let mut doc = document("absolute", "opacity:0", "", "");
    assert_child(&mut doc, "absolute", [255; 3]);
}

#[test]
fn fixed_child_in_zero_opacity_parent() {
    let mut doc = document("fixed", "opacity:0", "", "");
    assert_child(&mut doc, "fixed", [255; 3]);
}

#[test]
fn z_indexed_child_stays_in_opacity_context() {
    for position in ["absolute", "fixed"] {
        for z in [-1, 1] {
            let mut doc = document(position, "opacity:0.5", &format!("z-index:{z}"), "");
            assert_child(&mut doc, position, [255, 127, 127]);
        }
    }
}

#[test]
fn opacity_change_rebuilds_oof_ownership_after_paint() {
    for position in ["absolute", "fixed"] {
        for z in [0, 1] {
            let mut doc = document(
                position,
                "opacity:1",
                &format!("z-index:{z}"),
                "#parent:hover { opacity:0.5 }",
            );
            assert_child(&mut doc, position, [255, 0, 0]);
            for _ in 0..2 {
                assert!(doc.set_hover_to(95.0, 45.0));
                doc.resolve(0.0);
                assert_child(&mut doc, position, [255, 127, 127]);
                // Replaying clean subtree contributions must preserve ownership.
                doc.resolve(0.0);
                assert_child(&mut doc, position, [255, 127, 127]);
                assert!(doc.set_hover_to(190.0, 190.0));
                doc.resolve(0.0);
                assert_child(&mut doc, position, [255, 0, 0]);
            }
        }
    }
}

#[test]
fn nested_contexts_do_not_cull_a_child_outside_their_layout_overflow() {
    let mut doc = document(
        "absolute",
        "opacity:0.5; margin-left:400px",
        "z-index:1",
        "#middle { opacity:0.5; margin-left:50px }",
    );
    assert_child(&mut doc, "absolute", [255, 191, 191]);
}

#[test]
fn other_stacking_context_roots_capture_oof_children() {
    for style in [
        "isolation:isolate",
        "mix-blend-mode:multiply",
        "clip-path:inset(-100px)",
    ] {
        let mut doc = document("absolute", style, "z-index:1", "");
        let parent = doc.query_selector("#parent").unwrap().unwrap();
        let child = doc.query_selector("#child").unwrap().unwrap();
        assert!(
            doc.get_node(parent)
                .unwrap()
                .stacking_context
                .as_ref()
                .unwrap()
                .children
                .iter()
                .any(|entry| entry.node_id == child),
            "{style}"
        );
        assert_child(&mut doc, "absolute", [255, 0, 0]);
    }
    let mut doc = document("fixed", "position:relative; z-index:1", "z-index:1", "");
    let parent = doc.query_selector("#parent").unwrap().unwrap();
    let child = doc.query_selector("#child").unwrap().unwrap();
    assert!(
        doc.get_node(parent)
            .unwrap()
            .stacking_context
            .as_ref()
            .unwrap()
            .children
            .iter()
            .any(|entry| entry.node_id == child)
    );
    assert_child(&mut doc, "fixed", [255, 0, 0]);

    // An atomic clip still encloses the captured child, unlike an overflow
    // clip whose containing block is outside the clipping ancestor.
    let mut clipped = document("absolute", "opacity:0.5; clip-path:inset(0)", "", "");
    assert_eq!(pixel(&render(&mut clipped), 45, 55), [255; 3]);
}
