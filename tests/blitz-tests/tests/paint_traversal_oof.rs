//! Traversal API contracts when paint ancestry differs from positioning ancestry.

use anyrender::recording::RenderCommand;
use anyrender::{NullScenePainter, PaintScene, Scene};
use blitz_dom::{DocumentConfig, local_name};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::{
    DefaultPainter, ElementPaint, ElementPainter, Piece, paint_elements, paint_scene_with,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{Affine, Rect, Shape};
use std::{cell::RefCell, sync::Arc};

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

#[test]
fn locate_follows_reparented_boxes_even_outside_the_view_and_clips() {
    struct Locate {
        expected: Vec<(&'static str, Rect)>,
        seen: RefCell<Vec<String>>,
    }
    impl ElementPainter<NullScenePainter> for Locate {
        fn paint_element(
            &self,
            el: &mut ElementPaint<'_, NullScenePainter>,
            scene: &mut NullScenePainter,
        ) {
            // On the root call, locate must find even children not yet visited and culled boxes.
            for (selector, expected) in &self.expected {
                let node = el.document().query_selector(selector).unwrap().unwrap();
                let (at, boxes) = el.locate(node).unwrap();
                assert_eq!(
                    at.transform_rect_bbox(boxes.border_box),
                    *expected,
                    "{selector}"
                );
            }
            // Every callback agrees with the independent, culling-off walk.
            assert_eq!(el.locate(el.node().id).unwrap().0, el.transform());
            if let Some(id) = el.node().attr(local_name!("id")) {
                self.seen.borrow_mut().push(id.to_string());
            }
            el.with_content_clip(scene, |el, scene| el.children(scene));
        }
    }
    for transformed_root in [false, true] {
        for z_index in ["auto", "-1", "1"] {
            let mut doc = document(&format!(
                r#"<style>
                html {{ {} }}
                body {{ margin:0; height:2000px }}
                #cb {{ position:relative; left:30px; top:40px; width:100px; height:100px }}
                #effect {{ opacity:.5; overflow:scroll; margin-left:60px; padding-top:30px;
                           width:20px; height:20px }}
                #tall {{ height:1000px }}
                #abs, #below, #fixed {{ position:absolute; left:10px; top:10px;
                                      width:20px; height:20px; z-index:{z_index} }}
                #below {{ top:500px }} #fixed {{ position:fixed }}
                </style><div id=cb><div id=effect><div id=tall></div>
                <div id=abs></div><div id=below></div><div id=fixed></div></div></div>"#,
                if transformed_root {
                    "transform:translate(7px, 9px)"
                } else {
                    ""
                },
            ));
            let effect = doc.query_selector("#effect").unwrap().unwrap();
            assert!(
                doc.get_node(effect)
                    .unwrap()
                    .flags
                    .has_reparented_oof_paint()
            );
            doc.scroll_node_by(effect, 0.0, -50.0, |_| {});
            doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 25.0 });
            for scale in [1.0, 2.0] {
                doc.set_viewport(Viewport::new(200, 200, scale as f32, ColorScheme::Light));
                doc.resolve(0.0);
                let (dx, dy) = if transformed_root {
                    (7.0, 9.0)
                } else {
                    (0.0, 0.0)
                };
                let place = |x: f64, y: f64| {
                    Affine::translate((11.0, 13.0)).transform_rect_bbox(Rect::new(
                        x * scale,
                        y * scale,
                        (x + 20.0) * scale,
                        (y + 20.0) * scale,
                    ))
                };
                let locate = Locate {
                    expected: vec![
                        ("#abs", place(40.0 + dx, 25.0 + dy)),
                        ("#below", place(40.0 + dx, 515.0 + dy)),
                        (
                            "#fixed",
                            place(
                                10.0 + dx,
                                10.0 + dy - if transformed_root { 25.0 } else { 0.0 },
                            ),
                        ),
                    ],
                    seen: RefCell::new(Vec::new()),
                };
                paint_elements(
                    &mut NullScenePainter,
                    &doc,
                    scale,
                    200,
                    200,
                    11,
                    13,
                    &locate,
                );
                let seen = locate.seen.borrow();
                assert!(seen.iter().any(|id| id == "abs"));
                assert!(!seen.iter().any(|id| id == "below"));
            }
        }
    }
}

fn depth(scene: &Scene) -> usize {
    let mut depth: usize = 0;
    for command in &scene.commands {
        match command {
            RenderCommand::PushLayer(_) | RenderCommand::PushClipLayer(_) => depth += 1,
            RenderCommand::PopLayer => depth = depth.checked_sub(1).expect("unmatched pop"),
            _ => {}
        }
    }
    depth
}

#[test]
fn hand_called_and_omitted_pieces_balance_their_deferred_clips() {
    struct ByHand(bool);
    impl ElementPainter<Scene> for ByHand {
        fn paint_element(&self, el: &mut ElementPaint<'_, Scene>, scene: &mut Scene) {
            let start = depth(scene);
            el.with_box_layers(scene, |el, scene| {
                for piece in [Piece::Outline, Piece::OuterShadow] {
                    let before = depth(scene);
                    el.piece(scene, piece);
                    assert_eq!(depth(scene), before);
                }
                el.with_effect_layers(scene, |el, scene| {
                    for piece in [
                        Piece::Background,
                        Piece::InsetShadow,
                        Piece::TableParts,
                        Piece::Border,
                    ] {
                        let before = depth(scene);
                        if !self.0 || piece == Piece::Background {
                            el.piece(scene, piece);
                        }
                        assert_eq!(depth(scene), before);
                        // App drawing between arbitrary pieces inherits no accidentally open clip.
                        let marker = Rect::new(0.0, 0.0, 1.0, 1.0);
                        scene.fill(
                            peniko::Fill::NonZero,
                            el.transform(),
                            peniko::Color::BLACK,
                            None,
                            &marker,
                        );
                    }
                    el.with_content_clip(scene, |el, scene| {
                        let before = depth(scene);
                        if !self.0 {
                            el.own_content(scene);
                            assert_eq!(depth(scene), before);
                        }
                        el.children(scene);
                        assert_eq!(depth(scene), before);
                    });
                    let before = depth(scene);
                    el.scrollbars(scene);
                    assert_eq!(depth(scene), before);
                });
            });
            assert_eq!(depth(scene), start);
        }
    }
    let doc = document(
        r#"<style>
        body { margin:0 } #cb { position:relative }
        #clip { overflow:hidden; width:30px; height:30px }
        #effect { opacity:.5; overflow:hidden; width:20px; height:20px; border:1px solid;
                  outline:1px solid; box-shadow:1px 1px 1px; background:red }
        #child { position:absolute; left:50px; top:50px; width:20px; height:20px; background:green }
        </style><div id=cb><div id=clip><div id=effect>text<div id=child></div>
        </div></div></div>"#,
    );
    for omit in [false, true] {
        let mut scene = Scene::new();
        paint_elements(&mut scene, &doc, 1.0, 200, 200, 0, 0, &ByHand(omit));
        assert_eq!(depth(&scene), 0);
        assert!(
            scene
                .commands
                .iter()
                .any(|cmd| matches!(cmd, RenderCommand::PushClipLayer(_)))
        );
    }
}

#[test]
fn read_only_paint_and_subdocuments_use_the_same_oof_walk() {
    let html = r#"<style>body { margin:0 } #effect { opacity:.5; overflow:hidden; width:20px; height:20px }
        #child { position:absolute; left:50px; top:50px; width:20px; height:20px; background:red }
        </style><div id=effect><div id=child></div></div>"#;
    let mut doc = document(html);
    let mut painted = Scene::new();
    paint_scene_with(&mut painted, &mut doc, 1.0, 200, 200, 0, 0, &DefaultPainter);
    let mut read_only = Scene::new();
    paint_elements(&mut read_only, &doc, 1.0, 200, 200, 0, 0, &DefaultPainter);
    assert_eq!(painted, read_only);
    let mut parent = document(
        "<body style='margin:0'><iframe style='border:0; width:200px; height:200px'></iframe>",
    );
    let iframe = parent.query_selector("iframe").unwrap().unwrap();
    parent.mutate().set_sub_document(iframe, Box::new(doc));
    parent.resolve(0.0);
    let mut scene = Scene::new();
    paint_scene_with(
        &mut scene,
        &mut parent,
        1.0,
        200,
        200,
        0,
        0,
        &DefaultPainter,
    );
    assert_eq!(depth(&scene), 0);
    // The sub-document's opacity layer includes the captured child at (50, 50).
    assert!(scene.commands.iter().any(|cmd| matches!(cmd,
        RenderCommand::PushLayer(layer) if layer.alpha == 0.5 && layer.clip.bounding_box().x1 >= 70.0
    )));
}

#[test]
fn replacing_own_content_keeps_its_deferred_clip() {
    let mut doc = document(
        r#"<style>
        body { margin:0; background:white }
        #effect { opacity:.5; overflow:hidden; width:20px; height:20px }
        #child { position:absolute; left:50px; top:50px; width:20px; height:20px; background:red }
        </style><div id=effect><div id=child></div></div>"#,
    );
    struct Replace;
    impl<S: PaintScene> ElementPainter<S> for Replace {
        fn paint_element(&self, el: &mut ElementPaint<'_, S>, scene: &mut S) {
            el.paint_with(scene, |el, scene, piece| {
                if piece == Piece::OwnContent && el.node().attr(local_name!("id")) == Some("effect")
                {
                    scene.fill(
                        peniko::Fill::NonZero,
                        el.transform(),
                        peniko::Color::from_rgb8(0, 255, 0),
                        None,
                        &Rect::new(0.0, 0.0, 100.0, 100.0),
                    );
                } else {
                    el.piece(scene, piece);
                }
            });
        }
    }
    let picture = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
        |scene| paint_scene_with(scene, &mut doc, 1.0, 200, 200, 0, 0, &Replace),
        200,
        200,
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 200 + x) * 4;
        &picture[i..i + 3]
    };
    assert_eq!(pixel(10, 10), [127, 255, 127]);
    assert_eq!(pixel(30, 30), [255, 255, 255]);
    assert_eq!(pixel(55, 55), [255, 127, 127]);
}
