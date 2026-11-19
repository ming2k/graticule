use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::hash::Hash;

use crate::render::paint::Paint;
use crate::{Frame, Rect};

use super::scene::{Component, NodeId, NodeLayout, PointerPolicy, Scene, UiError};

#[derive(Clone, Debug, PartialEq, Eq)]
/// What happened to a mounted instance during [`UiRuntime::commit`].
pub enum Lifecycle<K> {
    /// A component was mounted this frame.
    Mounted {
        /// The mounted instance's key.
        key: K,
        /// The instance's stable id.
        id: NodeId,
    },
    /// A component disappeared and its local state was disposed.
    Unmounted {
        /// The unmounted instance's key.
        key: K,
        /// The instance's stable id.
        id: NodeId,
    },
    /// Focus moved, including to or from `None`.
    FocusChanged {
        /// The previously focused instance, if any.
        previous: Option<NodeId>,
        /// The newly focused instance, if any.
        current: Option<NodeId>,
    },
    /// Pointer capture was released because its holder is no longer eligible.
    CaptureReleased {
        /// The instance that lost capture.
        id: NodeId,
    },
}

/// Owns mounted instances and frame transactions. Scene declarations carry no
/// application vocabulary and are never the storage for business models.
pub struct UiRuntime<K> {
    presented: Scene<K>,
    pending: Option<Scene<K>>,
    next_id: u64,
    focused: Option<NodeId>,
    capture: Option<NodeId>,
    focus_history: Vec<NodeId>,
    state: HashMap<(NodeId, TypeId), Box<dyn Any + Send>>,
    invalidated: Vec<K>,
}

impl<K> Default for UiRuntime<K> {
    fn default() -> Self {
        Self {
            presented: Scene::default(),
            pending: None,
            next_id: 0,
            focused: None,
            capture: None,
            focus_history: Vec::new(),
            state: HashMap::new(),
            invalidated: Vec::new(),
        }
    }
}

impl<K: Clone + Eq + Hash> UiRuntime<K> {
    /// The committed scene: the only one queries and the terminal see.
    pub fn presented(&self) -> &Scene<K> {
        &self.presented
    }

    /// The scene being built this frame, or `NoPendingFrame`.
    pub fn pending(&self) -> Result<&Scene<K>, UiError> {
        self.pending.as_ref().ok_or(UiError::NoPendingFrame)
    }

    /// Replaces an uncommitted measurement pass. It cannot unmount instances
    /// or alter the interaction snapshot that the user is currently seeing.
    pub fn begin(&mut self, viewport: Rect) {
        self.pending = Some(Scene {
            viewport,
            ..Scene::default()
        });
    }

    /// Declare a component. Reuse is by key, so identity survives reordering.
    pub fn mount(&mut self, component: Component<K>) -> Result<NodeLayout, UiError> {
        let pending = self.pending.as_mut().ok_or(UiError::NoPendingFrame)?;
        // A stable key denotes the instance, independent of sibling position.
        let id = if let Some(id) = self.presented.id(&component.key) {
            id
        } else {
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or(UiError::IdentityExhausted)?;
            NodeId(self.next_id)
        };
        pending.push(id, component)
    }

    /// Discard the pending frame, publishing nothing.
    pub fn abort(&mut self) {
        self.pending = None;
    }

    /// Refine a measured allocation before presentation. Descendant clips are
    /// recomputed from declarations, never adjusted from stale screen boxes.
    pub fn place(&mut self, key: &K, layout: super::LayoutBox<K>) -> Result<NodeLayout, UiError> {
        let scene = self.pending.as_ref().ok_or(UiError::NoPendingFrame)?;
        let index = *scene.keys.get(key).ok_or(UiError::MissingNode)?;
        // Rebuild off to the side: if anything fails, the pending scene is
        // untouched, so a failed refinement cannot leave a truncated frame.
        let mut nodes = scene.entries();
        nodes[index].1.layout = layout;
        let rebuilt = Scene::rebuilt(scene.viewport, nodes)?;
        self.pending = Some(rebuilt);
        self.pending()?.layout(key).ok_or(UiError::MissingNode)
    }

    /// Explicit invalidation is for mutable leaf models. Declarative leaves
    /// can instead change their revision. Invalidations survive failed writes.
    pub fn invalidate_paint(&mut self, key: K) {
        if !self.invalidated.contains(&key) {
            self.invalidated.push(key);
        }
    }

    /// Paint an explicitly updated component during composition. The pending
    /// tree supplies its clip; a painter cannot choose a second allocation.
    /// This eager entry is useful for streaming document leaves. Declarative
    /// revision-based components use `paint` to skip unchanged work.
    pub fn paint_node<R>(
        &self,
        key: &K,
        frame: &mut Frame<'_>,
        painter: impl FnOnce(&mut Paint<'_>) -> R,
    ) -> Result<R, UiError> {
        let layout = self.pending()?.layout(key).ok_or(UiError::MissingNode)?;
        Ok(frame.paint(layout.clip, painter))
    }

    /// Both the previous and new coverage are damaged. A node with unchanged
    /// text but a different clip or layer still needs composition again.
    pub fn damage(&self) -> Result<Vec<Rect>, UiError> {
        let pending = self.pending()?;
        if self.presented.viewport != pending.viewport {
            return Ok(vec![pending.viewport]);
        }
        let mut damage = Vec::new();
        for old in &self.presented.nodes {
            let changed = pending.node(&old.component.key).is_none_or(|new| {
                old.layout != new.layout
                    || old.component.layer != new.component.layer
                    || old.component.revision != new.component.revision
                    || self.invalidated.contains(&old.component.key)
            });
            if changed {
                damage.push(old.layout.clip);
            }
        }
        for new in &pending.nodes {
            let changed = self.presented.node(&new.component.key).is_none_or(|old| {
                old.layout != new.layout
                    || old.component.layer != new.component.layer
                    || old.component.revision != new.component.revision
                    || self.invalidated.contains(&new.component.key)
            });
            if changed {
                damage.push(new.layout.clip);
            }
        }
        // Reordering siblings can change coverage without changing any box.
        let old_order: Vec<_> = self.presented.paint_order().map(|(key, _)| key).collect();
        let new_order: Vec<_> = pending.paint_order().map(|(key, _)| key).collect();
        if old_order != new_order {
            damage.push(pending.viewport);
        }
        damage.retain(|rect| rect.area() > 0);
        Ok(damage)
    }

    /// Composes damaged nodes in paint order. Repaints overlapping unchanged
    /// nodes as well: a clean overlay still covers a changed node behind it.
    /// The caller supplies the root background painter to clear old coverage.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        mut painter: impl FnMut(&K, NodeLayout, &mut Paint<'_>),
    ) -> Result<(), UiError> {
        let damage = self.damage()?;
        if damage.is_empty() {
            return Ok(());
        }
        // Repaint exactly the damage, rect by rect, in paint order. Unioning the
        // rects into one bounding box (the previous behaviour) turned two distant
        // one-cell changes into a near-full-screen repaint.
        let scene = self.pending()?;
        for (key, layout) in scene.paint_order() {
            for rect in &damage {
                let clip = rect.intersection(layout.clip);
                if clip.area() > 0 {
                    frame.paint(clip, |paint| painter(key, layout, paint));
                }
            }
        }
        Ok(())
    }

    /// Call only after successful terminal presentation. This is the only
    /// operation that publishes geometry or disposes unmounted local state.
    pub fn commit(&mut self) -> Result<Vec<Lifecycle<K>>, UiError> {
        let next = self.pending.take().ok_or(UiError::NoPendingFrame)?;
        let mut events = Vec::new();
        // Children are torn down before their logical owners.
        for node in self.presented.nodes.iter().rev() {
            if next.id(&node.component.key) != Some(node.id) {
                self.state.retain(|(owner, _), _| *owner != node.id);
                events.push(Lifecycle::Unmounted {
                    key: node.component.key.clone(),
                    id: node.id,
                });
            }
        }
        for node in &next.nodes {
            if self.presented.id(&node.component.key) != Some(node.id) {
                events.push(Lifecycle::Mounted {
                    key: node.component.key.clone(),
                    id: node.id,
                });
            }
        }
        self.presented = next;
        self.invalidated.clear();
        let previous = self.focused;
        self.focus_history
            .retain(|&id| self.presented.key(id).is_some());
        if self.focused.is_some_and(|id| !self.focus_eligible(id)) {
            if let Some(id) = self.focused.filter(|id| self.presented.key(*id).is_some()) {
                // Duplicate suppression: the history is a stack of distinct ids.
                self.focus_history.retain(|&saved| saved != id);
                self.focus_history.push(id);
            }
            self.focused = None;
        }
        if self.focused.is_none() {
            self.focused = self
                .focus_history
                .iter()
                .rev()
                .copied()
                .find(|&id| self.focus_eligible(id))
                .or_else(|| {
                    self.presented
                        .foreground()
                        .and_then(|key| self.presented.id(key))
                })
                .or_else(|| {
                    self.presented.order.iter().find_map(|&index| {
                        let node = &self.presented.nodes[index];
                        self.focus_eligible(node.id).then_some(node.id)
                    })
                });
        }
        if previous != self.focused {
            events.push(Lifecycle::FocusChanged {
                previous,
                current: self.focused,
            });
        }
        if let Some(id) = self.capture
            && !self.capture_eligible(id)
        {
            self.capture = None;
            events.push(Lifecycle::CaptureReleased { id });
        }
        Ok(events)
    }

    /// The focused instance.
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Move focus, if `id` is eligible.
    pub fn focus(&mut self, id: NodeId) -> Result<(), UiError> {
        if !self.focus_eligible(id) {
            return Err(UiError::InvalidFocus);
        }
        if self.focused != Some(id) {
            if let Some(old) = self.focused {
                // Duplicate suppression: the history is a stack of distinct ids.
                self.focus_history.retain(|&saved| saved != old);
                self.focus_history.push(old);
            }
            self.focused = Some(id);
        }
        Ok(())
    }

    /// Capture the pointer for `id`, if eligible.
    pub fn capture_pointer(&mut self, id: NodeId) -> Result<(), UiError> {
        if !self.capture_eligible(id) {
            return Err(UiError::InvalidCapture);
        }
        self.capture = Some(id);
        Ok(())
    }

    /// Release pointer capture.
    pub fn release_pointer(&mut self) {
        self.capture = None;
    }

    /// The key under `(x, y)`: the capture holder first, then a hit test.
    pub fn pointer_target(&self, x: u16, y: u16) -> Option<&K> {
        self.capture
            .and_then(|id| self.presented.key(id))
            .or_else(|| self.presented.hit_test(x, y))
    }

    /// Per-node local state, keyed by node and type.
    pub fn state<T: Any + Send + Default>(&mut self, id: NodeId) -> Result<&mut T, UiError> {
        if self.presented.key(id).is_none() {
            return Err(UiError::MissingNode);
        }
        // Keyed by node *and* type: one node used to hold a single untyped slot,
        // so whichever type asked first won and every later request for a
        // different type failed with a misleading `MissingNode`.
        self.state
            .entry((id, TypeId::of::<T>()))
            .or_insert_with(|| Box::<T>::default())
            .downcast_mut::<T>()
            .ok_or(UiError::StateTypeMismatch)
    }

    fn focus_eligible(&self, id: NodeId) -> bool {
        self.presented.key(id).is_some_and(|key| {
            self.presented.component(key).is_some_and(|c| c.focusable)
                && self
                    .presented
                    .layout(key)
                    .is_some_and(|layout| layout.clip.area() > 0)
                && self
                    .presented
                    .focus_barrier()
                    .is_none_or(|scope| self.presented.is_descendant(key, scope))
        })
    }

    fn capture_eligible(&self, id: NodeId) -> bool {
        self.presented.key(id).is_some_and(|key| {
            self.presented
                .component(key)
                .is_some_and(|c| c.pointer != PointerPolicy::Transparent)
                && self
                    .presented
                    .layout(key)
                    .is_some_and(|layout| layout.clip.area() > 0)
                && self
                    .presented
                    .pointer_barrier()
                    .is_none_or(|scope| self.presented.is_descendant(key, scope))
        })
    }
}
