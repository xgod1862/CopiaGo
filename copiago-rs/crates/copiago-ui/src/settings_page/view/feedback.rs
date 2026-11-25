use super::*;
use crate::composition::TextMeasurer;

#[derive(Clone, PartialEq)]
pub struct SettingsLayout {
    pub(super) lines: Vec<(String, bool)>,
    activity: Option<String>,
    driver: Option<DriverState>,
    connected: bool,
    help: bool,
    pub(super) height: f32,
    header: f32,
    pub(super) viewport: Rect,
    pub(super) inline: bool,
    pub(super) scroll_all: bool,
}

impl SettingsLayout {
    pub fn measure(
        state: &SettingsState,
        screen: SettingsScreen,
        available_height: f32,
    ) -> Result<Self> {
        let measure = TextMeasurer::new()?;
        let connection = state.connection();
        let phase = connection.phase;
        let guide = screen == SettingsScreen::PairingGuide;
        let driver =
            (guide && !connection.pairing_complete() && connection.driver != DriverState::Ready)
                .then_some(connection.driver);
        let language = state.language();
        let mut messages = Vec::new();
        let mut add = |value: String, warning| {
            if !value.is_empty() && !messages.iter().any(|(text, _)| text == &value) {
                messages.push((value, warning));
            }
        };
        let default = connection.default_detail();
        let detail = connection.detail.as_ref().unwrap_or(&default);
        if let Some(reason) = state.diagnosis() {
            add(reason.render(language).into_owned(), true);
        }
        if let Some(hint) = state.retry_hint() {
            add(hint.render(language).into_owned(), true);
        } else if (state.diagnosis().is_none() || connection.detail.is_some())
            && (guide_detail_visible(state) || !guide && connection.device.is_some())
            && !(phase.busy() && detail == &default)
        {
            add(
                detail.render(language).into_owned(),
                !phase.busy() && !matches!(phase, ConnectionPhase::Connected { .. }),
            );
        }
        if guide && phase == ConnectionPhase::DeveloperMode && connection.detail.is_none() {
            add(tr!(language, guide::DEVELOPER_HELP).into_owned(), true);
        }
        if let Some(error) = state.repair_error() {
            add(error.render(language).into_owned(), true);
        }
        if let Some(notice) = state.notice() {
            add(notice.render(language).into_owned(), true);
        }
        if let Some(driver) = driver {
            add(tr!(language, guide::driver_help(driver)).into_owned(), true);
        }
        let activity = if phase == ConnectionPhase::Waiting {
            Some(tr!(language, "等待下一次自动重试").into_owned())
        } else if phase.busy() {
            Some(
                if state.retry_hint().is_some() && connection.progress.is_none() {
                    tr!(language, "正在重新连接…").into_owned()
                } else {
                    default.render(language).into_owned()
                },
            )
        } else {
            None
        };
        let mut lines = Vec::new();
        for (message, warn) in messages {
            let wrapped = measure.lines(&message, INNER - 8.0, Font::Regular, 12.0)?;
            lines.extend(wrapped.into_iter().map(|line| (line, warn)));
        }
        let help = guide && (connection.needs_help() || state.retry_hint().is_some());
        let connected = matches!(phase, ConnectionPhase::Connected { .. });
        let header = lines.len() as f32 * 18.0
            + if activity.is_some() {
                34.0
            } else if connected {
                8.0
            } else {
                0.0
            }
            + if driver.is_some() { 40.0 } else { 0.0 }
            + if help { 40.0 } else { 0.0 };
        let header = if header > 0.0 { header + 8.0 } else { 0.0 };
        let height = (HEIGHT + header).min(available_height).max(1.0);
        let footer = if guide { 75.0 } else { 62.0 };
        // In a very small work area a single scroll surface starts with the errors.
        // There is never a second scroll area inside the pairing steps.
        // Reserve enough room for a full action button and the adjacent text.
        let scroll_all = height < 64.0 + footer + 64.0;
        let inline = 64.0 + header + footer + 64.0 > height;
        let viewport = if scroll_all {
            Rect::new(0.0, 0.0, WIDTH, height)
        } else {
            let top = 64.0 + if inline { 0.0 } else { header };
            Rect::new(0.0, top, WIDTH, height - footer - top)
        };
        Ok(Self {
            lines,
            activity,
            connected,
            driver,
            help,
            height,
            header,
            viewport,
            inline,
            scroll_all,
        })
    }

    pub fn bounds(&self) -> PopupBounds {
        PopupBounds::new(WIDTH, self.height, 24.0)
    }
}

impl Builder<'_> {
    pub(super) fn feedback(&mut self, layout: &SettingsLayout) -> Result<f32> {
        let scrolled = layout.inline;
        let mut y = if scrolled && !layout.scroll_all {
            0.0
        } else {
            60.0
        };
        for (line, warning) in &layout.lines {
            label(
                self.scene,
                self.target(scrolled),
                line,
                Rect::new(PAD + 2.0, y, INNER - 4.0, 18.0),
                Font::Regular,
                12.0,
                if *warning {
                    THEME.disconnected.text
                } else {
                    THEME.muted
                },
            )?;
            y += 18.0;
        }
        if let Some(activity) = &layout.activity {
            label(
                self.scene,
                self.target(scrolled),
                activity,
                Rect::new(PAD + 2.0, y, INNER - 4.0, 18.0),
                Font::Regular,
                12.0,
                THEME.muted,
            )?;
            self.add_progress(y + 22.0, scrolled)?;
            y += 34.0;
        } else if layout.connected {
            self.add_progress(y, scrolled)?;
            y += 8.0;
        }
        if let Some(driver) = layout.driver {
            self.driver_buttons(driver, y + 4.0, scrolled)?;
            y += 40.0;
        }
        if layout.help {
            self.help_row(y + 4.0, scrolled)?;
        }
        Ok(if layout.scroll_all {
            64.0 + layout.header
        } else if scrolled {
            layout.header
        } else {
            0.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_page::ConnectionState;
    use copiago_core::i18n::message;

    #[test]
    fn paired_retry_keeps_the_warning_and_reserves_the_same_progress_space() {
        let mut state = SettingsState::load(
            std::env::temp_dir().join("copiago-feedback-read-only"),
            "test",
        );
        let warning = message!("iPhone 已锁定，解锁后会自动重新连接。");
        let mut connection = ConnectionState {
            phase: ConnectionPhase::Waiting,
            detail: Some(warning.clone()),
            device: Some(DeviceInfo::default()),
            ..Default::default()
        };
        state.set_connection(connection.clone());
        let waiting = SettingsLayout::measure(&state, SettingsScreen::Settings, 800.0).unwrap();
        connection.phase = ConnectionPhase::Connecting;
        connection.detail = None;
        state.set_connection(connection.clone());
        let trying = SettingsLayout::measure(&state, SettingsScreen::Settings, 800.0).unwrap();
        assert_eq!(trying.lines, waiting.lines);
        assert_eq!(trying.height, waiting.height);
        assert_eq!(trying.viewport, waiting.viewport);
        assert_ne!(trying.activity, waiting.activity);
        assert!(waiting.activity.is_some() && trying.activity.is_some());
        assert_eq!(state.retry_hint(), Some(&warning));
        connection.phase = ConnectionPhase::Connected { wireless: true };
        state.set_connection(connection);
        assert!(state.retry_hint().is_none());
    }

    #[test]
    fn repair_reason_and_action_feedback_fit_above_the_steps_in_both_languages() {
        let mut state = SettingsState::load(
            std::env::temp_dir().join("copiago-feedback-read-only"),
            "test",
        );
        state.set_connection(ConnectionState {
            driver: DriverState::ServiceUnavailable,
            ..Default::default()
        });
        state.set_diagnosis(Some(message!("端口 27015 被「{program}」占用，Apple 连接服务无法工作。请退出该程序（或在任务管理器中结束它），然后点「修复连接服务」。", program="pwsh.exe")));
        let repair_error = message!(
            "Apple 连接服务仍不可用。请重启电脑后再试；仍不行请点「手动下载安装」重新安装 Apple 组件。"
        );
        state.activate(SettingsAction::InstallDriver, Instant::now());
        state.complete_driver(DriverState::ServiceUnavailable, repair_error.clone());
        state.activate(SettingsAction::CopyDiagnostics, Instant::now());
        state.set_notice(Some(message!(
            "诊断信息已复制。可以直接粘贴发给开发者，或点「反馈问题」提交到 GitHub。"
        )));
        for language in [LanguagePreference::ZhCn, LanguagePreference::En] {
            state.settings.language = language;
            for screen in [SettingsScreen::Settings, SettingsScreen::PairingGuide] {
                let measured = SettingsLayout::measure(&state, screen, 720.0).unwrap();
                let text: String = measured
                    .lines
                    .iter()
                    .map(|(line, _)| line.as_str())
                    .collect();
                assert!(text.contains("pwsh.exe"));
                let full_text = text.split_whitespace().collect::<String>();
                assert!(
                    full_text.contains(
                        &repair_error
                            .render(state.language())
                            .split_whitespace()
                            .collect::<String>()
                    )
                );
                let last = state.notice().unwrap().render(state.language());
                assert!(full_text.contains(&last.split_whitespace().collect::<String>()));
                assert!(!measured.inline);
                assert!(measured.viewport.y >= 60.0 + measured.header);
                assert!(measured.height <= 720.0);
                if screen == SettingsScreen::PairingGuide {
                    assert!(measured.driver.is_some() && measured.help);
                    let compact = SettingsLayout::measure(&state, screen, 240.0).unwrap();
                    assert!(compact.inline && compact.viewport.y == 64.0);
                    assert_eq!(compact.lines, measured.lines);
                }
            }
        }
    }
}
