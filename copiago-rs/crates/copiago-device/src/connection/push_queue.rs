use std::{cell::RefCell, collections::VecDeque, mem::size_of};

use copiago_core::clip::Clip;
use tokio::sync::Notify;

use crate::PasteboardError;

/// Session-local FIFO for copies waiting behind active pasteboard I/O.
pub(super) struct PushQueue {
    limit: usize,
    pending: RefCell<Pending>,
    incoming: Notify,
}

#[derive(Default)]
struct Pending {
    clips: VecDeque<Clip>,
    bytes: usize,
}

impl PushQueue {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            pending: RefCell::default(),
            incoming: Notify::new(),
        }
    }

    pub(super) fn send(&self, clip: Clip) -> Result<(), PasteboardError> {
        let bytes = charge(&clip);
        let mut pending = self.pending.borrow_mut();
        if bytes > self.limit - pending.bytes {
            return Err(PasteboardError::PushBusy);
        }
        pending.clips.push_back(clip);
        pending.bytes += bytes;
        self.incoming.notify_one();
        Ok(())
    }

    pub(super) async fn recv(&self) -> Clip {
        loop {
            if let Some(clip) = self.pop() {
                return clip;
            }
            self.incoming.notified().await;
        }
    }

    fn pop(&self) -> Option<Clip> {
        let mut pending = self.pending.borrow_mut();
        let clip = pending.clips.pop_front()?;
        // The active SET owns this item, so its storage is no longer queued.
        pending.bytes -= charge(&clip);
        Some(clip)
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
        let queue = PushQueue::new(4096);
        let mut bytes = Vec::with_capacity(3500);
        bytes.push(1);
        let image = Clip::Image {
            bytes,
            uti: "public.png".into(),
        };
        queue.send(image).unwrap();
        assert_eq!(
            queue.send(Clip::Text("next".into())),
            Err(PasteboardError::PushBusy),
        );
        assert!(matches!(queue.recv().await, Clip::Image { bytes, .. } if bytes == [1]));
        queue.send(Clip::Text("next".into())).unwrap();
        assert!(matches!(queue.recv().await, Clip::Text(text) if text == "next"));
    }
}
