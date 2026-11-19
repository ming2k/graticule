use std::collections::HashMap;
use std::hash::Hash;

use crate::layout::anchor::{AnchorTarget, AnchoredBox, compute_anchored_rect};
use crate::{Flex, FlexItem, Rect};

/// An instance token. Tokens are never reused, including after aborted frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub(super) u64);

/// Input ownership is independent from whether a node paints cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPolicy {
    #[default]
    /// The component neither reacts nor blocks input or the pointer.
    None,
    /// Receives unhandled keyboard events from descendants.
    Bubble,
    /// Owns declared event families without trapping focus or pointer input.
    /// Families are application-defined bits; the runtime interprets only
    /// their intersection with the dispatched event's family.
    Scope(u64),
    /// The highest painted scope blocks keyboard input to lower scopes.
    Modal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// How a node behaves under the pointer.
pub enum PointerPolicy {
    #[default]
    /// Clicks pass through to whatever is behind.
    Transparent,
    /// Clicks are delivered to this node and do not pass through.
    Target,
    /// Blocks every point in the viewport, including outside its own bounds.
    /// Descendants remain hittable; outside clicks target this barrier.
    Barrier,
}

/// Placement is separate from logical ownership. Viewport placement is the
/// explicit escape hatch for popups that outgrow their owner's clipping box.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum LayoutBox<K = ()> {
    #[default]
    /// Fill the parent's inner area.
    Fill,
    /// Absolute terminal coordinates, clipped by the parent's content box.
    Placed(Rect),
    /// Absolute terminal coordinates clipped only by the viewport.
    Viewport(Rect),
    /// Anchored to a target rectangle or scene node, clipped only by the viewport.
    Anchored(AnchoredBox<K>),
}

impl<K: Copy> Copy for LayoutBox<K> {}

/// A declaration, not the lifetime owner of a component's state.
#[derive(Clone, Debug)]
pub struct Component<K> {
    /// The instance's stable key.
    pub key: K,
    /// The owning parent's key, or `None` for a root.
    pub parent: Option<K>,
    /// How the node's box is derived.
    pub layout: LayoutBox<K>,
    /// Whether the node blocks or scopes input.
    pub input: InputPolicy,
    /// How the node behaves under the pointer.
    pub pointer: PointerPolicy,
    /// Whether the node can take focus.
    pub focusable: bool,
    /// Global paint layer; declaration order breaks ties.
    pub layer: i32,
    /// A changed revision invalidates painting, never measurement by itself.
    pub revision: u64,
}

impl<K> Component<K> {
    /// Declare a component under `parent`.
    pub fn new(key: K, parent: Option<K>, layout: LayoutBox<K>) -> Self {
        Self {
            key,
            parent,
            layout,
            input: InputPolicy::None,
            pointer: PointerPolicy::Transparent,
            focusable: false,
            layer: 0,
            revision: 0,
        }
    }

    /// Mark the component focusable and pointer-interactive.
    pub fn interactive(mut self) -> Self {
        self.pointer = PointerPolicy::Target;
        self.focusable = true;
        self.input = InputPolicy::Bubble;
        self
    }

    /// Mark the component as a modal foreground barrier.
    pub fn modal(mut self) -> Self {
        self.input = InputPolicy::Modal;
        self.pointer = PointerPolicy::Barrier;
        self.focusable = true;
        self
    }

    /// Claims the declared event families without trapping pointer input.
    /// Components the claim does not cover stay reachable below this one.
    pub fn scope(mut self, claims: u64) -> Self {
        self.input = InputPolicy::Scope(claims);
        self
    }

    /// Set the paint layer (higher paints later).
    pub fn layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// Set the revision that drives damage detection.
    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// A node's solved geometry.
pub struct NodeLayout {
    /// The node's own box.
    pub bounds: Rect,
    /// The region the node may paint into: its box intersected with its ancestors'.
    pub clip: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A scene/runtime invariant was violated.
pub enum UiError {
    /// Two components declared the same key or id.
    DuplicateKey,
    /// A component named a parent that is not in the scene.
    MissingParent,
    /// The requested key or id is not in the scene.
    MissingNode,
    /// An anchored component named a target that is not in the scene.
    MissingAnchor,
    /// A frame was queried without calling `begin` first.
    NoPendingFrame,
    /// The instance cannot take focus.
    InvalidFocus,
    /// The instance cannot capture the pointer.
    InvalidCapture,
    /// No further node ids are available.
    IdentityExhausted,
    /// A per-node state slot already holds a different type.
    StateTypeMismatch,
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateKey => f.write_str("duplicate component key in scene"),
            Self::MissingParent => {
                f.write_str("component specifies a parent key that is not in the scene")
            }
            Self::MissingNode => f.write_str("requested node was not found in the scene"),
            Self::MissingAnchor => {
                f.write_str("anchored component specifies a target node that is not in the scene")
            }
            Self::NoPendingFrame => {
                f.write_str("operation requires a pending frame (call begin() first)")
            }
            Self::InvalidFocus => {
                f.write_str("target node is not eligible to receive keyboard focus")
            }
            Self::InvalidCapture => f.write_str("target node is not eligible for pointer capture"),
            Self::IdentityExhausted => f.write_str("64-bit node id space exhausted"),
            Self::StateTypeMismatch => {
                f.write_str("node state slot is already occupied by a different type")
            }
        }
    }
}

impl std::error::Error for UiError {}

#[derive(Clone, Debug)]
pub(super) struct Node<K> {
    pub id: NodeId,
    pub component: Component<K>,
    pub layout: NodeLayout,
}

/// A solved frame. Construction requires parents before children, preventing
/// cycles by construction. Queries never inspect application state.
#[derive(Clone, Debug)]
pub struct Scene<K> {
    pub(super) viewport: Rect,
    pub(super) nodes: Vec<Node<K>>,
    pub(super) keys: HashMap<K, usize>,
    /// Node id -> index into `nodes`, so an id lookup is O(1) instead of a scan.
    pub(super) by_id: HashMap<NodeId, usize>,
    pub(super) order: Vec<usize>,
}

impl<K> Default for Scene<K> {
    fn default() -> Self {
        Self {
            viewport: Rect::default(),
            nodes: Vec::new(),
            keys: HashMap::new(),
            by_id: HashMap::new(),
            order: Vec::new(),
        }
    }
}

impl<K: Clone + Eq + Hash> Scene<K> {
    /// The frame's viewport.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The number of mounted nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the scene has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The id for a key.
    pub fn id(&self, key: &K) -> Option<NodeId> {
        self.node(key).map(|node| node.id)
    }

    /// The key for an id.
    pub fn key(&self, id: NodeId) -> Option<&K> {
        self.node_of(id).map(|node| &node.component.key)
    }

    /// The solved geometry for a key.
    pub fn layout(&self, key: &K) -> Option<NodeLayout> {
        self.node(key).map(|node| node.layout)
    }

    /// The solved bounds for a key.
    pub fn rect(&self, key: &K) -> Option<Rect> {
        self.layout(key).map(|layout| layout.bounds)
    }

    /// The declared component for a key.
    pub fn component(&self, key: &K) -> Option<&Component<K>> {
        self.node(key).map(|node| &node.component)
    }

    /// Nodes in paint order: `(key, layout)`, back to front.
    pub fn paint_order(&self) -> impl DoubleEndedIterator<Item = (&K, NodeLayout)> {
        self.order.iter().map(|&index| {
            let node = &self.nodes[index];
            (&node.component.key, node.layout)
        })
    }

    /// The topmost node under `(x, y)`, stopping at a pointer barrier.
    pub fn hit_test(&self, x: u16, y: u16) -> Option<&K> {
        if !self.viewport.contains(x, y) {
            return None;
        }
        for &index in self.order.iter().rev() {
            let node = &self.nodes[index];
            match node.component.pointer {
                PointerPolicy::Barrier => return Some(&node.component.key),
                PointerPolicy::Target if node.layout.clip.contains(x, y) => {
                    return Some(&node.component.key);
                }
                _ => {}
            }
        }
        None
    }

    /// The topmost modal barrier, if any.
    pub fn foreground(&self) -> Option<&K> {
        self.foreground_for(u64::MAX)
    }

    /// The topmost barrier within `family`.
    pub fn foreground_for(&self, family: u64) -> Option<&K> {
        self.order.iter().rev().find_map(|&index| {
            let node = &self.nodes[index];
            match node.component.input {
                InputPolicy::Modal => Some(&node.component.key),
                InputPolicy::Scope(claims) if claims & family != 0 => Some(&node.component.key),
                _ => None,
            }
        })
    }

    pub(super) fn focus_barrier(&self) -> Option<&K> {
        self.order.iter().rev().find_map(|&i| {
            let node = &self.nodes[i];
            (node.component.input == InputPolicy::Modal).then_some(&node.component.key)
        })
    }

    pub(super) fn pointer_barrier(&self) -> Option<&K> {
        self.order.iter().rev().find_map(|&i| {
            let node = &self.nodes[i];
            (node.component.pointer == PointerPolicy::Barrier).then_some(&node.component.key)
        })
    }

    /// Whether `child` is a descendant of `ancestor`.
    pub fn is_descendant(&self, child: &K, ancestor: &K) -> bool {
        let mut cursor = Some(child);
        while let Some(key) = cursor {
            if key == ancestor {
                return true;
            }
            cursor = self
                .node(key)
                .and_then(|node| node.component.parent.as_ref());
        }
        false
    }

    /// Keyboard ancestry terminates at the modal barrier. Unhandled does not
    /// mean that a modal permits delivery to a visually covered sibling.
    pub fn keyboard_path(&self, focused: Option<NodeId>) -> Vec<&K> {
        self.keyboard_path_for(focused, u64::MAX)
    }

    /// The keyboard delivery path from `focused` up to its `family` barrier.
    pub fn keyboard_path_for(&self, focused: Option<NodeId>, family: u64) -> Vec<&K> {
        let barrier = self.foreground_for(family);
        let target = focused
            .and_then(|id| self.key(id))
            .filter(|key| barrier.is_none_or(|scope| self.is_descendant(key, scope)))
            .or(barrier);
        let mut path = Vec::new();
        let mut cursor = target;
        while let Some(key) = cursor {
            let Some(node) = self.node(key) else { break };
            if node.component.input != InputPolicy::None {
                path.push(key);
            }
            if matches!(
                node.component.input,
                InputPolicy::Modal | InputPolicy::Scope(_)
            ) {
                break;
            }
            cursor = node.component.parent.as_ref();
        }
        path
    }

    /// Solve one container's children using the engine's integer flex solver.
    /// This operation performs no drawing and accepts content measurements
    /// supplied by leaf components.
    pub fn flex_children(
        &self,
        parent: &K,
        flex: &Flex,
        items: &[FlexItem],
        measure: &dyn Fn(usize, u16) -> u16,
    ) -> Result<Vec<Rect>, UiError> {
        let area = self.rect(parent).ok_or(UiError::MissingParent)?;
        let solved = flex.solve_with(area, items, measure);
        Ok((0..items.len()).map(|i| solved.rect(i)).collect())
    }

    pub(super) fn node(&self, key: &K) -> Option<&Node<K>> {
        self.keys.get(key).and_then(|&index| self.nodes.get(index))
    }

    pub(super) fn node_of(&self, id: NodeId) -> Option<&Node<K>> {
        self.by_id.get(&id).and_then(|&index| self.nodes.get(index))
    }

    /// Every node as `(id, component)`, in insertion order.
    pub(super) fn entries(&self) -> Vec<(NodeId, Component<K>)> {
        self.nodes
            .iter()
            .map(|node| (node.id, node.component.clone()))
            .collect()
    }

    /// Rebuild a scene from an explicit node list, recomputing geometry, clips,
    /// keys and paint order from the declarations. Nothing is mutated until the
    /// rebuild succeeds, which is what makes `UiRuntime::place` transactional.
    pub(super) fn rebuilt(
        viewport: Rect,
        nodes: Vec<(NodeId, Component<K>)>,
    ) -> Result<Scene<K>, UiError> {
        let mut scene = Scene {
            viewport,
            ..Scene::default()
        };
        for (id, component) in nodes {
            scene.push(id, component)?;
        }
        Ok(scene)
    }

    pub(super) fn push(
        &mut self,
        id: NodeId,
        component: Component<K>,
    ) -> Result<NodeLayout, UiError> {
        if self.keys.contains_key(&component.key) || self.by_id.contains_key(&id) {
            return Err(UiError::DuplicateKey);
        }
        let parent = match component.parent.as_ref() {
            Some(key) => self.layout(key).ok_or(UiError::MissingParent)?,
            None => NodeLayout {
                bounds: self.viewport,
                clip: self.viewport,
            },
        };
        let (bounds, clip) = match &component.layout {
            LayoutBox::Fill => (parent.bounds, parent.clip),
            LayoutBox::Placed(bounds) => (*bounds, parent.clip.intersection(*bounds)),
            LayoutBox::Viewport(bounds) => (*bounds, self.viewport.intersection(*bounds)),
            LayoutBox::Anchored(anchored) => {
                let target_rect = match &anchored.target {
                    AnchorTarget::Rect(r) => *r,
                    AnchorTarget::Node(k) => self.rect(k).ok_or(UiError::MissingAnchor)?,
                };
                let bounds = compute_anchored_rect(
                    target_rect,
                    self.viewport,
                    anchored.content_size,
                    anchored.placement,
                    anchored.alignment,
                    &anchored.constraints,
                );
                (bounds, self.viewport.intersection(bounds))
            }
        };
        let layout = NodeLayout { bounds, clip };
        let index = self.nodes.len();
        self.keys.insert(component.key.clone(), index);
        self.nodes.push(Node {
            id,
            component,
            layout,
        });
        self.by_id.insert(id, index);
        // Insert in paint order (layer, then mounting order) instead of sorting
        // the whole order list on every mount, which made n mounts O(n^2 log n).
        let slot = self.order.partition_point(|&i| {
            (self.nodes[i].component.layer, i) <= (self.nodes[index].component.layer, index)
        });
        self.order.insert(slot, index);
        Ok(layout)
    }
}

#[cfg(test)]
mod tests {
    use super::super::UiRuntime;
    use super::*;

    const FAMILY_SHEET: u64 = 1 << 0;
    const FAMILY_TRANSCRIPT: u64 = 1 << 1;
    const FAMILY_COMPOSER: u64 = 1 << 2;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Key {
        Transcript,
        Sheet,
        Modal,
        Composer,
    }

    fn build_scene(components: &[Component<Key>]) -> Scene<Key> {
        let mut runtime: UiRuntime<Key> = UiRuntime::default();
        runtime.begin(Rect::new(0, 0, 80, 24));
        for component in components {
            runtime.mount(component.clone()).unwrap();
        }
        runtime.commit().unwrap();
        runtime.presented().clone()
    }

    fn component(key: Key, layer: i32, input: InputPolicy) -> Component<Key> {
        let mut component = Component::new(key, None, LayoutBox::Fill).layer(layer);
        component.input = input;
        component
    }

    #[test]
    fn modal_is_foreground_for_every_family() {
        let scene = build_scene(&[
            component(Key::Transcript, 1, InputPolicy::Bubble),
            component(Key::Modal, 30, InputPolicy::Modal),
        ]);
        for family in [u64::MAX, FAMILY_TRANSCRIPT, FAMILY_COMPOSER, 0] {
            assert_eq!(scene.foreground_for(family), Some(&Key::Modal));
        }
    }

    #[test]
    fn scope_is_foreground_only_for_claimed_families() {
        let scene = build_scene(&[
            component(Key::Transcript, 1, InputPolicy::Scope(FAMILY_TRANSCRIPT)),
            component(Key::Sheet, 10, InputPolicy::Scope(FAMILY_SHEET)),
        ]);
        assert_eq!(scene.foreground_for(FAMILY_SHEET), Some(&Key::Sheet));
        assert_eq!(
            scene.foreground_for(FAMILY_TRANSCRIPT),
            Some(&Key::Transcript)
        );
        assert_eq!(scene.foreground_for(FAMILY_COMPOSER), None);
    }

    #[test]
    fn unclaimed_family_falls_through_to_lower_scope() {
        let scene = build_scene(&[
            component(Key::Composer, 3, InputPolicy::Scope(FAMILY_COMPOSER)),
            component(
                Key::Sheet,
                10,
                InputPolicy::Scope(FAMILY_SHEET | FAMILY_COMPOSER),
            ),
        ]);
        assert_eq!(scene.foreground_for(FAMILY_SHEET), Some(&Key::Sheet));
        assert_eq!(scene.foreground_for(FAMILY_COMPOSER), Some(&Key::Sheet));
        // The sheet does not claim the transcript family: a lower scope keeps it.
        let scene = build_scene(&[
            component(Key::Transcript, 1, InputPolicy::Scope(FAMILY_TRANSCRIPT)),
            component(Key::Sheet, 10, InputPolicy::Scope(FAMILY_SHEET)),
        ]);
        assert_eq!(
            scene.foreground_for(FAMILY_TRANSCRIPT),
            Some(&Key::Transcript)
        );
    }

    #[test]
    fn keyboard_path_terminates_at_the_family_barrier() {
        let scene = build_scene(&[
            component(Key::Transcript, 1, InputPolicy::Scope(FAMILY_TRANSCRIPT)),
            component(Key::Sheet, 10, InputPolicy::Scope(FAMILY_SHEET)),
        ]);
        let focused = scene.id(&Key::Transcript);
        assert_eq!(
            scene.keyboard_path_for(focused, FAMILY_SHEET),
            vec![&Key::Sheet]
        );
        assert_eq!(
            scene.keyboard_path_for(focused, FAMILY_TRANSCRIPT),
            vec![&Key::Transcript]
        );
    }

    #[test]
    fn focused_unclaimed_scope_yields_to_the_family_owner_below() {
        let scene = build_scene(&[
            component(Key::Transcript, 1, InputPolicy::Scope(FAMILY_TRANSCRIPT)),
            component(Key::Sheet, 10, InputPolicy::Scope(FAMILY_SHEET)),
        ]);
        let focused = scene.id(&Key::Sheet);
        assert_eq!(
            scene.keyboard_path_for(focused, FAMILY_TRANSCRIPT),
            vec![&Key::Transcript]
        );
    }

    #[test]
    fn layout_box_anchored_resolves_against_node_geometry() {
        use crate::layout::anchor::{
            AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, AnchoredBox,
        };

        let mut scene = Scene {
            viewport: Rect::new(0, 0, 100, 30),
            ..Default::default()
        };

        // Root
        let root = Component::new(
            Key::Transcript,
            None,
            LayoutBox::Placed(Rect::new(0, 0, 100, 30)),
        );
        scene.push(NodeId(1), root).unwrap();

        // Target row in list
        let row = Component::new(
            Key::Sheet,
            Some(Key::Transcript),
            LayoutBox::Placed(Rect::new(20, 10, 40, 2)),
        );
        scene.push(NodeId(2), row).unwrap();

        // Anchored popover targeting Key::Sheet
        let anchored = AnchoredBox {
            target: AnchorTarget::Node(Key::Sheet),
            placement: AnchorPlacement::Bottom,
            alignment: AnchorAlignment::Start,
            content_size: (30, 8),
            constraints: AnchorConstraints::default(),
        };
        let popover = Component::new(
            Key::Modal,
            Some(Key::Transcript),
            LayoutBox::Anchored(anchored),
        );
        let layout = scene.push(NodeId(3), popover).unwrap();

        assert_eq!(layout.bounds, Rect::new(20, 12, 30, 8));
        assert_eq!(layout.clip, Rect::new(20, 12, 30, 8));
    }

    #[test]
    fn layout_box_anchored_missing_anchor_returns_error() {
        use crate::layout::anchor::{
            AnchorAlignment, AnchorConstraints, AnchorPlacement, AnchorTarget, AnchoredBox,
        };

        let mut scene = Scene {
            viewport: Rect::new(0, 0, 100, 30),
            ..Default::default()
        };

        let anchored = AnchoredBox {
            target: AnchorTarget::Node(Key::Sheet),
            placement: AnchorPlacement::Bottom,
            alignment: AnchorAlignment::Start,
            content_size: (30, 8),
            constraints: AnchorConstraints::default(),
        };
        let popover = Component::new(Key::Modal, None, LayoutBox::Anchored(anchored));
        assert_eq!(
            scene.push(NodeId(1), popover).unwrap_err(),
            UiError::MissingAnchor
        );
    }
}
