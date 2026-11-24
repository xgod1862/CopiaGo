//! Apartment and dispatcher lifetime for the thread that pumps the popup messages.

use std::{marker::PhantomData, mem::size_of, rc::Rc};

use windows::{
    System::{DispatcherQueue, DispatcherQueueController},
    Win32::{
        System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions, RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize,
        },
        UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
            PeekMessageW, PostQuitMessage, QS_ALLINPUT, TranslateMessage, WM_QUIT,
        },
    },
    core::Result,
};
use windows_future::AsyncStatus;

/// The popup thread's WinRT apartment and DispatcherQueue.
///
/// Construct once on the UI thread. Its Win32 message loop must continue to
/// dispatch thread and window messages while the host is in use.
pub struct CompositionThread {
    controller: DispatcherQueueController,
    _apartment: Apartment,
}

impl CompositionThread {
    pub fn new() -> Result<Self> {
        // SAFETY: Initialization and its matching uninitialization run on this thread.
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
        let apartment = Apartment(PhantomData);

        // SAFETY: The options describe the already initialized current thread.
        let controller = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_NONE,
            })?
        };
        Ok(Self {
            controller,
            _apartment: apartment,
        })
    }

    pub fn dispatcher_queue(&self) -> Result<DispatcherQueue> {
        self.controller.DispatcherQueue()
    }
}

impl Drop for CompositionThread {
    fn drop(&mut self) {
        let Ok(shutdown) = self.controller.ShutdownQueueAsync() else {
            return;
        };
        // A current-thread DispatcherQueue needs the message pump until shutdown completes.
        let mut quit = None;
        let mut message = MSG::default();
        while shutdown.Status() == Ok(AsyncStatus::Started) {
            // SAFETY: The queue and message buffer belong to this thread.
            unsafe {
                if PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    if message.message == WM_QUIT {
                        quit = Some(message.wParam.0 as i32);
                    } else {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                } else {
                    MsgWaitForMultipleObjectsEx(None, 25, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                }
            }
        }
        if let Some(exit_code) = quit {
            // SAFETY: Restore the caller's WM_QUIT on the current thread.
            unsafe { PostQuitMessage(exit_code) };
        }
    }
}

struct Apartment(PhantomData<Rc<()>>);

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Each Apartment represents one successful RoInitialize on this thread.
        unsafe { RoUninitialize() };
    }
}
