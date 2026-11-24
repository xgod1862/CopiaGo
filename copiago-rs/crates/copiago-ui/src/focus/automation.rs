//! The UI thread owns the HWND and visuals. COM reads a synchronized model
//! and posts actions back to the UI thread.

mod provider;

use std::{
    cell::{Cell, OnceCell},
    sync::{Arc, Mutex, MutexGuard},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Accessibility::{
                IRawElementProviderSimple, StructureChangeType_ChildrenInvalidated,
                UIA_AutomationFocusChangedEventId, UIA_Invoke_InvokedEventId,
                UIA_ToggleToggleStatePropertyId, UiaClientsAreListening, UiaDisconnectProvider,
                UiaRaiseAutomationEvent, UiaRaiseAutomationPropertyChangedEvent,
                UiaRaiseStructureChangedEvent, UiaReturnRawElementProvider, UiaRootObjectId,
            },
            Input::KeyboardAndMouse::{
                GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
            },
            WindowsAndMessaging::{
                IsWindowVisible, MSG, PostMessageW, WM_APP, WM_KEYDOWN, WM_KEYUP,
            },
        },
    },
    core::Result,
};

use super::{
    FocusElement, FocusEvent, FocusId,
    model::{Card, Key, ROOT, RuntimeId, Tree},
};
use crate::components::Rect;
use provider::{Provider, decoration_provider, toggle_state};

const FOCUS_CHANGED: u32 = WM_APP + 5;
const AUTOMATION_INVOKE: u32 = WM_APP + 6;

#[derive(Default)]
pub(crate) struct PopupInteraction {
    tree: Arc<Mutex<Tree>>,
    root: OnceCell<IRawElementProviderSimple>,
    announced_focus: Cell<Option<RuntimeId>>,
}

impl PopupInteraction {
    pub(crate) fn place(&self, card: Card) {
        lock(&self.tree).card = Some(card);
    }

    pub(crate) fn hide(&self) {
        lock(&self.tree).hide();
        self.announced_focus.set(None);
    }

    pub(crate) fn native_focus(&self, hwnd: HWND, focused: bool) {
        if lock(&self.tree).set_native_focus(focused) {
            // SAFETY: This is called by the live input HWND's window procedure.
            let _ = unsafe { PostMessageW(Some(hwnd), FOCUS_CHANGED, WPARAM(0), LPARAM(0)) };
        }
    }

    pub(crate) fn get_object(&self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if lparam.0 as i32 != UiaRootObjectId || lock(&self.tree).card.is_none() {
            return None;
        }
        // SAFETY: This is processing UI Automation's WM_GETOBJECT on the live HWND.
        Some(unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, self.root(hwnd)) })
    }

    pub(crate) fn destroy(&self, hwnd: HWND) {
        self.hide();
        if let Some(root) = self.root.get() {
            // SAFETY: WM_DESTROY is still processing for this HWND and the root is live.
            unsafe {
                let _ = UiaDisconnectProvider(root);
                UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
            }
        }
    }

    pub(crate) fn decoration_object(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if lparam.0 as i32 != UiaRootObjectId {
            return None;
        }
        // SAFETY: This is the owned display window processing UIA's WM_GETOBJECT.
        unsafe {
            IsWindowVisible(hwnd).as_bool().then(|| {
                UiaReturnRawElementProvider(hwnd, wparam, lparam, &decoration_provider(hwnd))
            })
        }
    }

    pub(crate) fn destroy_decoration(hwnd: HWND) {
        // SAFETY: Clear the raised-event map while processing this HWND's WM_DESTROY.
        unsafe { UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None) };
    }

    pub(crate) fn publish(&self, hwnd: HWND, elements: Vec<FocusElement>) -> Result<()> {
        let update = lock(&self.tree).publish(elements)?;
        if listening() {
            if update.structure_changed {
                // SAFETY: ChildrenInvalidated refers to the root itself, so no child runtime ID is required.
                let _ = unsafe {
                    UiaRaiseStructureChangedEvent(
                        self.root(hwnd),
                        StructureChangeType_ChildrenInvalidated,
                        std::ptr::null_mut(),
                        0,
                    )
                };
            }
            for (runtime, before, after) in update.toggles_changed {
                let provider = Provider::new(&self.tree, hwnd, runtime).simple();
                // SAFETY: The provider and both ToggleState variants live through the call.
                let _ = unsafe {
                    UiaRaiseAutomationPropertyChangedEvent(
                        &provider,
                        UIA_ToggleToggleStatePropertyId,
                        &toggle_state(before).0.into(),
                        &toggle_state(after).0.into(),
                    )
                };
            }
        }
        Ok(())
    }

    pub(crate) fn focused_element(&self) -> Option<FocusId> {
        lock(&self.tree).focus_id()
    }

    pub(crate) fn request_focus(&self, id: Option<FocusId>) -> bool {
        lock(&self.tree).requested_focus(id)
    }

    pub(crate) fn focus_bounds(&self) -> Option<Rect> {
        let tree = lock(&self.tree);
        let runtime = tree.native_target().filter(|id| *id != ROOT)?;
        tree.bounds(runtime)
    }

    pub(crate) fn announce_focus(&self, hwnd: HWND) {
        let target = lock(&self.tree).native_target();
        let previous = self.announced_focus.replace(target);
        if let Some(runtime) = target.filter(|target| Some(*target) != previous)
            && listening()
        {
            let provider = self.simple(hwnd, runtime);
            // SAFETY: The provider is valid for the live HWND's current tree.
            let _ =
                unsafe { UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId) };
        }
    }

    pub(crate) fn notify_invoked(&self, hwnd: HWND, id: FocusId) {
        if !listening() {
            return;
        }
        let runtime = lock(&self.tree)
            .nodes
            .iter()
            .find(|node| {
                node.element.id == id
                    && matches!(
                        node.element.role,
                        super::FocusRole::Button | super::FocusRole::ListItem
                    )
            })
            .map(|node| node.runtime);
        if let Some(runtime) = runtime {
            let provider = self.simple(hwnd, runtime);
            // SAFETY: The provider remains live throughout the invocation notification.
            let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_Invoke_InvokedEventId) };
        }
    }

    pub(crate) fn handle_message(&self, message: &MSG) -> Option<FocusEvent> {
        let mut tree = lock(&self.tree);
        match message.message {
            FOCUS_CHANGED => Some(FocusEvent::Changed(tree.visible_focus_id())),
            AUTOMATION_INVOKE => {
                let runtime = RuntimeId::try_from(message.lParam.0).ok()?;
                tree.card?;
                tree.action(runtime).map(FocusEvent::Activated)
            }
            WM_KEYDOWN | WM_KEYUP => {
                let key = match VIRTUAL_KEY(u16::try_from(message.wParam.0).ok()?) {
                    VK_TAB => Key::Tab {
                        reverse: key_down(VK_SHIFT),
                    },
                    VK_RETURN => Key::Enter,
                    VK_SPACE => Key::Space,
                    _ => return None,
                };
                if key_down(VK_CONTROL) {
                    tree.cancel_space();
                    return None;
                }
                let repeated = message.lParam.0 & (1 << 30) != 0;
                tree.key(key, message.message == WM_KEYDOWN, repeated)
            }
            _ => None,
        }
    }

    fn root(&self, hwnd: HWND) -> &IRawElementProviderSimple {
        self.root
            .get_or_init(|| Provider::new(&self.tree, hwnd, ROOT).simple())
    }

    fn simple(&self, hwnd: HWND, runtime: RuntimeId) -> IRawElementProviderSimple {
        if runtime == ROOT {
            self.root(hwnd).clone()
        } else {
            Provider::new(&self.tree, hwnd, runtime).simple()
        }
    }
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: The key state belongs to the calling UI thread's message queue.
    unsafe { GetKeyState(i32::from(key.0)) < 0 }
}

fn listening() -> bool {
    // SAFETY: This takes no handles or pointers and reads UI Automation listener state.
    unsafe { UiaClientsAreListening().as_bool() }
}

fn lock(tree: &Mutex<Tree>) -> MutexGuard<'_, Tree> {
    tree.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
