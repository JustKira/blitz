//! Construction of the paint tree (`Node::paint_children` and
//! `Node::stacking_context`) from the laid-out box tree.
//!
//! The pass runs once per frame after layout and builds *topology* only: which
//! node ids each node paints itself and which z-indexed descendants are hoisted
//! to the nearest stacking-context root. Geometry (a hoisted box's offset from
//! its stacking-context root) is derived at use time from `final_layout()` and
//! memoised per geometry generation, so replaying a cached list for a clean
//! subtree can never paint or hit-test a box where layout no longer puts it.

use blitz_traits::node_id::NodeId;
use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use stylo_taffy::CopyStyleGetters;
use thin_vec::ThinVec;

use crate::layout::damage::ALL_DAMAGE;
use crate::tree::NodeTree;
use crate::{BaseDocument, Node};
use style::properties::generated::longhands::position::computed_value::T as Position;
use style::selector_parser::RestyleDamage;
use style::values::computed::Float;
use style::values::specified::box_::DisplayInside;
use taffy::{Point, Rect};

/// Offset of the hoisted child `child_id` from the border box of its
/// stacking-context root `sc_root_id`, excluding the child's own
/// `final_layout().location`: the sum of `location - scroll_offset` of every
/// box strictly between the two on the child's positioning chain. If the
/// painter is below the containing block, subtract its positioning-chain
/// offset instead: painting ownership does not change layout coordinates.
///
/// Walks `Node::containing_block` (the out-of-flow containing block for boxes
/// hoisted by Taffy, otherwise `layout_parent`, which is set for every layout
/// child by `resolve_layout_children`, anonymous blocks included).
pub fn hoisted_child_position(tree: &NodeTree, sc_root_id: NodeId, child_id: NodeId) -> Point<f32> {
    let child = &tree[child_id];
    let mut position = Point::ZERO;
    let mut current = child.containing_block();

    // A fixed-position box whose containing block is the root element is
    // positioned against the viewport and does not scroll with it: cancel the
    // scroll the walk below (or the stacking-context root itself) applies.
    // A fixed box contained by a transformed ancestor scrolls with it.
    if child.taffy_position() == taffy::Position::Fixed
        && let Some(cb) = current.and_then(|id| tree.get(id))
        && cb.is_viewport_containing_block()
    {
        let scroll_offset = *cb.scroll_offset();
        let viewport_scroll = tree.viewport_scroll();
        position.x += scroll_offset.x as f32 + viewport_scroll.x as f32;
        position.y += scroll_offset.y as f32 + viewport_scroll.y as f32;
    }

    while let Some(id) = current {
        if id == sc_root_id {
            return position;
        }
        let Some(node) = tree.get(id) else {
            break;
        };
        let location = node.final_layout().location;
        let scroll_offset = *node.scroll_offset();
        position.x += location.x - scroll_offset.x as f32;
        position.y += location.y - scroll_offset.y as f32;
        current = node.containing_block();
    }
    // The painter need not occur on the child's positioning chain (an
    // opacity context, for example, need not establish a containing block).
    // Both chains share the same root coordinate system.
    let mut current = Some(sc_root_id);
    while let Some(id) = current {
        let Some(node) = tree.get(id) else {
            break;
        };
        let location = node.final_layout().location;
        let scroll_offset = *node.scroll_offset();
        position.x -= location.x - scroll_offset.x as f32;
        position.y -= location.y - scroll_offset.y as f32;
        current = node.containing_block();
    }
    position
}

/// A child with a z_index that is hoisted up to it's containing Stacking Context for paint purposes
#[derive(Debug, Clone)]
pub struct HoistedPaintChild {
    pub node_id: NodeId,
    pub z_index: i32,
    /// `(geometry generation, offset from the stacking-context root)`, see
    /// [`Self::position`]. Generation `0` is never current.
    position: Cell<(u64, Point<f32>)>,
}

impl HoistedPaintChild {
    fn new(node_id: NodeId, z_index: i32) -> Self {
        Self {
            node_id,
            z_index,
            position: Cell::new((0, Point::ZERO)),
        }
    }

    /// The child's offset from the border box of its stacking-context root
    /// (`sc_root_id`), excluding the child's own `final_layout().location`.
    /// Computed at most once per geometry generation.
    pub fn position(&self, tree: &NodeTree, sc_root_id: NodeId) -> Point<f32> {
        let generation = tree.geometry_generation();
        let (stamp, cached) = self.position.get();
        if stamp == generation {
            return cached;
        }
        let position = hoisted_child_position(tree, sc_root_id, self.node_id);
        self.position.set((generation, position));
        position
    }

    /// Whether [`Self::position`] is memoised for the current geometry generation.
    pub fn position_is_cached(&self, tree: &NodeTree) -> bool {
        self.position.get().0 == tree.geometry_generation()
    }
}

#[derive(Debug)]
pub struct StackingContext {
    pub children: ThinVec<HoistedPaintChild>,
    /// The number of hoisted point children with negative z_index
    pub negative_z_count: u32,
    /// `(geometry generation, bounding box of the hoisted children)`, see
    /// [`Self::hoisted_content_bbox`].
    hoisted_content_bbox: Cell<(u64, Rect<f32>)>,
}

impl StackingContext {
    /// Build a sorted stacking context from the hoisted entries collected for it.
    fn from_children(children: ThinVec<HoistedPaintChild>) -> Self {
        let mut sc = Self {
            children,
            negative_z_count: 0,
            hoisted_content_bbox: Cell::new((0, Rect::ZERO)),
        };
        sc.sort();
        sc
    }

    /// Bounding box (in CSS pixels relative to the stacking-context root
    /// `sc_root_id`, unscrolled) of the scrollable overflow of all hoisted
    /// children, including their transforms. `scale` is the device pixel
    /// ratio `scrollable_overflow` is stored in. Computed at most once per
    /// geometry generation.
    pub fn hoisted_content_bbox(
        &self,
        tree: &NodeTree,
        sc_root_id: NodeId,
        scale: f64,
    ) -> Rect<f32> {
        let generation = tree.geometry_generation();
        let (stamp, cached) = self.hoisted_content_bbox.get();
        if stamp == generation {
            return cached;
        }

        let child_rect = |child: &HoistedPaintChild| -> Option<Rect<f32>> {
            let node = tree.get(child.node_id)?;
            let position = child.position(tree, sc_root_id);
            let location = node.final_layout().location;
            let mut overflow = node.scrolled_paint_bounds(scale);
            if let Some(transform) = node.transform().as_deref() {
                overflow = transform.transform_rect_bbox(overflow);
            }
            let left = position.x + location.x;
            let top = position.y + location.y;
            Some(Rect {
                left: left + (overflow.x0 / scale) as f32,
                top: top + (overflow.y0 / scale) as f32,
                right: left + (overflow.x1 / scale) as f32,
                bottom: top + (overflow.y1 / scale) as f32,
            })
        };

        // Children dropped by a DOM mutation since the last `resolve` yield
        // `None` and are left out, like hit testing skips them.
        let mut rects = self.children.iter().filter_map(child_rect);
        let mut area = rects.next().unwrap_or(Rect::ZERO);
        for pos in rects {
            area.left = area.left.min(pos.left);
            area.top = area.top.min(pos.top);
            area.right = area.right.max(pos.right);
            area.bottom = area.bottom.max(pos.bottom);
        }
        self.hoisted_content_bbox.set((generation, area));
        area
    }

    pub fn sort(&mut self) {
        self.children.sort_by_key(|c| c.z_index);
        self.negative_z_count = self.children.iter().take_while(|c| c.z_index < 0).count() as u32;
    }

    pub fn neg_z_range(&self) -> Range<usize> {
        0..(self.negative_z_count as usize)
    }

    pub fn pos_z_range(&self) -> Range<usize> {
        (self.negative_z_count as usize)..self.children.len()
    }

    pub fn neg_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.neg_z_range()].iter()
    }

    pub fn pos_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.pos_z_range()].iter()
    }
}

impl BaseDocument {
    /// Rebuild `paint_children` and `stacking_context` for the tree rooted at
    /// `root_id` from the laid-out box tree.
    ///
    /// Each node's layout children are placed either in its `paint_children`
    /// (sorted by paint order) or, when they have a non-zero `z-index` and are
    /// positioned or flex/grid items, in the nearest ancestor stacking context.
    ///
    /// Subtrees whose stored damage lacks `REBUILD_STACKING_CONTEXT` are skipped: their
    /// own lists are still valid and the entries they contributed to the
    /// enclosing stacking context are replayed from `Node::sc_contribution_cache`.
    pub(crate) fn build_paint_tree(&mut self, root_id: NodeId) {
        self.build_paint_tree_impl(root_id, None, &mut HashMap::new());
    }

    /// `stacking_context` is the entry list of the nearest ancestor stacking
    /// context, or `None` when `node_id` is itself a stacking-context root.
    fn build_paint_tree_impl(
        &mut self,
        node_id: NodeId,
        stacking_context: Option<&mut ThinVec<HoistedPaintChild>>,
        redirected: &mut HashMap<NodeId, Vec<NodeId>>,
    ) {
        {
            let node = &self.nodes[node_id];
            // `propagate_damage_flags` stores the union of a node's own and
            // its descendants' damage on the node, so the absence of
            // `REBUILD_STACKING_CONTEXT` (which every heavier level implies)
            // means nothing in the subtree can have changed a paint list.
            // Anonymous boxes are never skipped: damage marking walks the DOM
            // parent chain, which bypasses them.
            //
            // Whether a node is a stacking-context root also depends on its
            // parent (z-indexed flex/grid items), which does not damage the
            // node itself, so the role the caller resolved must match the one
            // this node was last built with.
            let role_unchanged = node.stacking_context.is_some() == stacking_context.is_none();
            let clean = role_unchanged
                && !node
                    .damage()
                    .unwrap_or(ALL_DAMAGE)
                    .contains(RestyleDamage::REBUILD_STACKING_CONTEXT)
                && !node.is_anonymous();
            if clean {
                if let Some(stacking_context) = stacking_context {
                    stacking_context.extend(node.sc_contribution_cache.borrow().iter().cloned());
                }
                return;
            }
        }

        let Some(display) = self.nodes[node_id].display_style() else {
            return;
        };

        // Entries hoisted out of this subtree are collected directly into the
        // storage that will hold them afterwards: the node's own stacking
        // context for a root, otherwise its contribution cache (from which
        // they are copied into the enclosing stacking context below).
        let mut entries = {
            let node = &mut self.nodes[node_id];
            let mut entries = match stacking_context {
                None => node
                    .stacking_context
                    .take()
                    .map(|sc| sc.children)
                    .unwrap_or_default(),
                Some(_) => std::mem::take(&mut *node.sc_contribution_cache.borrow_mut()),
            };
            entries.clear();
            entries
        };

        let children = self.nodes[node_id].layout_children.borrow_mut().take();
        let mut has_reparented_oof = false;
        if let Some(children) = children {
            let is_flex_or_grid =
                matches!(display.inside(), DisplayInside::Flex | DisplayInside::Grid);

            // Layout ownership stays with the containing block. Paint
            // ownership stops at any intervening stacking context instead.
            // Route those boxes before descending, so that the context builds
            // and caches its own lists normally. Clean subtrees can still skip
            // the walk: their ownership and cached lists are unchanged.
            let owned_here = |doc: &Self, child_id: NodeId| -> bool {
                doc.nodes[child_id]
                    .oof_containing_block
                    .get()
                    .is_none_or(|cb| cb == node_id)
            };
            // Boxes hoisted to this node from further down the tree, keyed by
            // the index of the layout child they descend from: they are visited
            // and listed in tree order, directly after that child (CSS 2.1
            // Appendix E). Boxes whose layout-parent chain does not lead through
            // a layout child of this node come after all of them.
            let hoisted: Vec<NodeId> = self.nodes[node_id]
                .hoisted_children
                .borrow()
                .iter()
                .copied()
                .filter(|&id| {
                    self.nodes
                        .get(id)
                        .is_some_and(|n| n.layout_parent.get() != Some(node_id))
                })
                .collect();
            let mut owned_hoisted = redirected.remove(&node_id).unwrap_or_default();
            has_reparented_oof = !owned_hoisted.is_empty();
            for id in hoisted {
                let owner = self.oof_paint_owner(node_id, id);
                if owner == node_id {
                    owned_hoisted.push(id);
                } else {
                    redirected.entry(owner).or_default().push(id);
                }
            }
            let mut hoisted_past_parent: Vec<(usize, NodeId)> = owned_hoisted
                .into_iter()
                .map(|id| {
                    let anchor = self
                        .layout_child_containing(node_id, id)
                        .and_then(|anchor| children.iter().position(|&c| c == anchor))
                        .unwrap_or(children.len());
                    (anchor, id)
                })
                .collect();
            // Different containing blocks (e.g. absolute and fixed siblings)
            // can redirect into the same context. Restore box-tree order when
            // their entries share an anchor.
            hoisted_past_parent.sort_by_cached_key(|&(anchor, id)| {
                let mut path = Vec::new();
                let mut current = id;
                while let Some(parent) = self.nodes[current].layout_parent.get() {
                    if parent == node_id {
                        break;
                    }
                    path.push(
                        self.nodes[parent]
                            .layout_children
                            .borrow()
                            .iter()
                            .flatten()
                            .position(|&id| id == current)
                            .unwrap_or(usize::MAX),
                    );
                    current = parent;
                }
                path.reverse();
                (anchor, path)
            });
            let mut next_hoisted = 0;

            let is_layer_root =
                stacking_context.is_none() || self.nodes[node_id].is_paint_layer_root();
            let keyed_base = self.scratch.keyed_children.len();

            for (index, &child_id) in children.iter().enumerate() {
                if owned_here(self, child_id) {
                    let is_sc_root = self.nodes[child_id].is_stacking_context_root(is_flex_or_grid);
                    let first_entry = entries.len();
                    self.build_paint_tree_impl(
                        child_id,
                        match is_sc_root {
                            true => None,
                            false => Some(&mut entries),
                        },
                        redirected,
                    );

                    // Only stacking contexts with a non-zero z-index are lifted
                    // out of tree order into the enclosing stacking context's
                    // z-sorted list (CSS 2.1 Appendix E).
                    let child = &self.nodes[child_id];
                    let z_index = child.z_index();
                    let paint_order = node_to_paint_order(child, is_flex_or_grid);
                    if is_sc_root && z_index != 0 {
                        entries.push(HoistedPaintChild::new(child_id, z_index))
                    } else if !is_layer_root && child.is_in_flow_positioned() {
                        // Positioned boxes paint above all in-flow content of
                        // the nearest positioned (or clipping) ancestor, not just
                        // of their parent: lift them out of this static box.
                        entries.push(HoistedPaintChild::new(child_id, 0))
                    } else {
                        self.scratch.keyed_children.push((paint_order, child_id));
                    }

                    // Positioned descendants lifted out of the static boxes of
                    // this child's subtree join this node's positioned paint
                    // level, in tree order after the child.
                    if is_layer_root && entries.len() > first_entry {
                        let subtree_entries: ThinVec<_> = entries.drain(first_entry..).collect();
                        for entry in subtree_entries {
                            if entry.z_index == 0 {
                                self.scratch
                                    .keyed_children
                                    .push(((2, paint_order.1), entry.node_id));
                            } else {
                                entries.push(entry);
                            }
                        }
                    }
                }

                while let Some(&(anchor, hoisted_id)) = hoisted_past_parent.get(next_hoisted)
                    && anchor == index
                {
                    self.attach_hoisted_past_parent(
                        hoisted_id,
                        is_flex_or_grid,
                        &mut entries,
                        redirected,
                    );
                    next_hoisted += 1;
                }
            }
            for &(_, hoisted_id) in &hoisted_past_parent[next_hoisted..] {
                self.attach_hoisted_past_parent(
                    hoisted_id,
                    is_flex_or_grid,
                    &mut entries,
                    redirected,
                );
            }

            self.scratch.keyed_children[keyed_base..].sort_by_key(|&(paint_order, _)| paint_order);

            let mut paint_children = self.nodes[node_id]
                .paint_children
                .borrow_mut()
                .take()
                .unwrap_or_default();
            paint_children.clear();
            paint_children.extend(
                self.scratch
                    .keyed_children
                    .drain(keyed_base..)
                    .map(|(_, id)| id),
            );

            let node = &self.nodes[node_id];
            *node.paint_children.borrow_mut() = Some(paint_children);
            *node.layout_children.borrow_mut() = Some(children);
        }

        // Store the subtree fact with the constructed topology, including on
        // clean-cache replay, rather than rediscovering it during painting.
        has_reparented_oof |= self.nodes[node_id]
            .paint_children
            .borrow()
            .iter()
            .flatten()
            .copied()
            .chain(entries.iter().map(|entry| entry.node_id))
            .any(|id| self.nodes[id].flags.has_reparented_oof_paint());
        let node = &mut self.nodes[node_id];
        node.flags.set(
            crate::node::NodeFlags::HAS_REPARENTED_OOF_PAINT,
            has_reparented_oof,
        );
        if let Some(stacking_context) = stacking_context {
            node.stacking_context = None;
            stacking_context.extend(entries.iter().cloned());
            *node.sc_contribution_cache.borrow_mut() = entries;
        } else {
            node.sc_contribution_cache.borrow_mut().clear();
            node.stacking_context = Some(Box::new(StackingContext::from_children(entries)));
        }
    }

    /// The layout child of `node_id` whose subtree contains `id`, following
    /// `layout_parent`.
    fn layout_child_containing(&self, node_id: NodeId, id: NodeId) -> Option<NodeId> {
        let mut current = id;
        loop {
            let parent = self.nodes.get(current)?.layout_parent.get()?;
            if parent == node_id {
                return Some(current);
            }
            current = parent;
        }
    }

    /// Nearest stacking context strictly between an out-of-flow box and its
    /// containing block on the box tree, or the containing block as before.
    fn oof_paint_owner(&self, containing_block: NodeId, child_id: NodeId) -> NodeId {
        let mut current = self.nodes[child_id].layout_parent.get();
        while let Some(id) = current {
            if id == containing_block {
                break;
            }
            let node = &self.nodes[id];
            let parent = node.layout_parent.get();
            let is_flex_or_grid_item = parent
                .and_then(|id| self.nodes[id].display_style())
                .is_some_and(|d| matches!(d.inside(), DisplayInside::Flex | DisplayInside::Grid));
            if node.is_stacking_context_root(is_flex_or_grid_item) {
                return id;
            }
            current = parent;
        }
        containing_block
    }

    /// Visit an out-of-flow box hoisted to its containing block past its
    /// layout parent and place it in the paint lists: z-indexed boxes go to
    /// the enclosing stacking context like any other z-indexed positioned
    /// child, the rest join `paint_children` at the same paint level as other
    /// positioned boxes (the caller's stable sort keeps tree order).
    fn attach_hoisted_past_parent(
        &mut self,
        child_id: NodeId,
        is_flex_or_grid: bool,
        entries: &mut ThinVec<HoistedPaintChild>,
        redirected: &mut HashMap<NodeId, Vec<NodeId>>,
    ) {
        self.build_paint_tree_impl(
            child_id,
            match self.nodes[child_id].is_stacking_context_root(false) {
                true => None,
                false => Some(entries),
            },
            redirected,
        );

        let z_index = self.nodes[child_id].z_index();
        if z_index != 0 {
            entries.push(HoistedPaintChild::new(child_id, z_index));
        } else {
            let paint_order = node_to_paint_order(&self.nodes[child_id], is_flex_or_grid);
            self.scratch.keyed_children.push((paint_order, child_id));
        }
    }
}

impl Node {
    fn is_viewport_containing_block(&self) -> bool {
        use style::values::computed::{Contain, ContainerType};
        use style::values::specified::box_::WillChangeBits;

        self.containing_block()
            .is_none_or(|id| matches!(self.tree()[id].data, crate::node::NodeData::Document(_)))
            && !self.primary_styles().is_some_and(|style| {
                // Unlike transforms and containment, filters (and will-change
                // for filters) do not establish a fixed containing block on
                // the document root. Do not use the non-root claims here.
                let box_style = style.get_box();
                box_style.has_transform_or_perspective()
                    || box_style.will_change.bits.intersects(
                        WillChangeBits::TRANSFORM
                            | WillChangeBits::PERSPECTIVE
                            | WillChangeBits::CONTAIN,
                    )
                    || box_style
                        .contain
                        .intersects(Contain::LAYOUT | Contain::PAINT)
                    || box_style
                        .container_type
                        .intersects(ContainerType::SIZE | ContainerType::INLINE_SIZE)
            })
    }

    /// Whether this node paints its positioned descendants itself (CSS 2.1
    /// Appendix E step 8) rather than leaving them to an ancestor: positioned
    /// boxes, and boxes that clip their contents. Stacking-context roots are
    /// handled by the caller.
    fn is_paint_layer_root(&self) -> bool {
        use style::values::computed::{Contain, Overflow};

        let Some(style) = self.primary_styles() else {
            return true;
        };
        let box_style = style.get_box();
        style.clone_position() != Position::Static
            || !matches!(box_style.overflow_x, Overflow::Visible)
            || !matches!(box_style.overflow_y, Overflow::Visible)
            || box_style.slow_clone_contain().contains(Contain::PAINT)
            || self.element_data().is_some_and(|e| {
                e.raster_image_data().is_some()
                    || e.sub_doc_data().is_some()
                    || e.text_input_data().is_some()
            })
    }

    /// Whether this is a `position: relative` or `sticky` box.
    fn is_in_flow_positioned(&self) -> bool {
        self.primary_styles().is_some_and(|style| {
            matches!(
                style.clone_position(),
                Position::Relative | Position::Sticky
            )
        })
    }

    /// The offset from the border box of `painter_id` at which this node is
    /// painted, excluding its own `final_layout().location`, when it is in the
    /// paint list of `painter_id` without being laid out relative to it (a
    /// positioned box lifted past static ancestors).
    #[inline]
    pub fn lifted_paint_offset(&self, painter_id: NodeId) -> Option<Point<f32>> {
        match self.containing_block() {
            // Most paint children are ordinary layout children. Avoid reading
            // computed positioning styles for them on every paint and hit.
            Some(id)
                if id == painter_id
                    && !self.out_of_flow.load(std::sync::atomic::Ordering::Relaxed) =>
            {
                None
            }
            Some(id)
                if self.taffy_position() == taffy::Position::Fixed
                    && self.tree()[id].is_viewport_containing_block() =>
            {
                Some(hoisted_child_position(self.tree(), painter_id, self.id))
            }
            Some(id) if id == painter_id => None,
            None => None,
            Some(_) => Some(hoisted_child_position(self.tree(), painter_id, self.id)),
        }
    }

    /// Conservative paint bounds in scaled, unscrolled local coordinates.
    /// Layout overflow follows containing blocks, so it alone cannot bound
    /// boxes painted by an intervening stacking context. Cache the union over
    /// the paint tree separately; do not change layout's overflow or clipping.
    #[inline]
    pub fn paint_bounds(&self, scale: f64) -> kurbo::Rect {
        // Ordinary subtrees retain layout's bounds without walking paint lists
        // or populating a cache that would be invalidated by every scroll.
        if !self.flags.has_reparented_oof_paint() {
            return *self.scrollable_overflow();
        }
        self.reparented_paint_bounds(scale)
    }

    fn reparented_paint_bounds(&self, scale: f64) -> kurbo::Rect {
        let generation = self.tree().geometry_generation();
        {
            let mut cache = self.tree().paint_bounds_cache.borrow_mut();
            if cache.0 != generation {
                cache.0 = generation;
                cache.1.clear();
            }
            if let Some(bounds) = cache.1.get(&self.id) {
                return *bounds;
            }
        }
        let mut bounds = *self.scrollable_overflow();
        if let Some(sc) = &self.stacking_context {
            let bbox = sc.hoisted_content_bbox(self.tree(), self.id, scale);
            bounds = bounds.union(kurbo::Rect::new(
                bbox.left as f64 * scale,
                bbox.top as f64 * scale,
                bbox.right as f64 * scale,
                bbox.bottom as f64 * scale,
            ));
        }
        for id in self.paint_children.borrow().iter().flatten() {
            let Some(child) = self.tree().get(*id) else {
                continue;
            };
            let mut child_bounds = child.scrolled_paint_bounds(scale);
            if let Some(transform) = child.transform().as_deref() {
                child_bounds = transform.transform_rect_bbox(child_bounds);
            }
            let offset = child.lifted_paint_offset(self.id).unwrap_or(Point::ZERO);
            let location = child.final_layout().location;
            bounds = bounds.union(
                child_bounds
                    + kurbo::Vec2::new(
                        (location.x + offset.x) as f64 * scale,
                        (location.y + offset.y) as f64 * scale,
                    ),
            );
        }
        self.tree()
            .paint_bounds_cache
            .borrow_mut()
            .1
            .insert(self.id, bounds);
        bounds
    }

    /// Bounds for culling and effect clips, before this box's scroll transform
    /// is applied. Captured descendants' offsets compensate for scrolling, so
    /// convert their content bounds back to border-box coordinates here (and
    /// when including a child's bounds in its painter's content bounds).
    #[inline]
    pub fn scrolled_paint_bounds(&self, scale: f64) -> kurbo::Rect {
        if !self.flags.has_reparented_oof_paint() {
            return *self.scrollable_overflow();
        }
        self.reparented_paint_bounds(scale)
            - kurbo::Vec2::new(self.scroll_offset().x, self.scroll_offset().y) * scale
    }
}

#[inline(always)]
fn position_to_order(pos: Position) -> i32 {
    match pos {
        Position::Static => 0,
        // All positioned descendants with z-index: auto share one paint
        // level (CSS 2.1 Appendix E step 8); the stable sort keeps them in
        // tree order among themselves, above in-flow content and floats.
        Position::Relative | Position::Sticky | Position::Absolute | Position::Fixed => 2,
    }
}
#[inline(always)]
fn float_to_order(pos: Float) -> i32 {
    match pos {
        Float::None => 0,
        _ => 1,
    }
}

/// Paint sort key: (paint level, order-modified position). Positioned
/// (z-index: auto) descendants paint above in-flow content (CSS 2.1
/// Appendix E step 8); within a level the stable sort preserves
/// (order-modified) document order.
#[inline(always)]
fn node_to_paint_order(node: &Node, is_flex_or_grid: bool) -> (i32, i32) {
    let Some(style) = node.primary_styles() else {
        return (0, 0);
    };
    let position = style.clone_position();
    if is_flex_or_grid {
        match position {
            Position::Static => (0, style.clone_order()),
            Position::Relative | Position::Sticky => (2, style.clone_order()),
            // Out-of-flow children are not flex/grid items: `order` does
            // not apply; tree order does.
            Position::Absolute | Position::Fixed => (2, 0),
        }
    } else {
        (
            position_to_order(position) + float_to_order(style.clone_float()),
            0,
        )
    }
}
