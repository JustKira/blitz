//! Hit testing content that an ancestor clips away.
//!
//! A box with `overflow` other than `visible` paints its contents only inside
//! its padding box. The hit test went on into the children wherever their
//! overflowing boxes reached, so a click below a short `overflow: hidden` box
//! landed on its clipped child and not on the element painted there.

use blitz_test_harness::{Harness, HarnessOptions};

fn harness(overflow: &str) -> Harness {
    Harness::from_html_with(
        &format!(
            "<html><head><style>\
             body {{ margin: 0 }}\
             #clip {{ height: 20px; width: 100px; border: 4px solid; overflow: {overflow} }}\
             #tall {{ height: 200px }}\
             #below {{ margin-top: 100px; width: 100px; height: 30px }}\
             #clip {{ margin-top: -130px }}\
             </style></head><body id=body>\
             <div id=below></div><div id=clip><div id=tall></div></div>\
             </body></html>"
        ),
        HarnessOptions {
            width: 600,
            height: 400,
            ..Default::default()
        },
    )
}

fn hit_id(harness: &Harness, x: f32, y: f32) -> String {
    let node_id = harness.hit_node(x, y);
    let doc = harness.base();
    doc.get_node(node_id)
        .and_then(|n| n.data.downcast_element())
        .and_then(|el| el.id.as_ref().map(|id| id.to_string()))
        .unwrap_or_default()
}

#[test]
fn content_clipped_away_is_not_hit() {
    for overflow in ["hidden", "clip", "auto", "scroll"] {
        let harness = harness(overflow);
        // inside the clip: the child
        assert_eq!(hit_id(&harness, 50.0, 15.0), "tall", "{overflow}");
        // on the clipping box's own border: the box
        assert_eq!(hit_id(&harness, 50.0, 26.0), "clip", "{overflow}");
        // below it, where the child would overflow to: what is painted there
        assert_eq!(hit_id(&harness, 50.0, 115.0), "below", "{overflow}");
        assert_ne!(hit_id(&harness, 50.0, 60.0), "tall", "{overflow}");
    }
}

#[test]
fn content_that_overflows_visibly_is_hit() {
    let harness = harness("visible");
    assert_eq!(hit_id(&harness, 50.0, 60.0), "tall");
}
