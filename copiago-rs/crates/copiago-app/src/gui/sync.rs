use std::{collections::HashMap, io, time::Instant};

use copiago_core::{
    clip::{Clip, FileOffer},
    diagnostic,
    files::{self, Confirmation, OfferDecision, ReceivedFile, human_size, short_name},
    history::{Source, StoredClip},
    i18n::{Language, Message, message},
    settings::SyncKind,
};
use copiago_device::{DeviceCommand, FileProgress, FileTransferError, PasteboardError};
use copiago_ui::history::{HistoryAction, HistoryList, HistoryOffer, HistoryRowId, StatusTone};
use copiago_win::{
    clipboard::ClipboardError,
    image::{ImageError, decode_scaled, encode_png, png_for_device},
    tray::NotificationKind,
};

use crate::clipboard::{PhoneClip, PushQueueError, clip_kind};

use super::Host;

#[derive(Default)]
pub(super) struct FileUi {
    next_id: u64,
    waiting: Option<Offered>,
    active: Option<Transfer>,
}

struct Offered {
    id: u64,
    offer: FileOffer,
    reason: Message,
    auto: bool,
}

struct Transfer {
    id: u64,
    name: String,
    progress: Option<TransferProgress>,
}

struct TransferProgress {
    received: u64,
    total: Option<u64>,
}

impl TransferProgress {
    fn percent(&self) -> Option<u8> {
        self.total.map(|total| {
            self.received
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100)
                .min(100) as u8
        })
    }

    fn message(&self) -> Message {
        if let Some(total) = self.total {
            message!(
                "{received} / {total} · {percent}%",
                received = human_size(self.received),
                total = human_size(total),
                percent = self.percent().unwrap()
            )
        } else {
            message!(
                "已传 {received} · 总大小未知",
                received = human_size(self.received)
            )
        }
    }

    fn status(&self, name: &str) -> Message {
        message!(
            "正在同步「{name}」 · {progress}",
            name = short_name(name),
            progress = self.message()
        )
    }
}

impl FileUi {
    pub fn presentation(&self, language: Language) -> Option<HistoryOffer> {
        if let Some(active) = &self.active
            && let Some(progress) = &active.progress
        {
            return Some(HistoryOffer {
                id: active.id,
                name: active.name.clone(),
                metadata: active
                    .name
                    .rsplit_once('.')
                    .map_or(String::new(), |(_, suffix)| suffix.to_uppercase()),
                reason: progress.message(),
                busy: true,
                percent: progress.percent(),
            });
        }

        let waiting = self.waiting.as_ref()?;
        let size = size_message(waiting.offer.size).render(language);
        let extension = waiting
            .offer
            .name
            .rsplit_once('.')
            .map_or(String::new(), |(_, suffix)| suffix.to_uppercase());
        let metadata = if extension.is_empty() {
            size.into_owned()
        } else {
            format!("{extension} · {size}")
        };
        Some(HistoryOffer {
            id: waiting.id,
            name: waiting.offer.name.clone(),
            metadata,
            reason: waiting.reason.clone(),
            busy: false,
            percent: None,
        })
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn update_progress(&mut self, id: u64, report: FileProgress) -> Option<Message> {
        let active = self.active.as_mut().filter(|active| active.id == id)?;
        let progress = active.progress.as_mut()?;
        progress.received = report.received_bytes;
        progress.total = Some(report.total_bytes);
        Some(progress.status(&active.name))
    }

    pub fn clear(&mut self) {
        self.waiting = None;
        self.active = None;
    }
}

#[derive(Default)]
pub(super) struct PushTracker {
    next_id: u64,
    pending: HashMap<u64, (SyncKind, Option<HistoryRowId>)>,
}

impl Host<'_> {
    pub(super) fn refresh_entries(&mut self) {
        let settings = self.settings.settings();
        let (phone, pc) = match self
            .history
            .entries(Source::Phone)
            .and_then(|phone| self.history.entries(Source::Pc).map(|pc| (phone, pc)))
        {
            Ok(entries) => entries,
            Err(_) => {
                self.history_notice = Some((message!("无法读取历史"), StatusTone::Warning));
                (Vec::new(), Vec::new())
            }
        };
        self.history_page.set_entries(
            HistoryList {
                enabled: settings.history,
                entries: phone,
            },
            HistoryList {
                enabled: settings.pc_history,
                entries: pc,
            },
        );
    }

    pub(super) fn on_windows_copy(&mut self, now: Instant) {
        let record = self.settings.settings().pc_history;
        if !self
            .clipboard_sync
            .should_read_windows(self.clipboard.sequence_number(), record)
        {
            return;
        }
        let Some((clip, kind)) = self
            .clipboard
            .read()
            .ok()
            .and_then(|read| self.clipboard_sync.read_windows(read))
        else {
            return;
        };
        if record {
            self.record_clip(Source::Pc, &clip);
            if self.popup.visible() {
                self.refresh_entries();
                self.redraw(now);
            }
        }
        if self.clipboard_sync.auto_push() {
            self.push_clip(clip, kind, None);
        }
    }

    fn push_clip(&mut self, clip: Clip, kind: SyncKind, row: Option<HistoryRowId>) {
        self.pushes.next_id += 1;
        let id = self.pushes.next_id;
        match self
            .clipboard_sync
            .queue_push(clip, Some(id), row.is_some(), &self.worker.commands)
        {
            Ok(true) => {
                self.pushes.pending.insert(id, (kind, row));
                diagnostic::event(format_args!(
                    "event=push_queued kind={} manual={}",
                    kind.as_str(),
                    row.is_some()
                ));
            }
            Ok(false) => {}
            Err(error) => {
                let reason = match error {
                    PushQueueError::Full => "queue_full",
                    PushQueueError::Closed => "queue_closed",
                };
                diagnostic::event(format_args!("event=push_failed reason={reason}"));
                if row.is_some() {
                    let notice = match error {
                        PushQueueError::Full => message!("发送到 iPhone 失败"),
                        PushQueueError::Closed => message!("iPhone 未连接，无法发送"),
                    };
                    self.report_send(notice);
                }
            }
        }
    }

    pub(super) fn send_from_history(&mut self, row: HistoryRowId) {
        if !matches!(
            self.settings.connection().phase,
            copiago_ui::settings_page::ConnectionPhase::Connected { .. }
        ) {
            self.report_send(message!("iPhone 未连接，无法发送"));
            return;
        }
        let clip = match self.history.get(row.id) {
            Ok(Some(StoredClip::Text(text))) => Ok(Clip::Text(text)),
            Ok(Some(StoredClip::Image(bytes))) => png_for_device(bytes)
                .map(|bytes| Clip::Image {
                    bytes,
                    uti: "public.png".into(),
                })
                .map_err(|_| message!("发送到 iPhone 失败")),
            _ => Err(message!("无法读取这条历史")),
        };
        match clip {
            Ok(clip) => {
                let kind = clip_kind(&clip);
                self.push_clip(clip, kind, Some(row));
            }
            Err(notice) => self.report_send(notice),
        }
    }

    fn report_send(&mut self, message: Message) {
        if !self.popup.visible() {
            self.notify(&message, false, NotificationKind::Standard);
        }
        self.set_status(message, StatusTone::Warning);
    }

    pub(super) fn on_push_result(&mut self, id: Option<u64>, result: Result<(), PasteboardError>) {
        let Some((kind, row)) = id.and_then(|id| self.pushes.pending.remove(&id)) else {
            return;
        };
        match result {
            Ok(()) => {
                diagnostic::event(format_args!(
                    "event=push_complete kind={} manual={}",
                    kind.as_str(),
                    row.is_some()
                ));
                let notice = message!("已发送{kind}到 iPhone", kind = kind_name(kind));
                if let Some(id) = row {
                    self.history_page.sent(id);
                    self.feedback(HistoryAction::Send(id));
                    if !self.popup.visible() {
                        self.notify(&notice, false, NotificationKind::Standard);
                    }
                }
                self.set_status(notice, StatusTone::Success);
            }
            Err(error) => {
                if error == PasteboardError::PushBusy {
                    self.clipboard_sync.allow_resend();
                }
                diagnostic::event(format_args!("event=push_failed reason={error}"));
                let notice = message!("发送到 iPhone 失败");
                self.notify(&notice, true, NotificationKind::Standard);
                self.set_status(notice, StatusTone::Warning);
            }
        }
    }

    pub(super) fn on_phone_copy(&mut self, clip: Clip, stale: bool) {
        self.clear_waiting_offer();
        let bytes = payload_bytes(&clip);
        let outcome =
            self.clipboard_sync
                .receive_phone(clip, stale, self.clipboard.sequence_number());
        let notice = match outcome {
            PhoneClip::Ignore => return,
            PhoneClip::Hold(kind) => {
                let pending = self.clipboard_sync.pending().unwrap();
                let note = self.record_clip(Source::Phone, pending);
                diagnostic::event(format_args!(
                    "event=pull_held kind={} bytes={bytes} while_disconnected={stale}",
                    kind.as_str()
                ));
                if self.settings.settings().pull {
                    let notice = message!(
                        "iPhone 断线期间复制的{kind}未覆盖电脑剪贴板，可从托盘菜单使用{note}",
                        kind = kind_name(kind),
                        note = note
                    );
                    self.last_alert = None;
                    if !self.popup.visible() {
                        self.notify(&notice, false, NotificationKind::Standard);
                    }
                    notice
                } else {
                    message!(
                        "iPhone 上复制了新的{kind} · 未自动写入电脑，可在面板或托盘菜单中使用{note}",
                        kind = kind_name(kind),
                        note = note
                    )
                }
            }
            PhoneClip::Write(clip, kind) => {
                if let Err(error) = self.write_clip(&clip) {
                    let notice = clipboard_error(error);
                    diagnostic::event(format_args!(
                        "event=pull_failed kind={} bytes={bytes}",
                        kind.as_str()
                    ));
                    self.notify(&notice, true, NotificationKind::Standard);
                    self.set_status(notice, StatusTone::Warning);
                    return;
                }
                let note = self.record_clip(Source::Phone, &clip);
                diagnostic::event(format_args!(
                    "event=pull_complete kind={} bytes={bytes} while_disconnected={stale}",
                    kind.as_str()
                ));
                let notice = message!(
                    "已同步{kind} · 可直接粘贴{note}",
                    kind = kind_name(kind),
                    note = note
                );
                self.last_alert = None;
                if !self.popup.visible() {
                    self.notify(&notice, false, NotificationKind::Standard);
                }
                notice
            }
        };
        self.set_status(notice, StatusTone::Success);
        if self.popup.visible() {
            self.refresh_entries();
        }
    }

    pub(super) fn use_latest(&mut self) {
        let Some(clip) = self.clipboard_sync.take_pending() else {
            return;
        };
        let kind = clip_kind(&clip);
        match self.write_clip(&clip) {
            Ok(()) => {
                self.set_status(
                    message!(
                        "已使用 iPhone 上的{kind} · 可直接粘贴",
                        kind = kind_name(kind)
                    ),
                    StatusTone::Success,
                );
            }
            Err(error) => self.set_status(clipboard_error(error), StatusTone::Warning),
        }
    }

    pub(super) fn copy_from_history(&mut self, id: HistoryRowId) -> io::Result<()> {
        let clip = self.history.get(id.id)?.ok_or(io::ErrorKind::NotFound)?;
        match clip {
            StoredClip::Text(text) => self.write_clip(&Clip::Text(text)).map_err(io::Error::other),
            StoredClip::Image(bytes) => self
                .write_clip(&Clip::Image {
                    bytes,
                    uti: String::new(),
                })
                .map_err(io::Error::other),
            StoredClip::File { name, bytes } => {
                let received = files::restore_received(&self.data_dir, &name, &bytes)?;
                self.publish_file(&received).map_err(io::Error::other)?;
                received.keep();
                Ok(())
            }
        }
    }

    fn record_clip(&self, source: Source, clip: &Clip) -> Message {
        let settings = self.settings.settings();
        let record = match source {
            Source::Phone => settings.history,
            Source::Pc => settings.pc_history,
        };
        if record {
            history_note(self.history.add(source, clip, history_preview))
        } else {
            Message::new("")
        }
    }

    /// Writes text such as a diagnostic report. Marked as our own write, so it is
    /// neither synced to the iPhone nor recorded in the PC history.
    pub(super) fn write_local_text(&mut self, text: String) -> Result<(), ClipboardError> {
        self.write_clip(&Clip::Text(text))
    }

    fn write_clip(&mut self, clip: &Clip) -> Result<(), ClipboardError> {
        match clip {
            Clip::Text(text) => self.clipboard.write_text(text),
            Clip::Image { bytes, .. } => self.clipboard.write_image(bytes),
            Clip::File(_) => unreachable!("files use publish_file"),
        }?;
        self.clipboard_sync
            .mark_written(self.clipboard.sequence_number());
        Ok(())
    }

    fn publish_file(&mut self, received: &ReceivedFile) -> Result<(), ClipboardError> {
        self.clipboard
            .write_files(&[received.path().to_path_buf()])?;
        self.clipboard_sync
            .mark_written(self.clipboard.sequence_number());
        Ok(())
    }

    pub(super) fn on_file_offer(&mut self, offer: FileOffer) {
        self.clear_waiting_offer();
        let bytes = offer.size;
        let decision = self.clipboard_sync.decide_offer(bytes);
        let reason = match decision {
            OfferDecision::Ignore => return,
            OfferDecision::TooLarge => {
                diagnostic::event(format_args!(
                    "event=file_failed reason=too_large bytes={bytes}"
                ));
                self.set_status(
                    message!(
                        "「{name}」{size}，超过 512 MB 上限，未同步",
                        name = short_name(&offer.name),
                        size = size_message(bytes)
                    ),
                    StatusTone::Warning,
                );
                return;
            }
            OfferDecision::Receive => {
                if self.offer.active.is_none() {
                    self.fetch_file(offer, None);
                    return;
                }
                message!("同步中…")
            }
            OfferDecision::Confirm(Confirmation::AutoSyncOff) => {
                message!("已关闭自动同步，需要时点「同步到电脑」")
            }
            OfferDecision::Confirm(Confirmation::OverThreshold) => {
                message!(
                    "超过 {limit} MB，要同步到电脑吗？",
                    limit = self.settings.settings().file_ask_mb
                )
            }
            OfferDecision::Confirm(Confirmation::UnknownSize) => {
                message!("大小未知，要同步到电脑吗？")
            }
        };
        let name = short_name(&offer.name);
        let size = size_message(bytes);
        if !self.popup.visible() && self.settings.settings().pull {
            self.notify(
                &message!(
                    "iPhone 上复制了「{name}」（{size}）。打开剪贴板面板选择是否同步",
                    name = &name,
                    size = size.clone()
                ),
                false,
                NotificationKind::Standard,
            );
        }
        diagnostic::event(format_args!("event=file_waiting bytes={bytes}"));
        self.set_status(
            message!(
                "收到文件「{name}」{size}，等你选择是否同步",
                name = name,
                size = size
            ),
            StatusTone::Muted,
        );
        self.offer.waiting = Some(Offered {
            id: self.offer.allocate_id(),
            offer,
            reason,
            auto: decision == OfferDecision::Receive,
        });
    }

    pub(super) fn answer_file_offer(&mut self, id: u64, accept: bool) {
        if self
            .offer
            .waiting
            .as_ref()
            .is_none_or(|waiting| waiting.id != id)
        {
            return;
        }
        if accept && self.offer.active.is_some() {
            return;
        }
        let waiting = self.offer.waiting.take().unwrap();
        if accept {
            self.fetch_file(waiting.offer, Some(waiting.id));
        } else {
            self.set_status(
                message!("已忽略「{name}」", name = short_name(&waiting.offer.name)),
                StatusTone::Muted,
            );
            diagnostic::event(format_args!("event=file_ignored"));
        }
    }

    pub(super) fn clear_waiting_offer(&mut self) {
        self.offer.waiting = None;
    }

    fn fetch_file(&mut self, offer: FileOffer, id: Option<u64>) {
        let id = id.unwrap_or_else(|| self.offer.allocate_id());
        let name = offer.name.clone();
        let short = short_name(&name);
        let bytes = offer.size;
        let progress = offer.data.is_none().then_some(TransferProgress {
            received: 0,
            total: (bytes != 0).then_some(bytes),
        });
        if self
            .worker
            .commands
            .try_send(DeviceCommand::FetchOffer(offer, id))
            .is_err()
        {
            let notice = message!("iPhone 未连接，无法同步文件");
            self.notify(&notice, true, NotificationKind::Standard);
            self.set_status(notice, StatusTone::Warning);
            diagnostic::event(format_args!("event=file_failed reason=queue"));
            return;
        }
        let notice = if let Some(progress) = &progress {
            progress.status(&name)
        } else if bytes == 0 {
            message!("正在同步「{name}」…", name = short)
        } else {
            message!(
                "正在同步「{name}」{size}…",
                name = short,
                size = human_size(bytes)
            )
        };
        self.offer.active = Some(Transfer { id, name, progress });
        self.set_status(notice, StatusTone::Muted);
        diagnostic::event(format_args!("event=file_requested bytes={bytes}"));
    }

    pub(super) fn on_file_progress(&mut self, id: u64, report: FileProgress) {
        if let Some(notice) = self.offer.update_progress(id, report) {
            self.set_status(notice, StatusTone::Muted);
        }
    }

    pub(super) fn on_file_result(
        &mut self,
        id: u64,
        result: Result<ReceivedFile, FileTransferError>,
    ) {
        let Some(active) = self.offer.active.take_if(|active| active.id == id) else {
            return;
        };
        let name = short_name(&active.name);
        let result = match result {
            Ok(received) => {
                let bytes = received.bytes();
                if self.publish_file(&received).is_ok() {
                    let settings = self.settings.settings();
                    let note = if settings.history && settings.pull_types.contains(&SyncKind::File)
                    {
                        history_note(self.history.add_received(active.name, &received))
                    } else {
                        Message::new("")
                    };
                    received.keep();
                    let notice = message!(
                        "已同步「{name}」· 可直接粘贴{note}",
                        name = &name,
                        note = note
                    );
                    if !self.popup.visible() {
                        self.notify(&notice, false, NotificationKind::Standard);
                    }
                    self.set_status(notice, StatusTone::Success);
                    if self.popup.visible() {
                        self.refresh_entries();
                    }
                    diagnostic::event(format_args!("event=file_complete bytes={bytes}"));
                    Ok(())
                } else {
                    Err(message!("无法把「{name}」放进电脑剪贴板", name = &name))
                }
            }
            Err(error) => {
                diagnostic::event(format_args!("event=file_failed reason={error}"));
                Err(match error {
                    FileTransferError::OfferExpired => {
                        message!("手机上已复制了别的内容，这个文件不再可用")
                    }
                    FileTransferError::TooLarge => {
                        message!("「{name}」太大，未能同步", name = &name)
                    }
                    FileTransferError::Disconnected => message!("iPhone 未连接，无法同步文件"),
                    _ => message!("同步「{name}」失败", name = &name),
                })
            }
        };
        if let Err(notice) = result {
            self.notify(&notice, true, NotificationKind::Standard);
            self.set_status(notice, StatusTone::Warning);
        }
        if self
            .offer
            .waiting
            .as_ref()
            .is_some_and(|waiting| waiting.auto)
        {
            let offer = self.offer.waiting.take().unwrap().offer;
            // Re-evaluate the live settings before handling a deferred copy.
            self.on_file_offer(offer);
        }
    }
}

/// Forget interrupts the former session even when removing a record fails.
pub(super) fn retire_forgotten_session(
    clipboard: &mut crate::clipboard::ClipboardSync,
    offer: &mut FileUi,
    pushes: &mut PushTracker,
    success: bool,
) {
    offer.clear();
    pushes.pending.clear();
    clipboard.allow_resend();
    if success {
        clipboard.forget();
    }
}

fn payload_bytes(clip: &Clip) -> usize {
    match clip {
        Clip::Text(text) => text.len(),
        Clip::Image { bytes, .. } => bytes.len(),
        Clip::File(_) => 0,
    }
}

fn kind_name(kind: SyncKind) -> Message {
    Message::new(match kind {
        SyncKind::Text => "文本",
        SyncKind::Image => "图片",
        SyncKind::File => "文件",
    })
}

fn size_message(bytes: u64) -> Message {
    if bytes == 0 {
        message!("大小未知")
    } else {
        message!("{size}", size = human_size(bytes))
    }
}

fn history_note(result: io::Result<bool>) -> Message {
    Message::new(match result {
        Ok(true) => "",
        Ok(false) => " · 项目过大，未加入历史",
        Err(_) => " · 历史保存失败",
    })
}

fn clipboard_error(error: ClipboardError) -> Message {
    Message::new(match error {
        ClipboardError::ImageTooLarge => "图片过大，未同步到电脑",
        _ => "写入 Windows 剪贴板失败",
    })
}

/// PNG thumbnail for a history entry. An image too large to shrink within the
/// memory budget (e.g. a big progressive JPEG without an embedded preview) is
/// still saved, with an empty preview: the popup shows the generic image icon
/// (as does Python 1.2.0, which reads the same database), and the picture can
/// still be copied again from history.
fn history_preview(bytes: &[u8], width: u32, height: u32) -> io::Result<Vec<u8>> {
    match decode_scaled(bytes, width, height) {
        Ok(picture) => encode_png(&picture).map_err(io::Error::other),
        Err(ImageError::ImageTooLarge) => Ok(Vec::new()),
        Err(other) => Err(io::Error::other(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardSync;
    use copiago_core::settings::Settings;

    #[test]
    fn streaming_card_tracks_only_the_current_transfer() {
        let mut file = FileUi {
            active: Some(Transfer {
                id: 2,
                name: "sample.zip".into(),
                progress: Some(TransferProgress {
                    received: 0,
                    total: None,
                }),
            }),
            next_id: 2,
            ..FileUi::default()
        };
        let card = file.presentation(Language::ZhCn).unwrap();
        assert_eq!(card.percent, None);
        assert_eq!(card.reason.render(Language::ZhCn), "已传 0 B · 总大小未知");
        let report = FileProgress {
            received_bytes: 40,
            total_bytes: 100,
        };
        assert!(file.update_progress(1, report).is_none());
        assert_eq!(file.presentation(Language::En).unwrap().percent, None);
        assert!(file.update_progress(2, report).is_some());
        let card = file.presentation(Language::En).unwrap();
        assert_eq!(card.id, 2);
        assert_eq!(card.percent, Some(40));
        assert_eq!(card.reason.render(Language::En), "40 B / 100 B · 40%");
        file.clear();
        assert!(file.update_progress(2, report).is_none());
        assert!(file.presentation(Language::En).is_none());
    }

    #[test]
    fn forget_failure_retires_transfers_without_discarding_held_copy() {
        let mut clipboard = ClipboardSync::new(
            Settings {
                pull: false,
                ..Settings::default()
            },
            10,
        );
        let clip = Clip::Text("held test copy".into());
        assert!(matches!(
            clipboard.receive_phone(clip.clone(), true, 10),
            PhoneClip::Hold(_)
        ));

        let mut file = FileUi {
            waiting: Some(Offered {
                id: 2,
                offer: FileOffer {
                    name: "replacement.bin".into(),
                    uti: String::new(),
                    size: 1,
                    change: None,
                    data: None,
                },
                reason: message!("大小未知，要同步到电脑吗？"),
                auto: false,
            }),
            active: Some(Transfer {
                id: 1,
                name: "canceled.bin".into(),
                progress: Some(TransferProgress {
                    received: 0,
                    total: Some(1),
                }),
            }),
            next_id: 2,
        };
        let mut pushes = PushTracker {
            next_id: 8,
            pending: HashMap::from([(8, (SyncKind::Text, None))]),
        };

        retire_forgotten_session(&mut clipboard, &mut file, &mut pushes, false);
        assert!(file.active.is_none() && file.waiting.is_none());
        assert!(pushes.pending.is_empty());
        assert!(clipboard.pending().is_some_and(|pending| pending == &clip));
        assert_eq!(file.next_id, 2);
        assert_eq!(pushes.next_id, 8);

        retire_forgotten_session(&mut clipboard, &mut file, &mut pushes, true);
        assert!(!clipboard.has_pending());
    }
}
