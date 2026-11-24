use std::{borrow::Cow, time::Instant};

use copiago_core::{
    history::Source,
    i18n::{Language, tr},
};
use windows::{
    UI::{Color, Composition::ContainerVisual},
    core::Result,
};
use windows_numerics::Vector2;

use crate::{
    animation::{Animator, Tween},
    components::{Button, ButtonStyle, Interaction, Rect, ScrollView, Theme, center, text, xy},
    composition::{PopupScene, rgb, rounded_rectangle},
};

use super::*;
use crate::history::HistoryOffer;

/// Status lines under the title before the text is ellipsized.
const MAX_DETAIL_LINES: usize = 4;

#[derive(Clone, PartialEq)]
pub(super) struct ChromeState {
    pub(super) tab: Source,
    language: Language,
    phone_count: usize,
    pc_count: usize,
    enabled: bool,
    clear: bool,
    pub(super) detail: Vec<String>,
    tone: StatusTone,
    retry_width: Option<f32>,
    offer: Option<HistoryOffer>,
    update: Option<String>,
}

impl ChromeState {
    pub(super) fn new(page: &HistoryPage, measure: &TextMeasurer) -> Result<Self> {
        let language = page.language;
        let value = page
            .status
            .detail
            .as_ref()
            .map_or(Cow::Borrowed(""), |detail| detail.render(language));
        let hide = value.is_empty()
            || (page.status.tone == StatusTone::Success
                && value.starts_with(tr!(language, "已连接").as_ref()));
        let retry_width = if !hide && page.status.can_retry {
            Some(
                measure
                    .width(&tr!(language, "继续连接"), Font::Regular, NOTE_SIZE)?
                    .ceil()
                    + 24.0,
            )
        } else {
            None
        };
        let detail = if hide {
            Vec::new()
        } else {
            let width = INNER - retry_width.map_or(0.0, |w| w + 8.0);
            // Instructions such as enabling Developer Mode need three lines; never cut them off.
            wrap(&value, width - 2.0, MAX_DETAIL_LINES, |line| {
                measure.width(line, Font::Regular, NOTE_SIZE)
            })?
        };
        Ok(Self {
            tab: page.tab,
            language,
            phone_count: page.phone.records.len(),
            pc_count: page.pc.records.len(),
            enabled: page.list(page.tab).enabled,
            clear: page.clear_until.is_some(),
            detail,
            tone: page.status.tone,
            retry_width,
            offer: page.current_offer().cloned(),
            update: page.update.clone(),
        })
    }

    fn count(&self, source: Source) -> usize {
        match source {
            Source::Phone => self.phone_count,
            Source::Pc => self.pc_count,
        }
    }
}

struct Control {
    target: HistoryTarget,
    bounds: Rect,
    clipped: bool,
    button: Option<Button>,
}

pub(super) struct Chrome {
    pub(super) frame: ContainerVisual,
    pub(super) scrolled: ContainerVisual,
    pub(super) state: ChromeState,
    controls: Vec<Control>,
}

impl Chrome {
    pub(super) fn new(
        scene: &PopupScene,
        animator: &Animator,
        layout: &HistoryLayout,
        previous_tab: Source,
    ) -> Result<Self> {
        let mut chrome = Self {
            frame: scene.compositor().CreateContainerVisual()?,
            scrolled: scene.compositor().CreateContainerVisual()?,
            state: layout.state.clone(),
            controls: Vec::new(),
        };
        let style = &layout.state;
        let language = style.language;
        label(
            scene,
            &chrome.frame,
            Rect::new(PAD, 16.0, 160.0, 36.0),
            &tr!(language, "剪贴板"),
            (Font::Bold, 20.0),
            LIGHT_THEME.ink,
            false,
        )?;
        let bounds = Rect::new(WIDTH - PAD - 32.0, 18.0, 32.0, 32.0);
        let button = Button::icon(
            scene,
            bounds,
            ButtonStyle::Neutral,
            '\u{E9E9}',
            16.0,
            &LIGHT_THEME,
        )?;
        let settings = HistoryTarget::Action(HistoryAction::OpenSettings);
        chrome.button(button, Some(settings), false)?;
        let detail_width = INNER - style.retry_width.map_or(0.0, |width| width + 8.0);
        let tone = match style.tone {
            StatusTone::Muted => LIGHT_THEME.muted,
            StatusTone::Success => LIGHT_THEME.connected.text,
            StatusTone::Warning => LIGHT_THEME.disconnected.text,
        };
        for (index, line) in style.detail.iter().enumerate() {
            label(
                scene,
                &chrome.frame,
                Rect::new(PAD, 61.0 + index as f32 * 18.0, detail_width, 18.0),
                line,
                (Font::Regular, NOTE_SIZE),
                tone,
                false,
            )?;
        }
        if let Some(width) = style.retry_width {
            let button = Button::text(
                scene,
                Rect::new(WIDTH - PAD - width, 58.0, width, 26.0),
                ButtonStyle::Neutral,
                &tr!(language, "继续连接"),
                &LIGHT_THEME,
            )?;
            chrome.button(
                button,
                Some(HistoryTarget::Action(HistoryAction::Retry)),
                false,
            )?;
        }
        chrome.tabs(scene, animator, layout, previous_tab)?;
        chrome.footer(scene, layout)?;
        if let Some(offer) = &style.offer {
            chrome.offer(scene, animator, &layout.measure, offer)?;
        } else if style.count(style.tab) == 0 {
            chrome.empty(scene, &layout.measure)?;
        }
        Ok(chrome)
    }

    fn button(
        &mut self,
        button: Button,
        target: Option<HistoryTarget>,
        clipped: bool,
    ) -> Result<()> {
        let root = if clipped { &self.scrolled } else { &self.frame };
        root.Children()?.InsertAtTop(button.visual())?;
        if let Some(target) = target {
            self.controls.push(Control {
                target,
                bounds: button.bounds(),
                clipped,
                button: Some(button),
            });
        }
        Ok(())
    }

    pub(super) fn hit(
        &self,
        point: Vector2,
        scroll: &ScrollView,
        now: Instant,
    ) -> Option<HistoryTarget> {
        self.controls.iter().rev().find_map(|control| {
            let point = if control.clipped {
                if !scroll
                    .layout()
                    .fully_visible(control.bounds, scroll.offset_at(now))
                {
                    return None;
                }
                scroll.content_point(point, now)?
            } else {
                point
            };
            control.bounds.contains(point).then_some(control.target)
        })
    }

    pub(super) fn controls(&self, scroll: &ScrollView, now: Instant) -> Vec<HistoryControl> {
        let language = self.state.language;
        self.controls
            .iter()
            .map(|control| {
                let source = match control.target {
                    HistoryTarget::Tab(Source::Phone) => "iPhone",
                    HistoryTarget::Tab(Source::Pc) => "电脑",
                    HistoryTarget::Action(action) => match action {
                        HistoryAction::OpenSettings => "连接与设置",
                        HistoryAction::OpenUpdate => "发现新版本 {version}，点击查看",
                        HistoryAction::Retry => "继续连接",
                        HistoryAction::Clear(_) if self.state.clear => "确认清空",
                        HistoryAction::Clear(_) => "清空",
                        HistoryAction::Enable(Source::Phone) => "开启 iPhone 剪贴板历史",
                        HistoryAction::Enable(Source::Pc) => "开启电脑剪贴板历史",
                        HistoryAction::RespondToOffer { accept: true, .. } => "同步到电脑",
                        HistoryAction::RespondToOffer { accept: false, .. } => "忽略",
                        HistoryAction::Copy(_)
                        | HistoryAction::Send(_)
                        | HistoryAction::Delete(_) => {
                            unreachable!("row actions are published by the history view")
                        }
                    },
                    HistoryTarget::Row(_) => unreachable!("rows are published by the history view"),
                };
                let label = if control.target == HistoryTarget::Action(HistoryAction::OpenUpdate) {
                    tr!(
                        language,
                        source,
                        version = self.state.update.as_deref().unwrap_or_default()
                    )
                } else {
                    tr!(language, source)
                };
                let bounds = if control.clipped {
                    scroll
                        .layout()
                        .visible_bounds(control.bounds, scroll.offset_at(now))
                } else {
                    Some(control.bounds)
                };
                HistoryControl {
                    target: control.target,
                    label: label.into_owned(),
                    bounds,
                }
            })
            .collect()
    }

    pub(super) fn scrolled_bounds(&self, target: HistoryTarget) -> Option<Rect> {
        self.controls
            .iter()
            .find(|control| control.clipped && control.target == target)
            .map(|control| control.bounds)
    }

    pub(super) fn interact(
        &mut self,
        animator: &Animator,
        state: impl Fn(HistoryTarget) -> Interaction,
    ) -> Result<()> {
        for control in &mut self.controls {
            if let Some(button) = &mut control.button {
                button.set_interaction(animator, state(control.target))?;
            }
        }
        Ok(())
    }

    fn tabs(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        layout: &HistoryLayout,
        previous: Source,
    ) -> Result<()> {
        let y = layout.tabs_top();
        shape(
            scene,
            &self.frame,
            Rect::new(PAD, y, INNER, 32.0),
            16.0,
            LIGHT_THEME.control,
        )?;
        let x = |tab| {
            PAD + if tab == Source::Phone {
                0.0
            } else {
                INNER / 2.0
            }
        };
        let indicator = rounded_rectangle(
            scene.compositor(),
            Vector2::new(INNER / 2.0 - 6.0, 26.0),
            13.0,
            LIGHT_THEME.white,
        )?;
        indicator.visual.SetOffset(xy(x(previous) + 3.0, y + 3.0))?;
        self.frame.Children()?.InsertAtTop(&indicator.visual)?;
        animator.offset(
            &indicator.visual,
            xy(x(self.state.tab) + 3.0, y + 3.0),
            Tween::new(280),
        )?;
        for (tab, name) in [(Source::Phone, "iPhone"), (Source::Pc, "电脑")] {
            let name = tr!(self.state.language, name);
            let count = self.state.count(tab);
            let name = if count == 0 {
                name
            } else {
                Cow::Owned(format!("{name} · {count}"))
            };
            let selected = self.state.tab == tab;
            let bounds = Rect::new(x(tab), y, INNER / 2.0, 32.0);
            label(
                scene,
                &self.frame,
                bounds,
                &name,
                (
                    if selected {
                        Font::Semibold
                    } else {
                        Font::Regular
                    },
                    12.5,
                ),
                if selected {
                    LIGHT_THEME.ink
                } else {
                    LIGHT_THEME.muted
                },
                true,
            )?;
            self.controls.push(Control {
                target: HistoryTarget::Tab(tab),
                bounds,
                clipped: false,
                button: None,
            });
        }
        Ok(())
    }

    fn footer(&mut self, scene: &PopupScene, layout: &HistoryLayout) -> Result<()> {
        let state = &layout.state;
        let language = state.language;
        let count = state.count(state.tab);
        let top = layout.viewport.bottom() + 12.0;
        let note = if let Some(version) = &state.update {
            tr!(
                language,
                "发现新版本 {version}，点击查看",
                version = version
            )
        } else if state.enabled && count != 0 {
            tr!(language, "{count} 条 · 加密保存在本机", count = count)
        } else if state.enabled {
            tr!(language, "已开启 · 加密保存在本机")
        } else {
            tr!(
                language,
                if state.tab == Source::Phone {
                    "未保留 iPhone 剪贴板历史"
                } else {
                    "未保留电脑剪贴板历史"
                }
            )
        };
        let mut right = WIDTH - PAD;
        if count != 0 {
            let value = tr!(
                language,
                if state.clear {
                    "确认清空"
                } else {
                    "清空"
                }
            );
            let width = button_width(&layout.measure, &value, 28.0)?;
            let theme = if state.clear {
                Theme {
                    control: LIGHT_THEME.danger_tint,
                    soft_ink: LIGHT_THEME.danger,
                    control_hover: LIGHT_THEME.danger_tint,
                    ..LIGHT_THEME
                }
            } else {
                LIGHT_THEME
            };
            let button = Button::text(
                scene,
                Rect::new(right - width, top, width, 32.0),
                ButtonStyle::Neutral,
                &value,
                &theme,
            )?;
            self.button(
                button,
                Some(HistoryTarget::Action(HistoryAction::Clear(state.tab))),
                false,
            )?;
            right -= width + 8.0;
        }
        let note_bounds = Rect::new(PAD + 4.0, top, right - PAD - 4.0, 32.0);
        let note = fit(&note, note_bounds.width - 2.0, |text| {
            layout.measure.width(text, Font::Regular, NOTE_SIZE)
        })?;
        let is_link = state.update.is_some();
        label(
            scene,
            &self.frame,
            note_bounds,
            &note,
            (Font::Regular, NOTE_SIZE),
            if is_link {
                LIGHT_THEME.accent
            } else {
                LIGHT_THEME.muted
            },
            false,
        )?;
        if is_link {
            let width = layout
                .measure
                .width(&note, Font::Regular, NOTE_SIZE)?
                .ceil()
                + 8.0;
            self.controls.push(Control {
                target: HistoryTarget::Action(HistoryAction::OpenUpdate),
                bounds: Rect::new(PAD, top, width, 32.0),
                clipped: false,
                button: None,
            });
        }
        Ok(())
    }

    fn offer(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        measure: &TextMeasurer,
        offer: &HistoryOffer,
    ) -> Result<()> {
        let language = self.state.language;
        shape(
            scene,
            &self.scrolled,
            Rect::new(PAD, 4.0, INNER, 96.0),
            16.0,
            rgb(240, 214, 170),
        )?;
        shape(
            scene,
            &self.scrolled,
            Rect::new(PAD + 1.0, 5.0, INNER - 2.0, 94.0),
            15.0,
            LIGHT_THEME.file_tint,
        )?;
        let tile = Rect::new(PAD + 12.0, 16.0, 36.0, 36.0);
        shape(scene, &self.scrolled, tile, 10.0, LIGHT_THEME.white)?;
        let color = LIGHT_THEME.disconnected.text;
        icon(scene, &self.scrolled, tile, '\u{E7C3}', 18.0, color)?;
        label(
            scene,
            &self.scrolled,
            Rect::new(PAD + 60.0, 15.0, INNER - 74.0, 20.0),
            &offer.name,
            (Font::Semibold, TEXT_SIZE),
            LIGHT_THEME.ink,
            false,
        )?;
        label(
            scene,
            &self.scrolled,
            Rect::new(PAD + 60.0, 34.0, INNER - 74.0, 20.0),
            &offer.metadata,
            (Font::Regular, NOTE_SIZE),
            LIGHT_THEME.muted,
            false,
        )?;
        let mut right = WIDTH - PAD - 12.0;
        for (accept, text, style) in [
            (
                true,
                if offer.busy {
                    "同步中…"
                } else {
                    "同步到电脑"
                },
                ButtonStyle::Primary,
            ),
            (false, "忽略", ButtonStyle::Neutral),
        ] {
            if offer.busy && !accept {
                continue;
            }
            let value = tr!(language, text);
            let width = button_width(measure, &value, 28.0)?;
            let mut button = Button::text(
                scene,
                Rect::new(right - width, 58.0, width, 30.0),
                style,
                &value,
                &LIGHT_THEME,
            )?;
            button.set_enabled(animator, !offer.busy)?;
            let action = HistoryAction::RespondToOffer {
                id: offer.id,
                accept,
            };
            self.button(
                button,
                (!offer.busy).then_some(HistoryTarget::Action(action)),
                true,
            )?;
            right -= width + 8.0;
        }
        let reason = offer.reason.render(language);
        label(
            scene,
            &self.scrolled,
            Rect::new(PAD + 14.0, 58.0, right - PAD - 18.0, 30.0),
            &reason,
            (Font::Regular, NOTE_SIZE),
            color,
            false,
        )
    }

    fn empty(&mut self, scene: &PopupScene, measure: &TextMeasurer) -> Result<()> {
        let state = &self.state;
        let language = state.language;
        let (title, hint, enable) = match (state.tab, state.enabled) {
            (Source::Phone, true) => (
                "等待从 iPhone 同步",
                "在 iPhone 上复制的文字和图片会出现在这里",
                None,
            ),
            (Source::Phone, false) => (
                "iPhone 剪贴板历史未开启",
                "开启后加密保存之后从手机收到的内容，可在设置中随时关闭",
                Some("开启 iPhone 剪贴板历史"),
            ),
            (Source::Pc, true) => (
                "等待你在电脑上复制",
                "在电脑上复制的文字和图片会出现在这里，可随时发到 iPhone",
                None,
            ),
            (Source::Pc, false) => (
                "电脑剪贴板历史未开启",
                "开启后记录在电脑上复制的内容（跳过密码等敏感内容），可随时发到 iPhone",
                Some("开启电脑剪贴板历史"),
            ),
        };
        let tile = Rect::new(WIDTH / 2.0 - 28.0, 48.0, 56.0, 56.0);
        shape(scene, &self.scrolled, tile, 28.0, LIGHT_THEME.tint)?;
        icon(
            scene,
            &self.scrolled,
            tile,
            '\u{E77F}',
            26.0,
            LIGHT_THEME.accent,
        )?;
        label(
            scene,
            &self.scrolled,
            Rect::new(PAD, 120.0, INNER, 24.0),
            &tr!(language, title),
            (Font::Semibold, 14.0),
            LIGHT_THEME.ink,
            true,
        )?;
        let width = WIDTH - 4.0 * PAD;
        for (index, line) in wrap(&tr!(language, hint), width - 2.0, 2, |text| {
            measure.width(text, Font::Regular, NOTE_SIZE)
        })?
        .iter()
        .enumerate()
        {
            label(
                scene,
                &self.scrolled,
                Rect::new(PAD * 2.0, 148.0 + index as f32 * 20.0, width, 20.0),
                line,
                (Font::Regular, NOTE_SIZE),
                LIGHT_THEME.muted,
                true,
            )?;
        }
        if let Some(enable) = enable {
            let value = tr!(language, enable);
            let width = (measure.width(&value, Font::Semibold, 14.0)?.ceil() + 40.0).min(INNER);
            let bounds = Rect::new((WIDTH - width) / 2.0, 208.0, width, 38.0);
            let button = Button::text(scene, bounds, ButtonStyle::Primary, &value, &LIGHT_THEME)?;
            self.button(
                button,
                Some(HistoryTarget::Action(HistoryAction::Enable(state.tab))),
                true,
            )?;
        }
        Ok(())
    }
}

fn label(
    scene: &PopupScene,
    parent: &ContainerVisual,
    bounds: Rect,
    value: &str,
    (font, size): (Font, f32),
    color: Color,
    centered: bool,
) -> Result<()> {
    let surface = text(scene, value, font, size, bounds.width, color)?;
    if centered {
        center(&surface, bounds)?;
    } else {
        surface.visual.SetOffset(xy(
            bounds.x,
            bounds.y + (bounds.height - surface.size.Y) / 2.0,
        ))?;
    }
    parent.Children()?.InsertAtTop(&surface.visual)
}

fn button_width(measure: &TextMeasurer, value: &str, padding: f32) -> Result<f32> {
    Ok(measure.width(value, Font::Regular, 12.5)?.ceil() + padding)
}

fn icon(
    scene: &PopupScene,
    parent: &ContainerVisual,
    bounds: Rect,
    glyph: char,
    size: f32,
    color: Color,
) -> Result<()> {
    let sprite = scene.icon(glyph, size, color)?;
    center(&sprite, bounds)?;
    parent.Children()?.InsertAtTop(&sprite.visual)
}

fn shape(
    scene: &PopupScene,
    parent: &ContainerVisual,
    bounds: Rect,
    radius: f32,
    color: Color,
) -> Result<()> {
    let shape = rounded_rectangle(scene.compositor(), bounds.size(), radius, color)?;
    shape.visual.SetOffset(xy(bounds.x, bounds.y))?;
    parent.Children()?.InsertAtTop(&shape.visual)
}

#[cfg(test)]
mod tests {
    use copiago_core::i18n::{Language, message};

    use super::*;
    use crate::history::{HistoryPage, HistoryStatus};

    #[test]
    fn long_connection_instructions_are_not_cut_off() {
        let mut page = HistoryPage::new(Language::ZhCn);
        page.set_status(HistoryStatus {
            detail: Some(message!(
                "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
            )),
            tone: StatusTone::Warning,
            can_retry: true,
            ..HistoryStatus::default()
        });
        let state = ChromeState::new(&page, &TextMeasurer::new().unwrap()).unwrap();
        assert!(state.detail.len() > 2, "{:?}", state.detail);
        assert!(state.detail.concat().ends_with("「继续连接」"));
    }
}
