//! Shared connection between the desktop message loop and the device thread.

use std::{
    io,
    ops::ControlFlow,
    path::PathBuf,
    sync::mpsc,
    thread::{self, JoinHandle},
};

use copiago_device::{DeviceCommand, DeviceEvent, DeviceRunOptions, HostSignal, run_device};
use tokio::sync::mpsc as tokio_mpsc;

use crate::window::WindowWaker;

pub(super) struct DeviceWorker {
    pub commands: tokio_mpsc::Sender<DeviceCommand>,
    pub signals: tokio_mpsc::Sender<HostSignal>,
    pub events: mpsc::Receiver<DeviceEvent>,
    thread: JoinHandle<io::Result<()>>,
}

impl DeviceWorker {
    pub fn start(
        data_dir: PathBuf,
        options: DeviceRunOptions,
        waker: WindowWaker,
    ) -> io::Result<Self> {
        let (commands, mut receive_commands) = tokio_mpsc::channel(4);
        let (signals, receive_signals) = tokio_mpsc::channel(2);
        let (send_events, events) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("copiago-device".to_owned())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|runtime| {
                        runtime.block_on(run_device(
                            &data_dir,
                            options,
                            &mut receive_commands,
                            receive_signals,
                            |event| {
                                if send_events.send(event).is_err() || !waker.device_event() {
                                    return ControlFlow::Break(());
                                }
                                ControlFlow::Continue(())
                            },
                        ));
                    });
                waker.worker_exited();
                result
            })?;
        Ok(Self {
            commands,
            signals,
            events,
            thread,
        })
    }

    pub fn shutdown(self) -> io::Result<()> {
        let _ = self.commands.try_send(DeviceCommand::Shutdown);
        drop(self.commands);
        drop(self.signals);
        drop(self.events);
        self.thread
            .join()
            .map_err(|_| io::Error::other("device thread panicked"))?
    }
}
