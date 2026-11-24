use std::collections::{HashMap, HashSet};

use windows::{
    Win32::Foundation::{E_FAIL, E_INVALIDARG, POINT},
    core::{Error, Result},
};
use windows_numerics::Vector2;

use super::{FocusElement, FocusEvent, FocusId, FocusRole};
use crate::components::Rect;

/// Zero identifies the HWND-hosted root. Child IDs are never reused by this host.
pub(super) type RuntimeId = i32;
pub(super) const ROOT: RuntimeId = 0;

#[derive(Clone, Copy)]
pub(crate) struct Card {
    pub origin: POINT,
    pub bounds: Rect,
    pub scale: f64,
}

pub(super) struct Node {
    pub element: FocusElement,
    pub runtime: RuntimeId,
    pub parent: RuntimeId,
}

impl Node {
    pub fn focusable(&self) -> bool {
        self.element.enabled && self.element.role != FocusRole::List
    }
}

#[derive(Clone, Copy)]
pub(super) enum Key {
    Tab { reverse: bool },
    Enter,
    Space,
}

pub(super) struct Update {
    pub structure_changed: bool,
    pub toggles_changed: Vec<(RuntimeId, bool, bool)>,
}

#[derive(Default)]
pub(super) struct Tree {
    pub card: Option<Card>,
    pub nodes: Vec<Node>,
    pub native_focus: bool,
    focus: Option<RuntimeId>,
    space_down: Option<RuntimeId>,
    last_runtime: RuntimeId,
}

impl Tree {
    pub fn publish(&mut self, elements: Vec<FocusElement>) -> Result<Update> {
        validate(&elements)?;
        let previous: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|node| (node.element.id, node))
            .collect();
        let mut last_runtime = self.last_runtime;
        let mut runtimes = HashMap::with_capacity(elements.len());
        let mut nodes = Vec::with_capacity(elements.len());
        let mut toggles_changed = Vec::new();
        for element in elements {
            let parent = match element.parent {
                Some(id) => runtimes[&id],
                None => ROOT,
            };
            let runtime = match previous.get(&element.id).filter(|old| {
                old.parent == parent
                    && std::mem::discriminant(&old.element.role)
                        == std::mem::discriminant(&element.role)
            }) {
                Some(old) => {
                    if let (
                        FocusRole::Toggle { checked: before },
                        FocusRole::Toggle { checked: after },
                    ) = (old.element.role, element.role)
                        && before != after
                    {
                        toggles_changed.push((old.runtime, before, after));
                    }
                    old.runtime
                }
                None => {
                    last_runtime = last_runtime
                        .checked_add(1)
                        .ok_or_else(|| Error::from_hresult(E_FAIL))?;
                    last_runtime
                }
            };
            runtimes.insert(element.id, runtime);
            nodes.push(Node {
                element,
                runtime,
                parent,
            });
        }
        let structure_changed = !nodes
            .iter()
            .map(|node| node.runtime)
            .eq(self.nodes.iter().map(|node| node.runtime));
        self.last_runtime = last_runtime;
        self.nodes = nodes;
        if self.focus.is_some_and(|id| self.action(id).is_none()) {
            self.set_focus(None);
        }
        Ok(Update {
            structure_changed,
            toggles_changed,
        })
    }

    pub fn hide(&mut self) {
        self.card = None;
        self.nodes = Vec::new();
        self.set_native_focus(false);
        self.set_focus(None);
    }

    pub fn get(&self, id: RuntimeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.runtime == id)
    }

    pub fn action(&self, runtime: RuntimeId) -> Option<FocusId> {
        self.get(runtime)
            .filter(|node| node.focusable())
            .map(|node| node.element.id)
    }

    pub fn requested_focus(&mut self, id: Option<FocusId>) -> bool {
        match id {
            None => self.set_focus(None),
            Some(id) => self
                .nodes
                .iter()
                .find(|node| node.element.id == id && node.focusable())
                .map(|node| node.runtime)
                .is_some_and(|runtime| self.set_focus(Some(runtime))),
        }
    }

    pub fn focus_id(&self) -> Option<FocusId> {
        self.focus.and_then(|runtime| self.action(runtime))
    }

    /// The root owns focus while Windows focuses the HWND and no child has focus.
    pub fn native_target(&self) -> Option<RuntimeId> {
        (self.card.is_some() && self.native_focus).then_some(self.focus.unwrap_or(ROOT))
    }

    pub fn visible_focus_id(&self) -> Option<FocusId> {
        self.native_target()
            .and_then(|runtime| self.action(runtime))
    }

    pub fn bounds(&self, id: RuntimeId) -> Option<Rect> {
        let card = self.card?;
        if id == ROOT {
            return Some(card.bounds);
        }
        card.bounds.intersect(self.get(id)?.element.bounds?)
    }

    pub fn hit(&self, point: Vector2) -> Option<RuntimeId> {
        let card = self.card?;
        if !card.bounds.contains(point) {
            return None;
        }
        Some(
            self.nodes
                .iter()
                .rev()
                .find(|node| {
                    self.bounds(node.runtime)
                        .is_some_and(|rect| rect.contains(point))
                })
                .map_or(ROOT, |node| node.runtime),
        )
    }

    pub fn set_native_focus(&mut self, focused: bool) -> bool {
        if !focused {
            self.space_down = None;
        }
        std::mem::replace(&mut self.native_focus, focused) != focused
    }

    pub fn set_focus(&mut self, focus: Option<RuntimeId>) -> bool {
        if self.focus == focus {
            return false;
        }
        self.space_down = None;
        self.focus = focus;
        true
    }

    pub fn cancel_space(&mut self) {
        self.space_down = None;
    }

    pub fn key(&mut self, key: Key, down: bool, repeated: bool) -> Option<FocusEvent> {
        self.native_target()?;
        match key {
            Key::Tab { reverse } if down => {
                let next = self.tab(reverse)?;
                Some(if self.set_focus(Some(next)) {
                    FocusEvent::Changed(self.focus_id())
                } else {
                    FocusEvent::Consumed
                })
            }
            Key::Tab { .. } => None,
            Key::Enter => self.focus_id().map(|id| {
                if down && !repeated {
                    FocusEvent::Activated(id)
                } else {
                    FocusEvent::Consumed
                }
            }),
            Key::Space if down => {
                let runtime = self.focus?;
                if !repeated {
                    self.space_down = Some(runtime);
                }
                Some(FocusEvent::Consumed)
            }
            Key::Space => {
                let pressed = self.space_down.take();
                let focus = self.focus?;
                Some(if pressed == Some(focus) {
                    FocusEvent::Activated(self.action(focus)?)
                } else {
                    FocusEvent::Consumed
                })
            }
        }
    }

    fn tab(&self, reverse: bool) -> Option<RuntimeId> {
        let mut candidates = self.nodes.iter().filter(|node| node.focusable());
        let first = if reverse {
            candidates.next_back()
        } else {
            candidates.next()
        }?;
        let mut next = Some(first);
        while let Some(node) = next {
            let following = if reverse {
                candidates.next_back()
            } else {
                candidates.next()
            };
            if Some(node.runtime) == self.focus {
                return Some(following.unwrap_or(first).runtime);
            }
            next = following;
        }
        Some(first.runtime)
    }
}

fn validate(elements: &[FocusElement]) -> Result<()> {
    let mut ids = HashSet::with_capacity(elements.len());
    let mut parents = HashSet::new();
    for element in elements {
        if !ids.insert(element.id)
            || element.name.trim().is_empty()
            || element.parent.is_some_and(|id| !parents.contains(&id))
            || element.bounds.is_some_and(|rect| {
                rect.width <= 0.0
                    || rect.height <= 0.0
                    || ![rect.x, rect.y, rect.right(), rect.bottom()]
                        .into_iter()
                        .all(f32::is_finite)
            })
        {
            return Err(Error::new(E_INVALIDARG, "Invalid popup focus element"));
        }
        if matches!(element.role, FocusRole::List | FocusRole::ListItem) {
            parents.insert(element.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: u64, role: FocusRole) -> FocusElement {
        FocusElement {
            id: FocusId(id),
            parent: None,
            name: format!("Control {id}"),
            role,
            bounds: Some(Rect::new(0.0, 0.0, 30.0, 40.0)),
            enabled: true,
        }
    }

    fn active_tree() -> Tree {
        Tree {
            card: Some(Card {
                origin: POINT::default(),
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                scale: 1.5,
            }),
            native_focus: true,
            ..Tree::default()
        }
    }

    #[test]
    fn tab_wraps_in_both_directions_and_reaches_offscreen_rows() {
        let mut tree = active_tree();
        let button = element(1, FocusRole::Button);
        let mut disabled = element(2, FocusRole::Button);
        disabled.enabled = false;
        let list = element(3, FocusRole::List);
        let mut row = element(4, FocusRole::ListItem);
        row.bounds = None;
        row.parent = Some(list.id);
        tree.publish(vec![button, disabled, list, row]).unwrap();

        for (reverse, id) in [(false, 1), (false, 4), (false, 1), (true, 4), (true, 1)] {
            assert_eq!(
                tree.key(Key::Tab { reverse }, true, false),
                Some(FocusEvent::Changed(Some(FocusId(id))))
            );
        }
    }

    #[test]
    fn enter_and_space_activate_once_and_cancel_on_focus_loss() {
        let mut tree = active_tree();
        let button = element(1, FocusRole::Button);
        let list_item = element(2, FocusRole::ListItem);
        tree.publish(vec![button, list_item]).unwrap();
        assert_eq!(tree.key(Key::Enter, true, false), None);
        assert!(tree.requested_focus(Some(FocusId(1))));
        assert_eq!(
            tree.key(Key::Enter, true, false),
            Some(FocusEvent::Activated(FocusId(1)))
        );
        assert_eq!(tree.key(Key::Enter, true, true), Some(FocusEvent::Consumed));
        assert_eq!(
            tree.key(Key::Space, true, false),
            Some(FocusEvent::Consumed)
        );
        assert_eq!(tree.key(Key::Space, true, true), Some(FocusEvent::Consumed));
        assert_eq!(
            tree.key(Key::Space, false, true),
            Some(FocusEvent::Activated(FocusId(1)))
        );
        assert_eq!(
            tree.key(Key::Space, false, true),
            Some(FocusEvent::Consumed)
        );

        tree.key(Key::Space, true, false);
        tree.requested_focus(Some(FocusId(2)));
        assert_eq!(
            tree.key(Key::Space, false, true),
            Some(FocusEvent::Consumed)
        );
        tree.key(Key::Space, true, false);
        tree.set_native_focus(false);
        assert_eq!(tree.native_target(), None);
        assert_eq!(tree.focus_id(), Some(FocusId(2)));
        tree.set_native_focus(true);
        assert_eq!(
            tree.key(Key::Space, false, true),
            Some(FocusEvent::Consumed)
        );
    }

    #[test]
    fn publication_preserves_identity_until_the_control_disappears() {
        let mut tree = active_tree();
        let mut button = element(7, FocusRole::Button);
        let mut toggle = element(8, FocusRole::Toggle { checked: false });
        let ids = vec![button.clone(), toggle.clone()];
        assert!(tree.publish(ids).unwrap().structure_changed);
        let button_runtime = tree.nodes[0].runtime;
        let toggle_runtime = tree.nodes[1].runtime;
        tree.requested_focus(Some(button.id));
        button.name = String::from("Updated label");
        button.bounds = Some(Rect::new(80.0, 0.0, 40.0, 30.0));
        toggle.role = FocusRole::Toggle { checked: true };
        let update = tree.publish(vec![button.clone(), toggle.clone()]).unwrap();
        assert!(!update.structure_changed);
        assert_eq!(update.toggles_changed, [(toggle_runtime, false, true)]);
        assert_eq!(tree.native_target(), Some(button_runtime));
        assert_eq!(
            tree.bounds(button_runtime),
            Some(Rect::new(80.0, 0.0, 20.0, 30.0))
        );
        assert_eq!(tree.hit(Vector2::new(95.0, 20.0)), Some(button_runtime));

        assert!(tree.publish(vec![button.clone(), button.clone()]).is_err());
        assert_eq!(tree.native_target(), Some(button_runtime));
        tree.key(Key::Space, true, false);
        button.enabled = false;
        tree.publish(vec![button.clone(), toggle.clone()]).unwrap();
        assert_eq!(tree.native_target(), Some(ROOT));
        assert_eq!(tree.action(button_runtime), None);
        assert_eq!(tree.key(Key::Space, false, true), None);

        tree.publish(vec![toggle]).unwrap();
        tree.publish(vec![button]).unwrap();
        assert!(tree.get(button_runtime).is_none());
        let fresh_runtime = tree.nodes[0].runtime;
        assert_ne!(fresh_runtime, button_runtime);
        tree.hide();
        assert!(tree.get(fresh_runtime).is_none());
        assert_eq!(tree.native_target(), None);
    }
}
