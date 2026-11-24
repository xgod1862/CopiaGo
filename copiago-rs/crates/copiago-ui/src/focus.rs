//! Keyboard and accessibility semantics for Composition-drawn popup elements.

mod automation;
mod model;

pub(crate) use automation::PopupInteraction;
pub(crate) use model::Card;

use crate::components::Rect;

/// Stable application ID. Keep it for the same control across layout and label changes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FocusId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusRole {
    Button,
    Toggle { checked: bool },
    List,
    ListItem,
}

/// A control or list in the order in which Tab should visit it.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusElement {
    pub id: FocusId,
    /// An optional list or list item earlier in the published sequence.
    pub parent: Option<FocusId>,
    /// Accessible, localized text, including labels for icon-only buttons.
    pub name: String,
    pub role: FocusRole,
    /// Visible bounds in card-relative DIPs, clipped to the scroll viewport if
    /// applicable. Use None for offscreen elements that can be revealed on focus.
    pub bounds: Option<Rect>,
    pub enabled: bool,
}

/// Returned before the native message is dispatched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusEvent {
    /// Reveal an offscreen target and publish the updated bounds when applicable.
    Changed(Option<FocusId>),
    /// Dispatch the same application action used for pointer input.
    Activated(FocusId),
    Consumed,
}
