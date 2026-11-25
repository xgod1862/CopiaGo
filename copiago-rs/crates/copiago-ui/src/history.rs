//! History data and actions for the popup. The application owns persistence,
//! clipboard writes, and device commands.

mod text;
mod view;

pub use view::{HistoryControl, HistoryLayout, HistoryView, ThumbnailPixels, ThumbnailRequest};

use std::time::{Duration, Instant};

use copiago_core::{
    history::{Entry, Source},
    i18n::{Language, Message},
};

use crate::components::{ProgressState, Status};

const CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);

/// Newest-first entries from one source. Disabling recording keeps old entries visible.
#[derive(Default)]
pub struct HistoryList {
    pub enabled: bool,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryRowId {
    pub source: Source,
    pub id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryAction {
    Copy(HistoryRowId),
    Send(HistoryRowId),
    Delete(HistoryRowId),
    Clear(Source),
    Enable(Source),
    RespondToOffer { id: u64, accept: bool },
    Retry,
    OpenSettings,
    OpenUpdate,
}

/// A target can change the selection or tab locally, or produce an application action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryTarget {
    Row(HistoryRowId),
    Tab(Source),
    Action(HistoryAction),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StatusTone {
    #[default]
    Muted,
    Success,
    Warning,
}

#[derive(Clone, PartialEq)]
pub struct HistoryStatus {
    pub connection: Status,
    pub device_name: String,
    pub detail: Option<Message>,
    pub retry_hint: Option<Message>,
    pub tone: StatusTone,
    pub progress: ProgressState,
    pub can_retry: bool,
    pub copied: bool,
}

impl Default for HistoryStatus {
    fn default() -> Self {
        Self {
            connection: Status::Connecting,
            device_name: String::new(),
            detail: None,
            retry_hint: None,
            tone: StatusTone::Muted,
            progress: ProgressState::Hidden,
            can_retry: false,
            copied: false,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct HistoryOffer {
    /// Assigned by the host when a new offer arrives. Keep it the same while receiving.
    pub id: u64,
    pub name: String,
    pub metadata: String,
    pub reason: Message,
    pub busy: bool,
    /// A missing percentage on an active transfer means its total is unknown.
    pub percent: Option<u8>,
}

#[derive(Default)]
struct List {
    enabled: bool,
    records: Vec<Record>,
}

struct Record {
    entry: Entry,
    revision: u64,
}

/// History state independent of a Composition scene, held only by the UI thread.
pub struct HistoryPage {
    language: Language,
    phone: List,
    pc: List,
    tab: Source,
    selected: usize,
    user_selected: bool,
    revision: u64,
    clear_until: Option<Instant>,
    copied: Option<(HistoryRowId, u64)>,
    sent: Option<(HistoryRowId, u64)>,
    status: HistoryStatus,
    offer: Option<HistoryOffer>,
    update: Option<String>,
}

impl HistoryPage {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            phone: List::default(),
            pc: List::default(),
            tab: Source::Phone,
            selected: 0,
            user_selected: false,
            revision: 0,
            clear_until: None,
            copied: None,
            sent: None,
            status: HistoryStatus::default(),
            offer: None,
            update: None,
        }
    }

    /// Reset when opening from the tray. Retain the last tab like the Python popup.
    pub fn reset_for_show(&mut self) {
        self.selected = 0;
        self.user_selected = false;
        self.clear_until = None;
        self.copied = None;
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
    }

    pub fn set_status(&mut self, status: HistoryStatus) {
        self.status = status;
    }

    pub fn set_offer(&mut self, offer: Option<HistoryOffer>) {
        if let Some(offer) = &offer
            && !offer.busy
            && self.offer.as_ref().is_none_or(|prior| prior.id != offer.id)
        {
            self.switch_tab(Source::Phone);
        }
        self.offer = offer;
    }

    pub fn set_update(&mut self, version: Option<String>) {
        self.update = version;
    }

    pub fn set_entries(&mut self, phone: HistoryList, pc: HistoryList) {
        let selected = self
            .list(self.tab)
            .records
            .get(self.selected)
            .map(|record| record.revision);
        replace(&mut self.phone, phone, &mut self.revision);
        replace(&mut self.pc, pc, &mut self.revision);
        let active = &self.list(self.tab).records;
        let count = active.len();
        self.selected = if self.user_selected {
            active
                .iter()
                .position(|record| Some(record.revision) == selected)
                .unwrap_or_else(|| self.selected.min(count.saturating_sub(1)))
        } else {
            0
        };
        if count == 0 {
            self.clear_until = None;
        }
        self.copied = self.copied.filter(|&(id, revision)| {
            self.record(id)
                .is_some_and(|item| item.revision == revision)
        });
        self.sent = self.sent.filter(|&(id, revision)| {
            self.record(id)
                .is_some_and(|item| item.revision == revision)
        });
    }

    pub fn tab(&self) -> Source {
        self.tab
    }

    pub fn selected(&self) -> Option<HistoryRowId> {
        self.list(self.tab)
            .records
            .get(self.selected)
            .map(|record| self.row_id(record))
    }

    pub fn move_selection(&mut self, step: isize) {
        let count = self.list(self.tab).records.len();
        if count != 0 {
            self.selected = self.selected.saturating_add_signed(step).min(count - 1);
            self.user_selected = true;
        }
    }

    pub fn clear_deadline(&self) -> Option<Instant> {
        self.clear_until
    }

    /// Call on the deadline and render again when this returns true.
    pub fn cancel_confirmation(&mut self) {
        self.clear_until = None;
    }

    pub fn expire_confirmation(&mut self, now: Instant) -> bool {
        if self.clear_until.is_some_and(|deadline| now >= deadline) {
            self.clear_until = None;
            return true;
        }
        false
    }

    pub fn copied(&mut self, id: HistoryRowId) {
        self.copied = self.record(id).map(|item| (id, item.revision));
    }

    pub fn sent(&mut self, id: HistoryRowId) {
        self.sent = self.record(id).map(|item| (id, item.revision));
    }

    /// A row click selects it. A double click or its copy button copies it.
    pub fn activate(&mut self, target: HistoryTarget, now: Instant) -> Option<HistoryAction> {
        match target {
            HistoryTarget::Tab(source) => self.switch_tab(source),
            HistoryTarget::Row(id) if id.source == self.tab => {
                if let Some(index) = self.index(id) {
                    self.selected = index;
                    self.user_selected = true;
                }
            }
            HistoryTarget::Action(HistoryAction::Clear(source))
                if source == self.tab && self.selected().is_some() =>
            {
                if self.clear_until.take().is_some_and(|limit| now < limit) {
                    return Some(HistoryAction::Clear(source));
                }
                self.clear_until = Some(now + CONFIRM_TIMEOUT);
            }
            HistoryTarget::Action(action) if self.can_activate(action) => return Some(action),
            _ => {}
        }
        None
    }

    fn can_activate(&self, action: HistoryAction) -> bool {
        match action {
            HistoryAction::Copy(id) | HistoryAction::Delete(id) => {
                id.source == self.tab && self.record(id).is_some()
            }
            HistoryAction::Send(id) => {
                id.source == Source::Pc && self.tab == Source::Pc && self.record(id).is_some()
            }
            HistoryAction::Enable(source) => {
                let list = self.list(source);
                source == self.tab && !list.enabled && list.records.is_empty()
            }
            HistoryAction::RespondToOffer { id, .. } => self
                .current_offer()
                .is_some_and(|offer| offer.id == id && !offer.busy),
            HistoryAction::Retry => self.status.can_retry,
            HistoryAction::OpenSettings => true,
            HistoryAction::OpenUpdate => self.update.is_some(),
            HistoryAction::Clear(_) => false,
        }
    }

    fn switch_tab(&mut self, tab: Source) {
        if self.tab != tab {
            self.tab = tab;
            self.selected = 0;
            self.user_selected = false;
            self.clear_until = None;
        }
    }

    fn list(&self, source: Source) -> &List {
        match source {
            Source::Phone => &self.phone,
            Source::Pc => &self.pc,
        }
    }

    fn row_id(&self, record: &Record) -> HistoryRowId {
        HistoryRowId {
            source: self.tab,
            id: record.entry.id,
        }
    }

    fn index(&self, id: HistoryRowId) -> Option<usize> {
        self.list(id.source)
            .records
            .iter()
            .position(|record| record.entry.id == id.id)
    }

    fn record(&self, id: HistoryRowId) -> Option<&Record> {
        self.index(id)
            .map(|index| &self.list(id.source).records[index])
    }

    fn current_offer(&self) -> Option<&HistoryOffer> {
        self.offer.as_ref().filter(|_| self.tab == Source::Phone)
    }
}

fn replace(list: &mut List, snapshot: HistoryList, revision: &mut u64) {
    let previous = std::mem::take(&mut list.records);
    list.enabled = snapshot.enabled;
    list.records = snapshot
        .entries
        .into_iter()
        .map(|entry| {
            let revision = previous
                .iter()
                .find(|old| {
                    old.entry.id == entry.id
                        && old.entry.created == entry.created
                        && old.entry.kind == entry.kind
                        && old.entry.preview == entry.preview
                })
                .map_or_else(
                    || {
                        *revision += 1;
                        *revision
                    },
                    |old| old.revision,
                );
            Record { entry, revision }
        })
        .collect();
}

#[cfg(test)]
mod tests;
