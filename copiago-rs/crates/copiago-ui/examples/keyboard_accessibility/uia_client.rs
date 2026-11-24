use std::{
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Accessibility::*,
    },
    core::{Error, Result},
};

use super::server::{BUTTON_NAME, DISABLED_NAME, ITEM_NAME, LIST_NAME, TOGGLE_NAME};

pub(super) fn query(hwnd: HWND) -> Result<()> {
    // SAFETY: COM is initialized and uninitialized on this client thread.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let _apartment = Apartment;
    let started = Instant::now();
    // SAFETY: All UIA interfaces are obtained and used within the client's MTA.
    // The target HWND was supplied by the parent probe process.
    unsafe {
        let client: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let root = client.ElementFromHandle(hwnd)?;
        check_node(&root, "CopiaGo", UIA_WindowControlTypeId)?;
        let condition = client.ControlViewCondition()?;
        let children = root.FindAll(TreeScope_Children, &condition)?;
        if children.Length()? != 4 {
            for index in 0..children.Length()? {
                let element = children.GetElement(index)?;
                eprintln!(
                    "unexpected child index={index} name={:?} type={:?}",
                    element.CurrentName()?,
                    element.CurrentControlType()?
                );
            }
        }
        assert_eq!(children.Length()?, 4);
        let button = children.GetElement(0)?;
        check_node(&button, BUTTON_NAME, UIA_ButtonControlTypeId)?;
        let button_bounds = button.CurrentBoundingRectangle()?;
        let root_bounds = root.CurrentBoundingRectangle()?;
        assert!(button_bounds.left > root_bounds.left && button_bounds.right < root_bounds.right);
        assert!(button_bounds.top > root_bounds.top && button_bounds.bottom < root_bounds.bottom);
        let disabled = children.GetElement(1)?;
        check_node(&disabled, DISABLED_NAME, UIA_ButtonControlTypeId)?;
        assert!(!disabled.CurrentIsEnabled()?.as_bool());
        let disabled_invoke: IUIAutomationInvokePattern =
            disabled.GetCurrentPatternAs(UIA_InvokePatternId)?;
        assert_eq!(
            disabled_invoke.Invoke().unwrap_err().code().0 as u32,
            UIA_E_ELEMENTNOTENABLED
        );
        let toggle = children.GetElement(2)?;
        check_node(&toggle, TOGGLE_NAME, UIA_CheckBoxControlTypeId)?;
        let list = children.GetElement(3)?;
        check_node(&list, LIST_NAME, UIA_ListControlTypeId)?;
        let rows = list.FindAll(TreeScope_Children, &condition)?;
        assert_eq!(rows.Length()?, 1);
        let row = rows.GetElement(0)?;
        check_node(&row, ITEM_NAME, UIA_ListItemControlTypeId)?;
        assert!(!row.CurrentIsOffscreen()?.as_bool());

        let run: IUIAutomationInvokePattern = button.GetCurrentPatternAs(UIA_InvokePatternId)?;
        let change: IUIAutomationTogglePattern = toggle.GetCurrentPatternAs(UIA_TogglePatternId)?;
        assert_eq!(change.CurrentToggleState()?, ToggleState_Off);
        run.Invoke()?;
        until(|| Ok(change.CurrentToggleState()? == ToggleState_On))?;
        change.Toggle()?;
        until(|| Ok(change.CurrentToggleState()? == ToggleState_Off))?;

        let remove: IUIAutomationInvokePattern = row.GetCurrentPatternAs(UIA_InvokePatternId)?;
        remove.Invoke()?;
        until(|| Ok(list.FindAll(TreeScope_Children, &condition)?.Length()? == 0))?;
        assert_eq!(
            row.CurrentName().unwrap_err().code().0 as u32,
            UIA_E_ELEMENTNOTAVAILABLE
        );
        assert_eq!(
            remove.Invoke().unwrap_err().code().0 as u32,
            UIA_E_ELEMENTNOTAVAILABLE
        );
    }
    println!(
        "client=passed names=6 control_types=6 button_invoke=true toggle=true list_item_invoke=true stale_rejected=true elapsed_ms={}",
        started.elapsed().as_millis()
    );
    Ok(())
}

fn check_node(element: &IUIAutomationElement, name: &str, kind: UIA_CONTROLTYPE_ID) -> Result<()> {
    // SAFETY: The caller established its COM apartment and passes a live UIA element.
    unsafe {
        assert_eq!(element.CurrentName()?.to_string(), name);
        assert_eq!(element.CurrentControlType()?, kind);
    }
    Ok(())
}

fn until(mut condition: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition()? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(Error::new(
        E_FAIL,
        "The UI Automation action was not observed",
    ))
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: This guard represents successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}
