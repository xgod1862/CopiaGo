//! Clipboard sequence tracking and sync policy shared by both desktop modes.

use copiago_core::{
    clip::Clip,
    files::{OfferDecision, decide_offer},
    settings::{Settings, SyncKind},
};
use copiago_device::DeviceCommand;
use copiago_win::clipboard::ClipboardRead;
use tokio::sync::mpsc;

pub(super) struct ClipboardSync {
    settings: Settings,
    connected: bool,
    observed_sequence: u32,
    written_sequence: Option<u32>,
    last_sent: Option<[u8; 32]>,
    // Retain an eligible phone copy until it is superseded or used manually.
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
            written_sequence: None,
            last_sent: None,
            pending_clip: None,
        }
    }

    pub(super) fn connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    pub(super) fn update_settings(&mut self, settings: Settings, sequence: u32) {
        if self.settings.push != settings.push || self.settings.pc_history != settings.pc_history {
            self.observed_sequence = sequence;
        }
        self.settings = settings;
    }

    pub(super) fn auto_push(&self) -> bool {
        self.settings.push && self.connected
    }

    pub(super) fn should_read_windows(&mut self, sequence: u32, record: bool) -> bool {
        if sequence == 0 || sequence == self.observed_sequence {
            return false;
        }
        self.observed_sequence = sequence;
        (record || self.auto_push()) && Some(sequence) != self.written_sequence
    }

    pub(super) fn queue_push(
        &mut self,
        clip: Clip,
        id: Option<u64>,
        manual: bool,
        commands: &mpsc::Sender<DeviceCommand>,
    ) -> Result<bool, PushQueueError> {
        let fingerprint = clip.fingerprint();
        if !manual && self.last_sent == Some(fingerprint) {
            return Ok(false);
        }
        commands
            .try_send(DeviceCommand::Push(clip, id))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => PushQueueError::Full,
                mpsc::error::TrySendError::Closed(_) => PushQueueError::Closed,
            })?;
        self.last_sent = Some(fingerprint);
        Ok(true)
    }

    pub(super) fn read_windows(&self, read: ClipboardRead) -> Option<(Clip, SyncKind)> {
        let (clip, kind) = match read {
            ClipboardRead::Text(text) => (Clip::Text(text), SyncKind::Text),
            ClipboardRead::Png(bytes) => (
                Clip::Image {
                    bytes,
                    uti: "public.png".to_owned(),
                },
                SyncKind::Image,
            ),
            ClipboardRead::Empty | ClipboardRead::Sensitive => return None,
        };
        self.settings
            .push_types
            .contains(&kind)
            .then_some((clip, kind))
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending_clip.is_some()
    }

    pub(super) fn pending(&self) -> Option<&Clip> {
        self.pending_clip.as_ref()
    }

    pub(super) fn take_pending(&mut self) -> Option<Clip> {
        self.pending_clip.take()
    }

    pub(super) fn forget(&mut self) {
        self.connected = false;
        self.pending_clip = None;
        self.last_sent = None;
    }

    pub(super) fn allow_resend(&mut self) {
        self.last_sent = None;
    }

    pub(super) fn receive_phone(&mut self, clip: Clip, stale: bool, sequence: u32) -> PhoneClip {
        let kind = clip_kind(&clip);
        if kind == SyncKind::File {
            return PhoneClip::Ignore;
        }
        if !self.settings.pull_types.contains(&kind) {
            return PhoneClip::Ignore;
        }
        let changed_elsewhere = self
            .written_sequence
            .is_some_and(|written| sequence != written);
        if !self.settings.pull || (stale && changed_elsewhere) {
            self.pending_clip = Some(clip);
            return PhoneClip::Hold(kind);
        }
        drop(self.pending_clip.take());
        PhoneClip::Write(clip, kind)
    }

    pub(super) fn mark_written(&mut self, sequence: u32) {
        self.written_sequence = (sequence != 0).then_some(sequence);
    }

    pub(super) fn decide_offer(&self, size: u64) -> OfferDecision {
        decide_offer(&self.settings, size)
    }
}

pub(super) fn clip_kind(clip: &Clip) -> SyncKind {
    match clip {
        Clip::Text(_) => SyncKind::Text,
        Clip::Image { .. } => SyncKind::Image,
        Clip::File(_) => SyncKind::File,
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
        assert!(!sync.should_read_windows(12, false));
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
        assert!(!sync.should_read_windows(11, false));
        sync.connected(true);
        assert!(!sync.should_read_windows(11, false));
        assert!(!sync.should_read_windows(0, false));
        assert!(sync.should_read_windows(12, false));
        assert!(!sync.should_read_windows(12, false));
        sync.mark_written(13);
        assert!(!sync.should_read_windows(13, false));
        sync.settings.push = false;
        assert!(!sync.should_read_windows(14, false));
    }

    #[test]
    fn pc_history_records_new_external_copies_even_when_offline() {
        let mut sync = ClipboardSync::new(Settings::default(), 10);
        let settings = Settings {
            pc_history: true,
            ..Settings::default()
        };
        sync.update_settings(settings, 11);
        assert!(!sync.should_read_windows(11, true));
        assert!(sync.should_read_windows(12, true));
        sync.mark_written(13);
        assert!(!sync.should_read_windows(13, true));
        assert!(!sync.auto_push());
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
        assert!(sync.read_windows(ClipboardRead::Sensitive).is_none());
        assert!(
            sync.read_windows(ClipboardRead::Text("example".to_owned()))
                .is_none()
        );
        assert!(receive.try_recv().is_err());

        let bytes = vec![1, 2, 3];
        let (image, kind) = sync
            .read_windows(ClipboardRead::Png(bytes.clone()))
            .unwrap();
        assert_eq!(kind, SyncKind::Image);
        assert!(matches!(
            sync.queue_push(image.clone(), Some(7), false, &send),
            Ok(true)
        ));
        assert!(matches!(
            receive.try_recv(),
            Ok(DeviceCommand::Push(Clip::Image { bytes: png, uti }, Some(7)))
                if png == bytes && uti == "public.png"
        ));
        assert!(matches!(
            sync.queue_push(image.clone(), None, false, &send),
            Ok(false)
        ));
        assert!(receive.try_recv().is_err());

        // A manual send can resend the same image independently of automatic options.
        sync.settings.push_types = vec![SyncKind::Text];
        assert!(matches!(
            sync.queue_push(image.clone(), Some(8), true, &send),
            Ok(true)
        ));
        assert!(matches!(
            receive.try_recv(),
            Ok(DeviceCommand::Push(_, Some(8)))
        ));

        sync.allow_resend();
        assert!(matches!(
            sync.queue_push(image, None, false, &send),
            Ok(true)
        ));
    }
}
