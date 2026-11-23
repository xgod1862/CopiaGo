use copiago_core::settings::FILE_ASK_MB_CHOICES;

use super::{super::SyncDirection, *};
use crate::{components::ButtonStyle, composition::rgb};

const ROW_HEIGHT: f32 = 44.0;

enum Row {
    Switch(SettingSwitch, &'static str),
    Choices(ChoiceRow, &'static str),
}

#[derive(Clone, Copy)]
enum ChoiceRow {
    Language,
    Content(SyncDirection),
    FileSize,
}

const FORM: [(&str, &[Row]); 4] = [
    ("", &[Row::Choices(ChoiceRow::Language, "语言")]),
    (
        "iPhone → 电脑",
        &[
            Row::Switch(SettingSwitch::Pull, "自动同步到电脑"),
            Row::Choices(ChoiceRow::Content(SyncDirection::ToPc), "同步内容"),
            Row::Choices(ChoiceRow::FileSize, "大文件先询问"),
            Row::Switch(SettingSwitch::History, "保留 iPhone 剪贴板历史"),
        ],
    ),
    (
        "电脑 → iPhone",
        &[
            Row::Switch(SettingSwitch::Push, "自动同步到 iPhone"),
            Row::Choices(ChoiceRow::Content(SyncDirection::ToPhone), "同步内容"),
            Row::Switch(SettingSwitch::PcHistory, "保留电脑剪贴板历史"),
        ],
    ),
    (
        "通用",
        &[
            Row::Switch(SettingSwitch::Startup, "登录 Windows 时启动"),
            Row::Switch(SettingSwitch::Notifications, "显示同步、断开和错误通知"),
            Row::Switch(SettingSwitch::UpdateCheck, "启动时检查新版本"),
        ],
    ),
];

impl Builder<'_> {
    pub(super) fn settings(&mut self, confirming: bool) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let back = Button::icon(
            scene,
            Rect::new(PAD, 18.0, 32.0, 32.0),
            ButtonStyle::Neutral,
            '\u{E72B}',
            16.0,
            &THEME,
        )?;
        self.add_button(
            back,
            tr!(language, "返回").into_owned(),
            SettingsAction::Back,
            false,
            ControlRole::Button,
        )?;
        label(
            scene,
            self.target(false),
            &tr!(language, "设置"),
            Rect::new(PAD + 44.0, 18.0, 135.0, 32.0),
            Font::Bold,
            20.0,
            THEME.ink,
        )?;
        let status = pill_status(self.state.connection().phase);
        let pill = StatusPill::new(
            scene,
            self.animator,
            Vector2::new(WIDTH - PAD, 20.0),
            135.0,
            status,
            &pill_label(language, status),
            &THEME,
        )?;
        self.target(false).Children()?.InsertAtTop(pill.visual())?;
        self.body.pill = Some(pill);

        let mut y = 4.0;
        if let Some(device) = &self.state.connection().device {
            y = self.device_card(device, y, confirming)? + 14.0;
        }
        for (section, rows) in FORM {
            if !section.is_empty() {
                label(
                    scene,
                    self.target(true),
                    &tr!(language, section),
                    Rect::new(PAD + 6.0, y, INNER - 12.0, 18.0),
                    Font::Semibold,
                    12.0,
                    THEME.muted,
                )?;
                y += 24.0;
            }
            let height = ROW_HEIGHT * rows.len() as f32;
            background(
                scene,
                self.target(true),
                Rect::new(PAD, y, INNER, height),
                16.0,
                THEME.card,
            )?;
            for (index, row) in rows.iter().enumerate() {
                let top = y + index as f32 * ROW_HEIGHT;
                if index > 0 {
                    background(
                        scene,
                        self.target(true),
                        Rect::new(PAD + 16.0, top, INNER - 32.0, 1.0),
                        0.0,
                        rgb(226, 231, 236),
                    )?;
                }
                match *row {
                    Row::Switch(setting, title) => self.switch_row(setting, title, top)?,
                    Row::Choices(kind, title) => self.choice_row(kind, section, title, top)?,
                }
            }
            y += height + 14.0;
        }
        self.settings_footer()?;
        Ok(y - 14.0)
    }

    fn device_card(&mut self, device: &DeviceInfo, y: f32, confirming: bool) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let (title, summary) = device_strings(device);
        let phase = self.state.connection().phase;
        let retry = phase.can_continue();
        let detail_y = y + if retry { 54.0 } else { 38.0 };
        let detail_height = if retry { 74.0 } else { 38.0 };
        let summary_y = detail_y + detail_height;
        let actions_y = summary_y + if summary.is_empty() { 0.0 } else { 18.0 };
        let height = actions_y - y + if retry { 50.0 } else { 18.0 };
        let phone = Rect::new(PAD + 12.0, y + 14.0, 36.0, 36.0);
        background(
            scene,
            self.target(true),
            Rect::new(PAD, y, INNER, height),
            16.0,
            THEME.card,
        )?;
        background(scene, self.target(true), phone, 10.0, THEME.tint)?;
        icon(
            scene,
            self.target(true),
            phone,
            '\u{E8EA}',
            18.0,
            THEME.accent,
        )?;

        let button = Rect::new(WIDTH - PAD - 12.0 - 126.0, y + 16.0, 126.0, 32.0);
        let text_x = PAD + 60.0;
        let title_width = button.x - text_x - 9.0;
        label(
            scene,
            self.target(true),
            &title,
            Rect::new(text_x, y + 12.0, title_width, 24.0),
            Font::Semibold,
            13.5,
            THEME.ink,
        )?;
        let label_text = tr!(
            language,
            if confirming {
                "确认忘记"
            } else {
                "忘记此 iPhone"
            }
        );
        let forget = Button::forget(scene, button, &label_text, confirming, &THEME)?;
        self.add_button(
            forget,
            label_text.into_owned(),
            SettingsAction::ForgetDevice,
            true,
            ControlRole::Button,
        )?;

        let detail_width = if retry {
            WIDTH - PAD - 12.0 - text_x
        } else {
            title_width
        };
        self.body.detail = Some(TextSlot::new(
            scene,
            self.target(true),
            Rect::new(text_x, detail_y, detail_width, detail_height),
            if retry { 4 } else { 2 },
        )?);
        if !summary.is_empty() {
            label(
                scene,
                self.target(true),
                &summary,
                Rect::new(text_x, summary_y, WIDTH - PAD - 14.0 - text_x, 18.0),
                Font::Regular,
                12.0,
                THEME.muted,
            )?;
        }
        if retry {
            let action = tr!(language, continue_label(phase)).into_owned();
            let retry = Button::text(
                scene,
                Rect::new(PAD + 12.0, actions_y + 5.0, INNER - 24.0, 32.0),
                ButtonStyle::Tinted,
                &action,
                &THEME,
            )?;
            self.add_button(
                retry,
                action,
                SettingsAction::Retry,
                true,
                ControlRole::Button,
            )?;
        }
        self.add_progress(y + height - 12.0)?;
        Ok(y + height)
    }

    fn switch_row(&mut self, setting: SettingSwitch, title: &str, y: f32) -> Result<()> {
        let row = Rect::new(PAD, y, INNER, ROW_HEIGHT);
        let (checked, enabled) = self.state.switch(setting);
        let hover = shape(
            self.scene,
            Rect::new(PAD + 4.0, y + 4.0, INNER - 8.0, ROW_HEIGHT - 8.0),
            12.0,
            transparent(THEME.card_hover),
        )?;
        self.target(true).Children()?.InsertAtTop(&hover.visual)?;
        let x = row.right() - 16.0 - Toggle::WIDTH;
        let toggle = Toggle::new(
            self.scene,
            Vector2::new(x, y + (ROW_HEIGHT - Toggle::HEIGHT) / 2.0),
            row,
            checked,
            enabled,
            &THEME,
        )?;
        let title = tr!(self.language, title).into_owned();
        label(
            self.scene,
            self.target(true),
            &title,
            Rect::new(PAD + 16.0, y, x - PAD - 24.0, ROW_HEIGHT),
            Font::Regular,
            13.5,
            if enabled { THEME.ink } else { THEME.muted },
        )?;
        self.target(true).Children()?.InsertAtTop(toggle.visual())?;
        self.body.controls.push(Control {
            action: SettingsAction::Toggle(setting),
            label: title,
            scrolled: true,
            input: Input::Toggle {
                toggle,
                hover,
                hovered: false,
                policy: self.animator.policy(),
            },
        });
        Ok(())
    }

    fn choice_row(&mut self, kind: ChoiceRow, section: &str, title: &str, y: f32) -> Result<()> {
        let title = tr!(self.language, title);
        let group = tr!(self.language, section);
        let mut right = WIDTH - PAD - 14.0;
        let single = !matches!(kind, ChoiceRow::Content(_));
        let first_control = self.body.controls.len();
        for (action, choice, selected) in kind.options(self.state).into_iter().rev() {
            let choice = tr!(self.language, choice.as_str());
            let button = Button::choice(
                self.scene,
                Vector2::new(right, y + ROW_HEIGHT / 2.0),
                &choice,
                selected,
                single,
                &THEME,
            )?;
            right = button.bounds().x - if single { 4.0 } else { 6.0 };
            let name = if section.is_empty() {
                format!("{title} · {choice}")
            } else {
                format!("{group} · {title} · {choice}")
            };
            self.add_button(button, name, action, true, ControlRole::Choice { selected })?;
        }
        self.body.controls[first_control..].reverse();
        label(
            self.scene,
            self.target(true),
            &title,
            Rect::new(PAD + 16.0, y, (right - PAD - 20.0).max(4.0), ROW_HEIGHT),
            Font::Regular,
            13.5,
            THEME.ink,
        )
    }

    fn settings_footer(&mut self) -> Result<()> {
        let (scene, language) = (self.scene, self.language);
        let mut y = SETTINGS_VIEWPORT.bottom() + 12.0;
        if self.state.notice().is_some() {
            self.body.notice = Some(TextSlot::new(
                scene,
                self.target(false),
                Rect::new(
                    PAD + 4.0,
                    SETTINGS_VIEWPORT.bottom() + 1.0,
                    INNER - 8.0,
                    34.0,
                ),
                2,
            )?);
            y = 452.0;
        }
        let version = version_label(self.state.version());
        if let Some(available) = self.state.available_version() {
            let update = tr!(language, "发现新版本 {version}", version = available);
            let title = format!("{version} · {update}");
            let button = Button::text(
                scene,
                Rect::new(PAD, y, INNER, 26.0),
                ButtonStyle::Tinted,
                &title,
                &THEME,
            )?;
            self.add_button(
                button,
                title,
                SettingsAction::OpenUpdate,
                false,
                ControlRole::Button,
            )?;
        } else {
            label(
                scene,
                self.target(false),
                &version,
                Rect::new(PAD + 4.0, y, INNER - 8.0, 26.0),
                Font::Regular,
                12.0,
                THEME.muted,
            )?;
        }
        Ok(())
    }
}

impl ChoiceRow {
    fn options(self, state: &SettingsState) -> Vec<(SettingsAction, String, bool)> {
        let settings = state.settings();
        match self {
            Self::Language => [
                (LanguagePreference::System, "跟随系统"),
                (LanguagePreference::ZhCn, "简体中文"),
                (LanguagePreference::En, "English"),
            ]
            .into_iter()
            .map(|(language, name)| {
                (
                    SettingsAction::ChooseLanguage(language),
                    name.to_owned(),
                    settings.language == language,
                )
            })
            .collect(),
            Self::Content(direction) => direction
                .choices()
                .iter()
                .copied()
                .map(|kind| {
                    let source = match kind {
                        SyncKind::Text => "文字",
                        SyncKind::Image => "图片",
                        SyncKind::File => "文件",
                    };
                    (
                        SettingsAction::ToggleType(direction, kind),
                        source.to_owned(),
                        direction.selected(settings).contains(&kind),
                    )
                })
                .collect(),
            Self::FileSize => FILE_ASK_MB_CHOICES
                .into_iter()
                .map(|value| {
                    let source = if value == 0 {
                        "不询问".to_owned()
                    } else {
                        format!("{value}MB")
                    };
                    (
                        SettingsAction::ChooseFileAskMb(value),
                        source,
                        settings.file_ask_mb == value,
                    )
                })
                .collect(),
        }
    }
}

pub(super) fn continue_label(phase: ConnectionPhase) -> &'static str {
    if phase == ConnectionPhase::DeveloperMode {
        "我已开启，继续连接"
    } else {
        "继续连接"
    }
}

pub(super) fn version_label(version: &str) -> String {
    if version.is_empty() {
        "CopiaGo".to_owned()
    } else {
        format!("CopiaGo {version}")
    }
}

fn device_strings(device: &DeviceInfo) -> (String, String) {
    // Match the display form in device_info.py, including names composed only of emoji.
    let name: String = device
        .name
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '\u{1F000}'..='\u{10FFFF}' | '\u{200D}' | '\u{20E3}' | '\u{FE0E}' | '\u{FE0F}'
            )
        })
        .collect();
    let mut title = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        title = if device.model.is_empty() {
            "iPhone"
        } else {
            &device.model
        }
        .to_owned();
    }
    let mut parts = Vec::new();
    if !device.model.is_empty() && device.model != title {
        parts.push(device.model.clone());
    }
    if !device.os_version.is_empty() {
        parts.push(format!("iOS {}", device.os_version));
    }
    (title, parts.join(" · "))
}
