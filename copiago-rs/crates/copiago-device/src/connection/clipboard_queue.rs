use std::{cell::RefCell, collections::VecDeque, mem::size_of};

use copiago_core::clip::{Clip, FileOffer};
use tokio::sync::Notify;

use crate::{DeviceCommand, PasteboardError};

/// Session-local FIFO for clipboard commands waiting behind active pasteboard I/O.
pub(super) struct ClipboardQueue {
    limit: usize,
    pending: RefCell<Pending>,
    incoming: Notify,
}

#[derive(Default)]
struct Pending {
    commands: VecDeque<DeviceCommand>,
    bytes: usize,
}

impl ClipboardQueue {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            pending: RefCell::default(),
            incoming: Notify::new(),
        }
    }

    pub(super) fn send_push(&self, clip: Clip, id: Option<u64>) -> Result<(), PasteboardError> {
        let bytes = charge(&clip);
        let mut pending = self.pending.borrow_mut();
        if bytes > self.limit - pending.bytes {
            return Err(PasteboardError::PushBusy);
        }
        pending.commands.push_back(DeviceCommand::Push(clip, id));
        pending.bytes += bytes;
        self.incoming.notify_one();
        Ok(())
    }

    // ActiveSession limits file commands to one outstanding request separately.
    pub(super) fn send_offer(&self, offer: FileOffer, id: u64) {
        self.pending
            .borrow_mut()
            .commands
            .push_back(DeviceCommand::FetchOffer(offer, id));
        self.incoming.notify_one();
    }

    pub(super) async fn recv(&self) -> DeviceCommand {
        loop {
            if let Some(command) = self.pop() {
                return command;
            }
            self.incoming.notified().await;
        }
    }

    fn pop(&self) -> Option<DeviceCommand> {
        let mut pending = self.pending.borrow_mut();
        let command = pending.commands.pop_front()?;
        if let DeviceCommand::Push(clip, _) = &command {
            // The active SET owns this item, so its storage is no longer queued.
            pending.bytes -= charge(clip);
        }
        Some(command)
    }
}

fn charge(clip: &Clip) -> usize {
    // Account for retained allocations. A minimum charge also limits queue
    // metadata growth when the host submits many tiny copies.
    let payload = match clip {
        Clip::Text(text) => text.capacity(),
        Clip::Image { bytes, uti } => bytes.capacity() + uti.capacity(),
        Clip::File(offer) => {
            offer.name.capacity()
                + offer.uti.capacity()
                + offer.data.as_ref().map_or(0, Vec::capacity)
                + offer
                    .change
                    .as_ref()
                    .and_then(|change| change.nonce.as_ref())
                    .map_or(0, Vec::capacity)
        }
    };
    (payload + size_of::<Clip>()).max(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enforces_waiting_byte_budget_and_releases_it_when_a_copy_starts() {
        let queue = ClipboardQueue::new(4096);
        let mut bytes = Vec::with_capacity(3500);
        bytes.push(1);
        let image = Clip::Image {
            bytes,
            uti: "public.png".into(),
        };
        queue.send_push(image, None).unwrap();
        assert_eq!(
            queue.send_push(Clip::Text("next".into()), None),
            Err(PasteboardError::PushBusy),
        );
        assert!(
            matches!(queue.recv().await, DeviceCommand::Push(Clip::Image { bytes, .. }, None) if bytes == [1])
        );
        queue.send_push(Clip::Text("next".into()), None).unwrap();
        assert!(
            matches!(queue.recv().await, DeviceCommand::Push(Clip::Text(text), None) if text == "next")
        );
    }
}
