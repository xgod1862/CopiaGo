use std::sync::{Arc, Mutex, Weak};

use windows::{
    Win32::{
        Foundation::{E_OUTOFMEMORY, E_POINTER, HWND, LPARAM, WPARAM},
        System::{
            Com::SAFEARRAY,
            Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement},
            Variant::{VARIANT, VT_I4},
        },
        UI::{Accessibility::*, WindowsAndMessaging::PostMessageW},
    },
    core::{Error, IUnknown, IUnknown_Vtbl, Interface, OutRef, Result, implement, interface},
};
use windows_numerics::Vector2;

use super::{AUTOMATION_INVOKE, FOCUS_CHANGED, ROOT, RuntimeId, Tree, lock};
use crate::focus::FocusRole;

// These match the SDK interfaces. OutRef permits S_OK with a null interface
// for a missing relative, pattern, or focused element. The generated provider
// traits return non-null Rust interfaces for those same out parameters.
#[interface("d6dd68d1-86fd-4332-8666-9abedea2d24c")]
unsafe trait RawSimple: IUnknown {
    fn provider_options(&self, result: *mut ProviderOptions) -> Result<()>;
    fn get_pattern_provider(&self, id: UIA_PATTERN_ID, result: OutRef<'_, IUnknown>) -> Result<()>;
    fn get_property_value(&self, id: UIA_PROPERTY_ID, result: *mut VARIANT) -> Result<()>;
    fn host_raw_element_provider(
        &self,
        result: OutRef<'_, IRawElementProviderSimple>,
    ) -> Result<()>;
}

#[interface("f7063da8-8359-439c-9297-bbc5299a7d87")]
unsafe trait RawFragment: IUnknown {
    fn navigate(
        &self,
        direction: NavigateDirection,
        result: OutRef<'_, IRawElementProviderFragment>,
    ) -> Result<()>;
    fn get_runtime_id(&self, result: OutRef<'_, *mut SAFEARRAY>) -> Result<()>;
    fn bounding_rectangle(&self, result: *mut UiaRect) -> Result<()>;
    fn get_embedded_fragment_roots(&self, result: OutRef<'_, *mut SAFEARRAY>) -> Result<()>;
    fn set_focus(&self) -> Result<()>;
    fn fragment_root(&self, result: OutRef<'_, IRawElementProviderFragmentRoot>) -> Result<()>;
}

#[interface("620ce2a5-ab8f-40a9-86cb-de3c75599b58")]
unsafe trait RawRoot: IUnknown {
    fn element_provider_from_point(
        &self,
        x: f64,
        y: f64,
        result: OutRef<'_, IRawElementProviderFragment>,
    ) -> Result<()>;
    fn get_focus(&self, result: OutRef<'_, IRawElementProviderFragment>) -> Result<()>;
}

// Provider instances can outlive their element. They hold neither the popup's
// snapshot nor graphics alive, and every query/action revalidates the runtime ID.
#[derive(Clone)]
pub(super) struct Provider {
    tree: Weak<Mutex<Tree>>,
    hwnd: isize,
    runtime: RuntimeId,
}

#[implement(RawSimple, RawFragment, RawRoot)]
struct Root(Provider);

#[implement(RawSimple, RawFragment)]
struct Element(Provider);

#[implement(RawSimple)]
struct Decoration(isize);

#[implement(IInvokeProvider)]
struct Invoke(Provider);

#[implement(IToggleProvider)]
struct Toggle(Provider);

#[allow(non_upper_case_globals)] // Match the Windows SDK's property and navigation constants.
impl Provider {
    pub(super) fn new(tree: &Arc<Mutex<Tree>>, hwnd: HWND, runtime: RuntimeId) -> Self {
        Self {
            tree: Arc::downgrade(tree),
            hwnd: hwnd.0 as isize,
            runtime,
        }
    }

    pub(super) fn simple(self) -> IRawElementProviderSimple {
        let simple: RawSimple = if self.runtime == ROOT {
            Root(self).into()
        } else {
            Element(self).into()
        };
        // SAFETY: RawSimple uses the same IID and vtable ABI as IRawElementProviderSimple.
        unsafe { IRawElementProviderSimple::from_raw(simple.into_raw()) }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    fn at(&self, runtime: RuntimeId) -> Self {
        Self {
            runtime,
            ..self.clone()
        }
    }

    fn fragment(&self, runtime: Option<RuntimeId>) -> Result<Option<IRawElementProviderFragment>> {
        runtime
            .map(|runtime| self.at(runtime).simple().cast())
            .transpose()
    }

    fn with_tree<T>(&self, query: impl FnOnce(&mut Tree) -> Result<T>) -> Result<T> {
        let tree = self.tree.upgrade().ok_or_else(unavailable)?;
        let mut guard = lock(&tree);
        if guard.card.is_none() || (self.runtime != ROOT && guard.get(self.runtime).is_none()) {
            return Err(unavailable());
        }
        query(&mut guard)
    }

    fn navigation(
        &self,
        direction: NavigateDirection,
    ) -> Result<Option<IRawElementProviderFragment>> {
        let id = self.with_tree(|tree| {
            let current = tree.get(self.runtime);
            let mut children = tree.nodes.iter().filter(|node| node.parent == self.runtime);
            let related = match direction {
                NavigateDirection_Parent => current.map(|node| node.parent),
                NavigateDirection_FirstChild => children.next().map(|node| node.runtime),
                NavigateDirection_LastChild => children.next_back().map(|node| node.runtime),
                NavigateDirection_NextSibling | NavigateDirection_PreviousSibling => current
                    .and_then(|node| {
                        let mut siblings = tree
                            .nodes
                            .iter()
                            .filter(|sibling| sibling.parent == node.parent);
                        let neighbor = if direction == NavigateDirection_NextSibling {
                            siblings.find(|sibling| sibling.runtime == self.runtime)?;
                            siblings.next()
                        } else {
                            siblings.rfind(|sibling| sibling.runtime == self.runtime)?;
                            siblings.next_back()
                        };
                        neighbor.map(|neighbor| neighbor.runtime)
                    }),
                _ => None,
            };
            Ok(related)
        })?;
        self.fragment(id)
    }

    fn bounds(&self) -> Result<UiaRect> {
        self.with_tree(|tree| {
            let card = tree.card.ok_or_else(unavailable)?;
            Ok(tree
                .bounds(self.runtime)
                .map_or(UiaRect::default(), |rect| UiaRect {
                    left: f64::from(card.origin.x) + f64::from(rect.x) * card.scale,
                    top: f64::from(card.origin.y) + f64::from(rect.y) * card.scale,
                    width: f64::from(rect.width) * card.scale,
                    height: f64::from(rect.height) * card.scale,
                }))
        })
    }

    fn property(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.with_tree(|tree| {
            let value = match id {
                UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => true.into(),
                UIA_HasKeyboardFocusPropertyId => {
                    (tree.native_target() == Some(self.runtime)).into()
                }
                UIA_IsOffscreenPropertyId => tree.bounds(self.runtime).is_none().into(),
                _ => match tree.get(self.runtime) {
                    None => match id {
                        UIA_NamePropertyId => "CopiaGo".into(),
                        UIA_ControlTypePropertyId => UIA_WindowControlTypeId.0.into(),
                        UIA_IsEnabledPropertyId | UIA_IsKeyboardFocusablePropertyId => true.into(),
                        _ => VARIANT::default(),
                    },
                    Some(node) => match id {
                        UIA_NamePropertyId => node.element.name.as_str().into(),
                        UIA_ControlTypePropertyId => control_type(node.element.role).0.into(),
                        UIA_IsKeyboardFocusablePropertyId => node.focusable().into(),
                        UIA_IsEnabledPropertyId => node.element.enabled.into(),
                        _ => VARIANT::default(),
                    },
                },
            };
            Ok(value)
        })
    }

    fn pattern(&self, pattern: UIA_PATTERN_ID) -> Result<Option<IUnknown>> {
        let role =
            self.with_tree(|tree| Ok(tree.get(self.runtime).map(|node| node.element.role)))?;
        Ok(match (pattern, role) {
            (UIA_InvokePatternId, Some(FocusRole::Button | FocusRole::ListItem)) => {
                let provider: IInvokeProvider = Invoke(self.clone()).into();
                Some(provider.into())
            }
            (UIA_TogglePatternId, Some(FocusRole::Toggle { .. })) => {
                let provider: IToggleProvider = Toggle(self.clone()).into();
                Some(provider.into())
            }
            _ => None,
        })
    }

    fn check_action(&self, tree: &Tree) -> Result<()> {
        let node = tree.get(self.runtime).ok_or_else(unavailable)?;
        if !node.element.enabled {
            return Err(error(UIA_E_ELEMENTNOTENABLED));
        }
        if !node.focusable() {
            return Err(error(UIA_E_INVALIDOPERATION));
        }
        Ok(())
    }

    fn focus(&self) -> Result<()> {
        self.with_tree(|tree| {
            if self.runtime != ROOT {
                self.check_action(tree)?;
            }
            // SAFETY: The target HWND is live for the duration of the tree lock.
            unsafe { PostMessageW(Some(self.hwnd()), FOCUS_CHANGED, WPARAM(0), LPARAM(0))? };
            tree.set_focus((self.runtime != ROOT).then_some(self.runtime));
            Ok(())
        })
    }

    fn invoke(&self) -> Result<()> {
        self.with_tree(|tree| {
            self.check_action(tree)?;
            // SAFETY: The tree lock keeps the window live. The ID is carried by value.
            unsafe {
                PostMessageW(
                    Some(self.hwnd()),
                    AUTOMATION_INVOKE,
                    WPARAM(0),
                    LPARAM(self.runtime as isize),
                )
            }
        })
    }
}

macro_rules! impl_simple_and_fragment {
    ($provider:ident) => {
        impl RawSimple_Impl for $provider {
            unsafe fn provider_options(&self, result: *mut ProviderOptions) -> Result<()> {
                // SAFETY: COM supplies this writable out parameter when it is not null.
                unsafe {
                    write_out(
                        result,
                        ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading,
                    )
                }
            }

            unsafe fn get_pattern_provider(
                &self,
                id: UIA_PATTERN_ID,
                result: OutRef<'_, IUnknown>,
            ) -> Result<()> {
                result.write(self.0.pattern(id)?)
            }

            unsafe fn get_property_value(
                &self,
                id: UIA_PROPERTY_ID,
                result: *mut VARIANT,
            ) -> Result<()> {
                // SAFETY: COM takes ownership of the VARIANT written to this out parameter.
                unsafe { write_out(result, self.0.property(id)?) }
            }

            unsafe fn host_raw_element_provider(
                &self,
                result: OutRef<'_, IRawElementProviderSimple>,
            ) -> Result<()> {
                self.0.with_tree(|_| Ok(()))?;
                if self.0.runtime != ROOT {
                    return result.write(None);
                }
                // SAFETY: The root belongs to this HWND, validated against the live tree.
                result.write(Some(unsafe { UiaHostProviderFromHwnd(self.0.hwnd())? }))
            }
        }

        impl RawFragment_Impl for $provider {
            unsafe fn navigate(
                &self,
                direction: NavigateDirection,
                result: OutRef<'_, IRawElementProviderFragment>,
            ) -> Result<()> {
                result.write(self.0.navigation(direction)?)
            }

            unsafe fn get_runtime_id(&self, result: OutRef<'_, *mut SAFEARRAY>) -> Result<()> {
                if result.is_null() {
                    return Err(Error::from_hresult(E_POINTER));
                }
                self.0.with_tree(|_| Ok(()))?;
                let runtime = self.0.runtime;
                if runtime == ROOT {
                    result.write(std::ptr::null_mut())
                } else {
                    result.write(runtime_id(runtime)?)
                }
            }

            unsafe fn bounding_rectangle(&self, result: *mut UiaRect) -> Result<()> {
                // SAFETY: COM supplies this writable out parameter when it is not null.
                unsafe { write_out(result, self.0.bounds()?) }
            }

            unsafe fn get_embedded_fragment_roots(
                &self,
                result: OutRef<'_, *mut SAFEARRAY>,
            ) -> Result<()> {
                self.0.with_tree(|_| Ok(()))?;
                result.write(std::ptr::null_mut())
            }

            unsafe fn set_focus(&self) -> Result<()> {
                // UI Automation focuses the HWND. Update virtual focus and queue its repaint.
                self.0.focus()
            }

            unsafe fn fragment_root(
                &self,
                result: OutRef<'_, IRawElementProviderFragmentRoot>,
            ) -> Result<()> {
                self.0.with_tree(|_| Ok(()))?;
                result.write(Some(self.0.at(ROOT).simple().cast()?))
            }
        }
    };
}

impl_simple_and_fragment!(Root_Impl);
impl_simple_and_fragment!(Element_Impl);

impl RawSimple_Impl for Decoration_Impl {
    unsafe fn provider_options(&self, result: *mut ProviderOptions) -> Result<()> {
        // SAFETY: COM supplies this writable out parameter when it is not null.
        unsafe {
            write_out(
                result,
                ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading,
            )
        }
    }

    unsafe fn get_pattern_provider(
        &self,
        _: UIA_PATTERN_ID,
        result: OutRef<'_, IUnknown>,
    ) -> Result<()> {
        result.write(None)
    }

    unsafe fn get_property_value(&self, id: UIA_PROPERTY_ID, result: *mut VARIANT) -> Result<()> {
        let value = if id == UIA_IsControlElementPropertyId || id == UIA_IsContentElementPropertyId
        {
            false.into()
        } else {
            VARIANT::default()
        };
        // SAFETY: COM takes ownership of the VARIANT written to this out parameter.
        unsafe { write_out(result, value) }
    }

    unsafe fn host_raw_element_provider(
        &self,
        result: OutRef<'_, IRawElementProviderSimple>,
    ) -> Result<()> {
        // SAFETY: This provider serves the owned, display-only window.
        result.write(Some(unsafe {
            UiaHostProviderFromHwnd(HWND(self.0 as *mut _))?
        }))
    }
}

pub(super) fn decoration_provider(hwnd: HWND) -> IRawElementProviderSimple {
    let raw: RawSimple = Decoration(hwnd.0 as isize).into();
    // SAFETY: RawSimple uses the same IID and vtable ABI as IRawElementProviderSimple.
    unsafe { IRawElementProviderSimple::from_raw(raw.into_raw()) }
}

impl RawRoot_Impl for Root_Impl {
    unsafe fn element_provider_from_point(
        &self,
        x: f64,
        y: f64,
        result: OutRef<'_, IRawElementProviderFragment>,
    ) -> Result<()> {
        let runtime = self.0.with_tree(|tree| {
            let card = tree.card.ok_or_else(unavailable)?;
            Ok(tree.hit(Vector2::new(
                ((x - f64::from(card.origin.x)) / card.scale) as f32,
                ((y - f64::from(card.origin.y)) / card.scale) as f32,
            )))
        })?;
        result.write(self.0.fragment(runtime)?)
    }

    unsafe fn get_focus(&self, result: OutRef<'_, IRawElementProviderFragment>) -> Result<()> {
        let runtime = self.0.with_tree(|tree| Ok(tree.native_target()))?;
        result.write(self.0.fragment(runtime)?)
    }
}

impl IInvokeProvider_Impl for Invoke_Impl {
    fn Invoke(&self) -> Result<()> {
        self.0.invoke()
    }
}

impl IToggleProvider_Impl for Toggle_Impl {
    fn Toggle(&self) -> Result<()> {
        self.0.invoke()
    }

    fn ToggleState(&self) -> Result<ToggleState> {
        self.0.with_tree(
            |tree| match tree.get(self.0.runtime).map(|node| node.element.role) {
                Some(FocusRole::Toggle { checked }) => Ok(toggle_state(checked)),
                _ => Err(unavailable()),
            },
        )
    }
}

fn control_type(role: FocusRole) -> UIA_CONTROLTYPE_ID {
    match role {
        FocusRole::Button => UIA_ButtonControlTypeId,
        FocusRole::Toggle { .. } => UIA_CheckBoxControlTypeId,
        FocusRole::List => UIA_ListControlTypeId,
        FocusRole::ListItem => UIA_ListItemControlTypeId,
    }
}

pub(super) fn toggle_state(checked: bool) -> ToggleState {
    if checked {
        ToggleState_On
    } else {
        ToggleState_Off
    }
}

fn runtime_id(runtime: RuntimeId) -> Result<*mut SAFEARRAY> {
    let values = [UiaAppendRuntimeId as i32, runtime];
    // SAFETY: Allocate a one-dimensional VT_I4 SAFEARRAY and populate its
    // two elements by copying from live i32 values. Transfer it to the caller.
    unsafe {
        let array = SafeArrayCreateVector(VT_I4, 0, values.len() as u32);
        if array.is_null() {
            return Err(Error::from_hresult(E_OUTOFMEMORY));
        }
        for (index, value) in (0_i32..).zip(values.iter()) {
            if let Err(error) = SafeArrayPutElement(array, &index, std::ptr::from_ref(value).cast())
            {
                let _ = SafeArrayDestroy(array);
                return Err(error);
            }
        }
        Ok(array)
    }
}

fn error(code: u32) -> Error {
    Error::from_hresult(windows::core::HRESULT(code as i32))
}

unsafe fn write_out<T>(result: *mut T, value: T) -> Result<()> {
    if result.is_null() {
        return Err(Error::from_hresult(E_POINTER));
    }
    // SAFETY: The caller guarantees writable storage at a non-null result pointer.
    unsafe { result.write(value) };
    Ok(())
}

fn unavailable() -> Error {
    error(UIA_E_ELEMENTNOTAVAILABLE)
}
