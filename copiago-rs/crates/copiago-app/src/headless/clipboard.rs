//! Clipboard policy shared by the headless host and the later tray integration.

use copiago_core::{
    clip::Clip,
    settings::{Settings, SyncKind},
};
use copiago_device::DeviceCommand;
use copiago_win::clipboard::ClipboardRead;
use tokio::sync::mpsc;

pub(super) struct ClipboardSync {
    settings: Settings,
    connected: bool,
    observed_sequence: u32,
    written_sequence: u32,
    last_sent: Option<[u8; 32]>,
    // Retain an eligible phone copy for manual use by the future tray menu.
    pending_clip: Option<Clip>,
}

pub(super) enum PhoneClip {
    Ignore,
    Hold(SyncKind),
    Write(Clip, SyncKind),
}

pub(super) enum PushQueueError {
    Full,
    Closed,
}

impl ClipboardSync {
    pub(super) fn new(settings: Settings, sequence: u32) -> Self {
        Self {
            settings,
            connected: false,
            observed_sequence: sequence,
            written_sequence: sequence,
            last_sent: None,
            pending_clip: None,
        }
    }

    pub(super) fn connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    pub(super) fn should_read_windows(&mut self, sequence: u32) -> bool {
        if sequence == 0 || sequence == self.observed_sequence {
            return false;
        }
        self.observed_sequence = sequence;
        self.settings.push && self.connected && sequence != self.written_sequence
    }

    pub(super) fn queue_push(
        &mut self,
        read: ClipboardRead,
        commands: &mpsc::Sender<DeviceCommand>,
    ) -> Result<Option<SyncKind>, PushQueueError> {
        let (clip, kind) = match read {
            ClipboardRead::Text(text) => (Clip::Text(text), SyncKind::Text),
            ClipboardRead::Png(bytes) => (
                Clip::Image {
                    bytes,
                    uti: "public.png".to_owned(),
                },
                SyncKind::Image,
            ),
            ClipboardRead::Empty | ClipboardRead::Sensitive => return Ok(None),
        };
        if !self.settings.push_types.contains(&kind) {
            return Ok(None);
        }
        let fingerprint = clip.fingerprint();
        if self.last_sent == Some(fingerprint) {
            return Ok(None);
        }
        commands
            .try_send(DeviceCommand::Push(clip))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => PushQueueError::Full,
                mpsc::error::TrySendError::Closed(_) => PushQueueError::Closed,
            })?;
        self.last_sent = Some(fingerprint);
        Ok(Some(kind))
    }

    pub(super) fn allow_resend(&mut self) {
        self.last_sent = None;
    }

    pub(super) fn receive_phone(&mut self, clip: Clip, stale: bool, sequence: u32) -> PhoneClip {
        let kind = match clip {
            Clip::Text(_) => SyncKind::Text,
            Clip::Image { .. } => SyncKind::Image,
            Clip::File(_) => return PhoneClip::Ignore,
        };
        if !self.settings.pull_types.contains(&kind) {
            return PhoneClip::Ignore;
        }
        if !self.settings.pull || (stale && sequence != self.written_sequence) {
            self.pending_clip = Some(clip);
            return PhoneClip::Hold(kind);
        }
        drop(self.pending_clip.take());
        PhoneClip::Write(clip, kind)
    }

    pub(super) fn mark_written(&mut self, sequence: u32) {
        self.written_sequence = sequence;
    }

    pub(super) fn offers_files(&self) -> bool {
        self.settings.pull_types.contains(&SyncKind::File)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Clip {
        Clip::Text("示例 😀".to_owned())
    }

    #[test]
    fn writes_disconnected_copy_when_windows_clipboard_is_unchanged() {
        let mut sync = ClipboardSync::new(Settings::default(), 10);
        assert!(matches!(
            sync.receive_phone(text(), true, 10),
            PhoneClip::Write(Clip::Text(_), SyncKind::Text)
        ));
        sync.mark_written(11);
        assert!(matches!(
            sync.receive_phone(text(), true, 11),
            PhoneClip::Write(Clip::Text(_), SyncKind::Text)
        ));
    }

    #[test]
    fn holds_disconnected_copy_when_another_app_has_copied() {
        let mut sync = ClipboardSync::new(Settings::default(), 10);
        sync.mark_written(11);
        // Seeing a Windows notification must not change the last-write baseline.
        assert!(!sync.should_read_windows(12));
        assert!(matches!(
            sync.receive_phone(text(), true, 12),
            PhoneClip::Hold(SyncKind::Text)
        ));
        assert!(
            sync.pending_clip
                .as_ref()
                .is_some_and(|clip| clip == &text())
        );
        assert!(matches!(
            sync.receive_phone(text(), false, 12),
            PhoneClip::Write(Clip::Text(_), SyncKind::Text)
        ));
        assert!(sync.pending_clip.is_none());
    }

    #[test]
    fn holds_allowed_copies_when_pull_is_off_and_drops_excluded_types() {
        let settings = Settings {
            pull: false,
            pull_types: vec![SyncKind::Image],
            ..Settings::default()
        };
        let mut sync = ClipboardSync::new(settings, 10);
        let image = Clip::Image {
            bytes: vec![1, 2, 3],
            uti: "public.png".to_owned(),
        };
        assert!(matches!(
            sync.receive_phone(image.clone(), false, 10),
            PhoneClip::Hold(SyncKind::Image)
        ));
        assert!(matches!(
            sync.receive_phone(text(), false, 10),
            PhoneClip::Ignore
        ));
        assert!(
            sync.pending_clip
                .as_ref()
                .is_some_and(|clip| clip == &image)
        );
    }

    #[test]
    fn only_reads_new_windows_copies_when_push_is_enabled_and_connected() {
        let mut sync = ClipboardSync::new(
            Settings {
                push: true,
                ..Settings::default()
            },
            10,
        );
        assert!(!sync.should_read_windows(11));
        sync.connected(true);
        assert!(!sync.should_read_windows(11));
        assert!(!sync.should_read_windows(0));
        assert!(sync.should_read_windows(12));
        assert!(!sync.should_read_windows(12));
        sync.mark_written(13);
        assert!(!sync.should_read_windows(13));
        sync.settings.push = false;
        assert!(!sync.should_read_windows(14));
    }

    #[test]
    fn queues_allowed_nonsensitive_copies_without_repeating_a_payload() {
        let mut sync = ClipboardSync::new(
            Settings {
                push: true,
                push_types: vec![SyncKind::Image],
                ..Settings::default()
            },
            10,
        );
        let (send, mut receive) = mpsc::channel(1);
        assert!(matches!(
            sync.queue_push(ClipboardRead::Sensitive, &send),
            Ok(None)
        ));
        assert!(matches!(
            sync.queue_push(ClipboardRead::Text("example".to_owned()), &send),
            Ok(None)
        ));
        assert!(receive.try_recv().is_err());

        let bytes = vec![1, 2, 3];
        assert!(matches!(
            sync.queue_push(ClipboardRead::Png(bytes.clone()), &send),
            Ok(Some(SyncKind::Image))
        ));
        assert!(matches!(
            receive.try_recv(),
            Ok(DeviceCommand::Push(Clip::Image { bytes: png, uti }))
                if png == bytes && uti == "public.png"
        ));
        assert!(matches!(
            sync.queue_push(ClipboardRead::Png(bytes.clone()), &send),
            Ok(None)
        ));
        assert!(receive.try_recv().is_err());

        // A device queue rejection after submission must allow the next copy.
        sync.allow_resend();
        assert!(matches!(
            sync.queue_push(ClipboardRead::Png(bytes), &send),
            Ok(Some(SyncKind::Image))
        ));
    }
}
