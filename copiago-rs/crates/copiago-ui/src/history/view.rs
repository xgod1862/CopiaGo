mod chrome;

use std::time::Instant;

use chrono::{Local, NaiveDate};
use copiago_core::{history::Source, i18n::tr};
use windows::{
    UI::{
        Color,
        Composition::{ContainerVisual, SpriteVisual},
    },
    Win32::Foundation::E_INVALIDARG,
    core::{Error, Interface, Result},
};
use windows_numerics::Vector2;

use crate::{
    animation::{Animator, MotionPolicy},
    components::{
        Interaction, ItemAction, ItemActions, ItemContent, ItemHit, ItemKind, ItemLayout,
        LIGHT_THEME, ListItem, ProgressBar, ProgressState, Rect, Removal, ScrollLayout, ScrollView,
        Status, StatusPill, Tooltip, holder, xy,
    },
    composition::{Font, MAX_THUMBNAIL_SIDE, PopupBounds, PopupScene, TextMeasurer},
};

use super::text::{fit, human_size, short_time, wrap};
use super::{HistoryAction, HistoryPage, HistoryRowId, HistoryTarget, Record, StatusTone};
use chrome::{Chrome, ChromeState};

const WIDTH: f32 = 392.0;
const PAD: f32 = 18.0;
const INNER: f32 = WIDTH - 2.0 * PAD;
const HEADER: f32 = 64.0;
const LIST_HEIGHT: f32 = 356.0;
const TEXT_SIZE: f32 = ItemLayout::TITLE_SIZE;
const NOTE_SIZE: f32 = 12.0;
const PREVIEW_WIDTH: u32 = 88;
const PREVIEW_HEIGHT: u32 = 56;
const MAX_VISIBLE_THUMBNAILS: usize = 6;

/// Geometry measured before showing a popup, so the host can anchor its actual height.
/// Recompute after updating the page, then pass the same layout to the view.
pub struct HistoryLayout {
    measure: TextMeasurer,
    state: ChromeState,
    viewport: Rect,
    inline_height: f32,
    scroll_all: bool,
    rows: Vec<LayoutRow>,
    content_height: f32,
    today: NaiveDate,
    selected_title: Vec<String>,
}

struct LayoutRow {
    id: HistoryRowId,
    index: usize,
    revision: u64,
    origin: Vector2,
    item: ItemLayout,
}

impl HistoryPage {
    pub fn layout(&self) -> Result<HistoryLayout> {
        self.layout_with_limit(f32::INFINITY)
    }

    pub fn layout_with_limit(&self, available_height: f32) -> Result<HistoryLayout> {
        self.measure_layout(Local::now().date_naive(), available_height)
    }

    fn measure_layout(&self, today: NaiveDate, available_height: f32) -> Result<HistoryLayout> {
        let measure = TextMeasurer::new()?;
        let state = ChromeState::new(self, &measure)?;
        let progress = self.status.progress != ProgressState::Hidden;
        let tabs_top = HEADER
            + if state.detail.is_empty() {
                0.0
            } else {
                state.detail.len() as f32 * 18.0 + 6.0
            }
            + if state.copied { 22.0 } else { 0.0 }
            + if progress { 16.0 } else { 0.0 }
            + if state.activity.is_some() { 22.0 } else { 0.0 };
        // Move all fixed chrome into the same scroll view if even a single
        // usable row cannot fit between the header and footer.
        let footer = state.footer_height();
        let scroll_all = HEADER + 42.0 + footer + 64.0 > available_height;
        let (inline_height, viewport) = if scroll_all {
            (
                tabs_top + 42.0,
                Rect::new(0.0, 0.0, WIDTH, available_height.max(1.0)),
            )
        } else {
            let inline = if tabs_top + 42.0 + footer + 64.0 > available_height {
                tabs_top - HEADER
            } else {
                0.0
            };
            let top = tabs_top - inline + 42.0;
            (
                inline,
                Rect::new(
                    0.0,
                    top,
                    WIDTH,
                    (available_height - top - footer).min(LIST_HEIGHT),
                ),
            )
        };
        let mut y = inline_height
            + 4.0
            + if state.offer_height() > 0.0 {
                state.offer_height() + 8.0
            } else {
                0.0
            };
        let mut rows = Vec::new();
        let mut selected_title = Vec::new();
        for (index, record) in self.list(self.tab).records.iter().enumerate() {
            let selected = self.selected == index;
            let kind = kind(record);
            let actions = if selected {
                match self.tab {
                    Source::Phone => ItemActions::CopyDelete,
                    Source::Pc => ItemActions::CopySendDelete,
                }
            } else {
                ItemActions::None
            };
            let mut item = if kind == ItemKind::Image {
                ItemLayout::image(INNER, png_aspect(&record.entry.preview), actions)
            } else {
                ItemLayout::new(INNER, ItemLayout::ROW_HEIGHT, kind, actions)
            };
            if selected && matches!(kind, ItemKind::Text | ItemKind::Link) {
                selected_title = wrap(&preview_text(record), item.wrap_width(), 3, |line| {
                    measure.width(line, Font::Semibold, TEXT_SIZE)
                })?;
                item.height = ItemLayout::selected_text_height(selected_title.len());
            }
            rows.push(LayoutRow {
                id: self.row_id(record),
                index,
                revision: record.revision,
                origin: Vector2::new(PAD, y),
                item,
            });
            y += item.height + ItemLayout::GAP;
        }
        let rows_empty = rows.is_empty();
        Ok(HistoryLayout {
            measure,
            state,
            viewport,
            inline_height,
            scroll_all,
            rows,
            content_height: if rows_empty {
                y.max(inline_height + 258.0)
            } else {
                y
            } + if scroll_all { footer } else { 0.0 },
            today,
            selected_title,
        })
    }
}

impl HistoryLayout {
    pub fn bounds(&self) -> PopupBounds {
        PopupBounds::new(
            WIDTH,
            self.viewport.bottom()
                + if self.scroll_all {
                    0.0
                } else {
                    self.state.footer_height()
                },
            24.0,
        )
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    pub fn scrolls_feedback(&self) -> bool {
        self.inline_height > 0.0
    }

    fn feedback_bottom(&self) -> f32 {
        if self.scroll_all {
            self.tabs_top()
        } else if self.scrolls_feedback() {
            self.inline_height + 4.0
        } else {
            self.tabs_top()
        }
    }

    fn tabs_top(&self) -> f32 {
        if self.scroll_all {
            self.inline_height - 42.0
        } else {
            self.viewport.y - 42.0
        }
    }

    fn footer_top(&self) -> f32 {
        if self.scroll_all {
            self.content_height - self.state.footer_height()
        } else {
            self.viewport.bottom()
        }
    }
}

#[derive(PartialEq)]
struct RowPaint {
    revision: u64,
    selected: bool,
    language: copiago_core::i18n::Language,
    today: NaiveDate,
    copied: bool,
    sent: Option<super::SendMark>,
}

struct Row {
    id: HistoryRowId,
    visual: ListItem,
    paint: RowPaint,
    thumbnail: ThumbnailState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ThumbnailState {
    NotRequested,
    Loaded,
    Invalid,
}

/// What the host should decode for one visible image row.
pub struct ThumbnailRequest<'a> {
    pub id: HistoryRowId,
    /// The stored 88 × 56 PNG, when valid. Use it if the original cannot be read.
    pub preview: Option<&'a [u8]>,
    /// Pixel size of the image slot. Decode to fit inside it, keeping the aspect ratio.
    pub width: u32,
    pub height: u32,
}

/// Decoder result for a history image. Pixels are tightly packed top-down
/// straight-alpha BGRA, at most `MAX_THUMBNAIL_SIDE` on each edge.
pub struct ThumbnailPixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// One control in the active history tab, including offscreen rows.
pub struct HistoryControl {
    pub target: HistoryTarget,
    pub label: String,
    pub bounds: Option<Rect>,
}

/// Mounted history visuals. Drop when the host hides or replaces its scene.
/// The caller dispatches the domain actions returned by HistoryPage.
pub struct HistoryView {
    root: ContainerVisual,
    animator: Animator,
    chrome: Chrome,
    pill: StatusPill,
    progress: ProgressBar,
    scroll: ScrollView,
    fade: SpriteVisual,
    tooltip: Tooltip,
    language: copiago_core::i18n::Language,
    rows: Vec<Row>,
}

impl HistoryView {
    /// Mount into the scene's content using the same layout used to size the host.
    pub fn mount(
        scene: &PopupScene,
        page: &HistoryPage,
        layout: &HistoryLayout,
        policy: MotionPolicy,
        now: Instant,
    ) -> Result<Self> {
        let view = Self::new(scene, page, layout, policy, now)?;
        scene.content().Children()?.InsertAtTop(view.visual())?;
        Ok(view)
    }

    /// Build without attaching the visuals, for a page transition.
    pub fn new(
        scene: &PopupScene,
        page: &HistoryPage,
        layout: &HistoryLayout,
        policy: MotionPolicy,
        now: Instant,
    ) -> Result<Self> {
        let root = holder(scene, Rect::new(0.0, 0.0, WIDTH, layout.bounds().height))?;
        let animator = Animator::new(scene.compositor(), policy)?;
        let chrome = Chrome::new(scene, &animator, layout, layout.state.tab)?;
        let scroll = ScrollView::new(
            scene,
            ScrollLayout::new(layout.viewport, layout.content_height)?,
            &LIGHT_THEME,
        )?;
        scroll.content().Children()?.InsertAtTop(&chrome.scrolled)?;
        let fade = viewport_fade(scene, layout.viewport.height)?;
        scroll
            .visual()
            .Children()?
            .InsertAbove(&fade, scroll.content())?;
        let pill = StatusPill::new(
            scene,
            &animator,
            Vector2::new(WIDTH - PAD - 40.0, 20.0),
            pill_width(layout)?,
            page.status.connection,
            &pill_label(page),
            &LIGHT_THEME,
        )?;
        let progress = ProgressBar::new(
            scene,
            Vector2::new(PAD, layout.feedback_bottom() - 15.0),
            INNER,
            &LIGHT_THEME,
        )?;
        let children = root.Children()?;
        children.InsertAtTop(&chrome.frame)?;
        if layout.scroll_all {
            scroll.content().Children()?.InsertAtTop(pill.visual())?;
        } else {
            children.InsertAtTop(pill.visual())?;
        }
        if layout.scrolls_feedback() {
            scroll
                .content()
                .Children()?
                .InsertAtTop(progress.visual())?;
        } else {
            children.InsertAtTop(progress.visual())?;
        }
        children.InsertAtTop(scroll.visual())?;
        let tooltip = action_tooltip(scene, layout, page.language)?;
        children.InsertAtTop(tooltip.visual())?;
        let mut view = Self {
            root,
            animator,
            chrome,
            pill,
            progress,
            scroll,
            fade,
            tooltip,
            language: page.language,
            rows: Vec::new(),
        };
        view.refresh(scene, page, layout, now)?;
        Ok(view)
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.root
    }

    pub fn set_motion_policy(&mut self, policy: MotionPolicy) {
        self.animator.set_policy(policy);
    }

    /// Refresh in place while the height is unchanged. If the layout height
    /// changes, rebuild through PopupHost and mount a new view.
    pub fn refresh(
        &mut self,
        scene: &PopupScene,
        page: &HistoryPage,
        layout: &HistoryLayout,
        now: Instant,
    ) -> Result<()> {
        if layout.viewport != self.scroll.layout().viewport() {
            return Err(Error::new(
                E_INVALIDARG,
                "History layout needs a resized popup",
            ));
        }
        if page.language != self.language {
            let tooltip = action_tooltip(scene, layout, page.language)?;
            let children = self.root.Children()?;
            children.Remove(self.tooltip.visual())?;
            children.InsertAtTop(tooltip.visual())?;
            self.tooltip = tooltip;
            self.language = page.language;
        }
        let records = &page.list(page.tab).records;
        if layout.state.tab != page.tab
            || records.len() != layout.rows.len()
            || !records
                .iter()
                .zip(&layout.rows)
                .all(|(record, row)| record.revision == row.revision)
        {
            return Err(Error::new(E_INVALIDARG, "History layout is out of date"));
        }
        let tab_changed = self.chrome.state.tab != layout.state.tab;
        if self.chrome.inline_height != layout.inline_height {
            let parent = if self.chrome.inline_height > 0.0 {
                self.scroll.content()
            } else {
                &self.root
            };
            parent.Children()?.Remove(self.progress.visual())?;
            let parent = if layout.scrolls_feedback() {
                self.scroll.content()
            } else {
                &self.root
            };
            parent.Children()?.InsertAtTop(self.progress.visual())?;
            self.progress
                .visual()
                .SetOffset(xy(PAD, layout.feedback_bottom() - 15.0))?;
        }
        let same_frame = self.chrome.inline_height == layout.inline_height
            && self.chrome.footer_top == layout.footer_top()
            && self.chrome.state.same_frame(&layout.state);
        // Progress rewrites the detail several times a second; only a new message scrolls.
        let reveal_feedback = layout.scrolls_feedback()
            && !same_frame
            && self.chrome.state.detail != layout.state.detail;
        if same_frame {
            self.chrome.update_live(scene, &layout.state)?;
        } else if self.chrome.state != layout.state
            || self.chrome.inline_height != layout.inline_height
            || self.chrome.footer_top != layout.footer_top()
        {
            let next = Chrome::new(scene, &self.animator, layout, self.chrome.state.tab)?;
            crate::page_transition::crossfade(
                &self.animator,
                &self.root,
                &self.chrome.frame,
                &next.frame,
            )?;
            crate::page_transition::crossfade(
                &self.animator,
                self.scroll.content(),
                &self.chrome.scrolled,
                &next.scrolled,
            )?;
            self.chrome = next;
        }
        self.pill.set_status(
            scene,
            &self.animator,
            page.status.connection,
            &pill_label(page),
        )?;
        self.progress
            .set_state(&self.animator, page.status.progress)?;
        self.scroll.set_content_height(layout.content_height, now)?;
        if tab_changed || reveal_feedback {
            self.scroll.jump_to(0.0, now)?;
        } else if !self.animator.policy().allows_motion() {
            self.scroll.jump_to(self.scroll.target_offset(), now)?;
        }
        self.fade.SetIsVisible(self.scroll.layout().limit() > 0.0)?;
        self.refresh_rows(scene, page, layout, tab_changed, now)
    }

    fn refresh_rows(
        &mut self,
        scene: &PopupScene,
        page: &HistoryPage,
        layout: &HistoryLayout,
        tab_changed: bool,
        now: Instant,
    ) -> Result<()> {
        let mut previous = std::mem::take(&mut self.rows);
        let animate_insert = !tab_changed && !previous.is_empty();
        let children = self.scroll.content().Children()?;
        let removal = if previous.len() == layout.rows.len()
            && layout
                .rows
                .iter()
                .any(|placed| !previous.iter().any(|row| row.id == placed.id))
        {
            Removal::Fade
        } else {
            Removal::Collapse
        };
        for placed in &layout.rows {
            let record = &page.list(page.tab).records[placed.index];
            let paint = RowPaint {
                revision: record.revision,
                selected: placed.index == page.selected,
                language: page.language,
                today: layout.today,
                copied: page.copied == Some((placed.id, record.revision)),
                sent: page.send_mark(placed.id, record.revision),
            };
            let old = previous
                .iter()
                .position(|row| row.id == placed.id)
                .map(|index| previous.swap_remove(index));
            let mut row = match old {
                Some(row) if row.paint == paint => row,
                mut old => {
                    // Selection repaints a row. Carry its decoded picture over instead of
                    // decoding the original again.
                    let reused = match &mut old {
                        Some(old)
                            if old.thumbnail == ThumbnailState::Loaded
                                && old.paint.revision == paint.revision
                                && old.visual.layout().tile().size()
                                    == placed.item.tile().size() =>
                        {
                            old.visual.take_thumbnail()?
                        }
                        _ => None,
                    };
                    let origin = if let Some(old) = &old {
                        children.Remove(old.visual.visual())?;
                        old.visual.bounds_at(now).origin()
                    } else {
                        placed.origin
                    };
                    let mut visual = placed.paint(scene, &paint, record, layout, origin)?;
                    if let Some(above) = self.rows.last() {
                        children.InsertBelow(visual.visual(), above.visual.visual())?;
                    } else {
                        children.InsertBelow(visual.visual(), &self.chrome.scrolled)?;
                    }
                    if old.is_none() && animate_insert {
                        visual.animate_in(&self.animator, now)?;
                    }
                    let thumbnail = match &reused {
                        Some(picture) => {
                            visual.adopt_thumbnail(picture)?;
                            ThumbnailState::Loaded
                        }
                        None => ThumbnailState::NotRequested,
                    };
                    Row {
                        id: placed.id,
                        visual,
                        paint,
                        thumbnail,
                    }
                }
            };
            row.visual.move_to(&self.animator, placed.origin, now)?;
            self.rows.push(row);
        }
        for row in previous {
            // Do not keep thumbnails alive during a removal animation.
            let mut visual = row.visual;
            if row.thumbnail == ThumbnailState::Loaded {
                visual.set_thumbnail(&self.animator, None)?;
            }
            if tab_changed {
                children.Remove(visual.visual())?;
            } else {
                children.Remove(visual.visual())?;
                children.InsertAtBottom(visual.visual())?;
                visual.remove(&self.animator, self.scroll.content(), removal)?;
            }
        }
        Ok(())
    }

    /// Input uses visual positions during reflow, and clipped buttons are inert.
    pub fn hit(&self, point: Vector2, now: Instant) -> Option<HistoryTarget> {
        self.chrome.hit(point, &self.scroll, now).or_else(|| {
            self.rows.iter().find_map(|row| {
                self.scroll
                    .hit_item(point, &row.visual, now)
                    .map(|hit| match hit {
                        ItemHit::Body => HistoryTarget::Row(row.id),
                        ItemHit::Action(action) => {
                            HistoryTarget::Action(row_action(row.id, action))
                        }
                    })
            })
        })
    }

    pub fn set_interaction(
        &mut self,
        hovered: Option<HistoryTarget>,
        pressed: Option<HistoryTarget>,
        now: Instant,
    ) -> Result<()> {
        let state = |target| match (hovered == Some(target), pressed == Some(target)) {
            (true, true) => Interaction::Pressed,
            (true, false) => Interaction::Hovered,
            _ => Interaction::Idle,
        };
        self.chrome.interact(&self.animator, state)?;
        for row in &mut self.rows {
            row.visual
                .set_interaction(&self.animator, state(HistoryTarget::Row(row.id)))?;
            let active = [ItemAction::Copy, ItemAction::Send, ItemAction::Delete]
                .into_iter()
                .find_map(|action| {
                    let target = HistoryTarget::Action(row_action(row.id, action));
                    (hovered == Some(target)).then(|| (action, state(target)))
                });
            row.visual.set_action_interaction(&self.animator, active)?;
        }
        // Name the icon-only row action under a resting pointer. A press hides it.
        let layout = self.scroll.layout();
        let offset = self.scroll.offset_at(now);
        let tip = match hovered {
            Some(HistoryTarget::Action(action)) if pressed.is_none() => {
                self.rows.iter().find_map(|row| {
                    let (key, action) = match action {
                        HistoryAction::Copy(id) if id == row.id => (0, ItemAction::Copy),
                        HistoryAction::Send(id) if id == row.id => (1, ItemAction::Send),
                        HistoryAction::Delete(id) if id == row.id => (2, ItemAction::Delete),
                        _ => return None,
                    };
                    let bounds = row.visual.action_bounds_at(action, now)?;
                    Some((key, layout.visible_bounds(bounds, offset)?))
                })
            }
            _ => None,
        };
        self.tooltip.set(&self.animator, tip)?;
        Ok(())
    }

    pub fn scroll_wheel(&mut self, delta: i16, now: Instant) -> Result<()> {
        self.tooltip.set(&self.animator, None)?;
        self.scroll.scroll_wheel(&self.animator, delta, now)
    }

    pub fn scroll_offset(&self) -> f32 {
        self.scroll.target_offset()
    }

    pub fn restore_scroll(&mut self, offset: f32, now: Instant) -> Result<()> {
        self.scroll.jump_to(offset, now)
    }

    pub fn controls(&self, page: &HistoryPage, now: Instant) -> Vec<HistoryControl> {
        let mut controls = self.chrome.controls(&self.scroll, now);
        let layout = self.scroll.layout();
        let offset = self.scroll.offset_at(now);
        for row in &self.rows {
            let Some(record) = page.record(row.id) else {
                continue;
            };
            let (source, label) = match kind(record) {
                ItemKind::Image => ("图片", String::new()),
                ItemKind::File => ("文件", preview_text(record)),
                ItemKind::Text | ItemKind::Link => ("文字", preview_text(record)),
            };
            let label = if label.trim().is_empty() {
                tr!(page.language, source).into_owned()
            } else {
                label
            };
            controls.push(HistoryControl {
                target: HistoryTarget::Row(row.id),
                label,
                bounds: layout.visible_bounds(row.visual.bounds_at(now), offset),
            });
            for (action, label) in [
                (ItemAction::Copy, "复制"),
                (ItemAction::Send, "发送到 iPhone"),
                (ItemAction::Delete, "删除"),
            ] {
                if let Some(bounds) = row.visual.action_bounds_at(action, now) {
                    controls.push(HistoryControl {
                        target: HistoryTarget::Action(row_action(row.id, action)),
                        label: tr!(page.language, label).into_owned(),
                        bounds: layout.visible_bounds(bounds, offset),
                    });
                }
            }
        }
        controls
    }

    pub fn reveal_control(&mut self, target: HistoryTarget, now: Instant) -> Result<()> {
        match target {
            HistoryTarget::Row(id)
            | HistoryTarget::Action(
                HistoryAction::Copy(id) | HistoryAction::Send(id) | HistoryAction::Delete(id),
            ) => self.reveal(id, now),
            _ => {
                if let Some(bounds) = self.chrome.scrolled_bounds(target) {
                    self.scroll.reveal(&self.animator, bounds, now)?;
                }
                Ok(())
            }
        }
    }

    pub fn reveal(&mut self, id: HistoryRowId, now: Instant) -> Result<()> {
        if let Some(row) = self.rows.iter().find(|row| row.id == id) {
            self.scroll
                .reveal(&self.animator, row.visual.bounds(), now)?;
        }
        Ok(())
    }

    /// Call after refresh and while the viewport scrolls. The decoder is only
    /// asked for bounded PNG previews whose image tile intersects the viewport.
    /// A failed decode keeps the type icon. Images leaving the viewport are released.
    pub fn refresh_thumbnails(
        &mut self,
        scene: &PopupScene,
        page: &HistoryPage,
        now: Instant,
        mut decode: impl FnMut(ThumbnailRequest<'_>) -> Option<ThumbnailPixels>,
    ) -> Result<()> {
        let layout = self.scroll.layout();
        let offset = self.scroll.offset_at(now);
        // Release the offscreen surfaces before allocating replacements.
        let mut loaded = 0;
        for row in &mut self.rows {
            if row.thumbnail == ThumbnailState::Loaded && !tile_visible(layout, offset, row, now) {
                row.visual.set_thumbnail(&self.animator, None)?;
                row.thumbnail = ThumbnailState::NotRequested;
            }
            if row.thumbnail == ThumbnailState::Loaded {
                loaded += 1;
            }
        }
        for row in &mut self.rows {
            if loaded == MAX_VISIBLE_THUMBNAILS {
                break;
            }
            if row.visual.layout().kind != ItemKind::Image
                || row.thumbnail != ThumbnailState::NotRequested
                || !tile_visible(layout, offset, row, now)
            {
                continue;
            }
            let tile = row.visual.layout().tile().size();
            let pixels =
                |dips: f32| ((dips * scene.scale()).ceil() as u32).clamp(1, MAX_THUMBNAIL_SIDE);
            let picture = page
                .record(row.id)
                .filter(|record| record.revision == row.paint.revision)
                .and_then(|record| {
                    decode(ThumbnailRequest {
                        id: row.id,
                        preview: Some(record.entry.preview.as_slice())
                            .filter(|&preview| bounded_png(preview)),
                        width: pixels(tile.X),
                        height: pixels(tile.Y),
                    })
                });
            if let Some(picture) = picture {
                // Show the whole picture, scaled to fit and centred in its slot.
                let size = if row.visual.layout().has_media() {
                    contain(picture.width, picture.height, tile)
                } else {
                    tile
                };
                let sprite = scene.thumbnail(picture.width, picture.height, &picture.bgra, size)?;
                row.visual
                    .set_thumbnail(&self.animator, Some(&sprite.cast()?))?;
                row.thumbnail = ThumbnailState::Loaded;
                loaded += 1;
            } else {
                row.thumbnail = ThumbnailState::Invalid;
            }
        }
        Ok(())
    }
}

impl LayoutRow {
    fn paint(
        &self,
        scene: &PopupScene,
        style: &RowPaint,
        record: &Record,
        layout: &HistoryLayout,
        origin: Vector2,
    ) -> Result<ListItem> {
        let language = style.language;
        let mut detail = short_time(&record.entry.created, style.today, language);
        match self.item.kind {
            ItemKind::Image if record.entry.size > 0 => {
                detail.push_str(" · ");
                detail.push_str(&human_size(record.entry.size));
            }
            ItemKind::Text | ItemKind::Link => {
                if let Some(count) = record.entry.chars {
                    detail.push_str(" · ");
                    detail.push_str(&tr!(language, "{count} 字", count = count.to_string()));
                }
            }
            _ => {}
        }
        let title = match self.item.kind {
            ItemKind::Image => capitalized(&tr!(language, "图片")),
            ItemKind::File => {
                let value = String::from_utf8_lossy(&record.entry.preview);
                let (name, size) = value.split_once('\n').unwrap_or((&value, ""));
                if let Ok(size) = size.parse::<u64>() {
                    detail.push_str(" · ");
                    detail.push_str(&human_size(size));
                }
                if name.is_empty() {
                    capitalized(&tr!(language, "文件"))
                } else {
                    name.to_owned()
                }
            }
            ItemKind::Text => preview_text(record),
            ItemKind::Link => preview_text(record)
                .split_once("://")
                .map_or_else(String::new, |(_, tail)| tail.to_owned()),
        };
        let mark = if style.copied {
            Some("已复制")
        } else {
            style.sent.map(|mark| match mark {
                super::SendMark::Sending => "正在发送到 iPhone…",
                super::SendMark::Sent => "已发送到 iPhone",
                super::SendMark::Failed => "发送失败",
            })
        };
        if let Some(mark) = mark {
            detail.push_str(" · ");
            detail.push_str(&tr!(language, mark));
        }
        let lines = if style.selected && matches!(self.item.kind, ItemKind::Text | ItemKind::Link) {
            layout
                .selected_title
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        } else {
            // Leave room for the text sprite's anti-aliasing margin.
            let title = fit(&title, self.item.text_width() - 2.0, |value| {
                layout.measure.width(value, Font::Semibold, TEXT_SIZE)
            })?;
            return self.build(scene, style, origin, &[&title], &detail);
        };
        self.build(scene, style, origin, &lines, &detail)
    }

    fn build(
        &self,
        scene: &PopupScene,
        style: &RowPaint,
        origin: Vector2,
        title: &[&str],
        detail: &str,
    ) -> Result<ListItem> {
        ListItem::new(
            scene,
            origin,
            self.item,
            ItemContent {
                title,
                detail,
                emphasized_detail: style.copied || style.sent.is_some(),
                copied: style.copied,
            },
            &LIGHT_THEME,
        )
    }
}

fn pill_width(layout: &HistoryLayout) -> Result<f32> {
    // Use the wider of both title languages to avoid rebuilding the pill on language switches.
    let en = layout.measure.width("Clipboard", Font::Bold, 20.0)?;
    let zh = layout.measure.width("剪贴板", Font::Bold, 20.0)?;
    Ok(WIDTH - 2.0 * PAD - 52.0 - en.max(zh))
}

fn pill_label(page: &HistoryPage) -> String {
    let label = tr!(
        page.language,
        match page.status.connection {
            Status::Connected => "已连接",
            Status::Connecting => "连接中",
            Status::Disconnected => "未连接",
        }
    );
    if page.status.connection == Status::Connected && !page.status.device_name.is_empty() {
        format!("{label} · {}", page.status.device_name)
    } else {
        label.into_owned()
    }
}

fn row_action(id: HistoryRowId, action: ItemAction) -> HistoryAction {
    match action {
        ItemAction::Copy => HistoryAction::Copy(id),
        ItemAction::Send => HistoryAction::Send(id),
        ItemAction::Delete => HistoryAction::Delete(id),
    }
}

fn kind(record: &Record) -> ItemKind {
    match record.entry.kind.as_str() {
        "file" => ItemKind::File,
        "image" => ItemKind::Image,
        _ if record.entry.preview.starts_with(b"http://")
            || record.entry.preview.starts_with(b"https://") =>
        {
            ItemKind::Link
        }
        _ => ItemKind::Text,
    }
}

fn preview_text(record: &Record) -> String {
    let text = String::from_utf8_lossy(&record.entry.preview);
    let mut value: String = text.chars().take(96).collect();
    if value.chars().count() >= 96 {
        value.push('…');
    }
    value
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .chain(characters)
        .collect()
}

fn tile_visible(layout: ScrollLayout, offset: f32, row: &Row, now: Instant) -> bool {
    let tile = row
        .visual
        .layout()
        .tile()
        .translated(row.visual.bounds_at(now).origin());
    layout.visible_bounds(tile, offset).is_some()
}

fn bounded_png(bytes: &[u8]) -> bool {
    // Both history writers store PNG previews no larger than 88 × 56.
    if bytes.len() < 33
        || bytes.len() > 64 * 1024
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR")
    {
        return false;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    (1..=PREVIEW_WIDTH).contains(&width) && (1..=PREVIEW_HEIGHT).contains(&height)
}

fn viewport_fade(scene: &PopupScene, height: f32) -> Result<SpriteVisual> {
    let compositor = scene.compositor();
    let brush = compositor.CreateLinearGradientBrush()?;
    brush.SetStartPoint(Vector2::new(0.0, 0.0))?;
    brush.SetEndPoint(Vector2::new(0.0, 1.0))?;
    let stops = brush.ColorStops()?;
    stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(
        0.0,
        Color {
            A: 0,
            ..LIGHT_THEME.white
        },
    )?)?;
    stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(1.0, LIGHT_THEME.white)?)?;
    let visual = compositor.CreateSpriteVisual()?;
    visual.SetSize(Vector2::new(WIDTH, 18.0))?;
    visual.SetOffset(xy(0.0, height - 18.0))?;
    visual.SetBrush(&brush)?;
    Ok(visual)
}

/// Labels in the key order used by `HistoryView::set_interaction`.
const ACTION_TIPS: [&str; 3] = ["复制", "发送到 iPhone", "删除"];

fn action_tooltip(
    scene: &PopupScene,
    layout: &HistoryLayout,
    language: copiago_core::i18n::Language,
) -> Result<Tooltip> {
    let labels = ACTION_TIPS.map(|label| tr!(language, label));
    let labels = labels
        .iter()
        .map(|label| label.as_ref())
        .collect::<Vec<_>>();
    Tooltip::new(
        scene,
        Rect::new(0.0, 0.0, WIDTH, layout.bounds().height),
        &labels,
        &LIGHT_THEME,
    )
}

/// Width over height from a stored preview's PNG header.
fn png_aspect(bytes: &[u8]) -> Option<f32> {
    if !bounded_png(bytes) {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some(width as f32 / height as f32)
}

/// The largest size with the picture's aspect ratio that fits inside `slot`.
fn contain(width: u32, height: u32, slot: Vector2) -> Vector2 {
    let scale = (slot.X / width as f32).min(slot.Y / height as f32);
    Vector2::new(
        (width as f32 * scale).round().clamp(1.0, slot.X),
        (height as f32 * scale).round().clamp(1.0, slot.Y),
    )
}
