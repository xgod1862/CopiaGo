use super::{
    form::{continue_label, version_label},
    *,
};
use crate::components::ButtonStyle;

const STEPS: [(&str, &str); 4] = [
    (
        "用数据线连接并解锁 iPhone",
        "使用 USB 数据线连接这台电脑，并保持手机解锁。",
    ),
    (
        "点「连接 iPhone」并信任此电脑",
        "手机弹出提示时点「信任」，并输入锁屏密码。",
    ),
    (
        "连接并检查 iPhone 服务",
        "请保持电脑联网、iPhone 解锁和数据线连接。",
    ),
    (
        "拔掉数据线，开始使用",
        "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接。",
    ),
];
/// Shown after CopiaGo asked the phone to reveal the switch, which iOS hides until a
/// developer tool requests it.
const DEVELOPER_HELP: &str = concat!(
    "打开 iPhone「设置 → 隐私与安全性」，在底部找到「开发者模式」并打开，按提示重启。",
    "重启后解锁，在弹窗中点「打开」，再点下方按钮。若仍没有该开关，可能是公司或学校管理的设备。"
);
const DRIVER_HELP: &str = "需要先安装 Apple 设备连接组件。将通过 Windows 包管理器安装 Apple Mobile Device Support，并代表你接受其许可协议。";
const SERVICE_HELP: &str = "Apple 连接服务不可用或无法读取设备。修复将请求管理员权限，启动或重启该服务，可能暂时中断其他 Apple 软件的设备连接。";

impl Builder<'_> {
    pub(super) fn guide(&mut self, complete: bool) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let title = tr!(
            language,
            if complete {
                "配对完成"
            } else {
                "连接 iPhone"
            }
        );
        label(
            scene,
            self.target(false),
            &title,
            Rect::new(PAD, 18.0, INNER - 90.0, 32.0),
            Font::Bold,
            20.0,
            THEME.ink,
        )?;
        for (position, glyph, source, action) in [
            (
                WIDTH - PAD - 72.0,
                '\u{E713}',
                "设置",
                SettingsAction::OpenSettings,
            ),
            (
                WIDTH - PAD - 32.0,
                '\u{E711}',
                "关闭",
                SettingsAction::Close,
            ),
        ] {
            let button = Button::icon(
                scene,
                Rect::new(position, 18.0, 32.0, 32.0),
                ButtonStyle::Neutral,
                glyph,
                16.0,
                &THEME,
            )?;
            self.add_button(
                button,
                tr!(language, source).into_owned(),
                action,
                false,
                ControlRole::Button,
            )?;
        }

        let connection = self.state.connection();
        let phase = connection.phase;
        let bottom = if complete {
            self.guide_done()?
        } else {
            self.guide_steps(phase)?
        };
        if !complete && connection.driver != DriverState::Ready {
            self.driver_box(connection.driver)?;
        }

        let (caption, action) = if complete {
            ("开始使用", SettingsAction::FinishPairing)
        } else if phase.busy() {
            ("正在连接…", SettingsAction::RequestPair)
        } else if phase.can_continue() {
            (continue_label(phase), SettingsAction::Retry)
        } else {
            ("连接 iPhone", SettingsAction::RequestPair)
        };
        let title = tr!(language, caption).into_owned();
        let primary = Button::text(
            scene,
            Rect::new(PAD, 411.0, INNER, 44.0),
            ButtonStyle::Primary,
            &title,
            &THEME,
        )?;
        self.add_button(primary, title, action, false, ControlRole::Button)?;
        let version = scene.text(
            &version_label(self.state.version()),
            TextStyle {
                font: Font::Regular,
                size: 12.0,
                max_width: INNER,
                color: THEME.muted,
            },
        )?;
        center(&version, Rect::new(PAD, 457.0, INNER, 19.0))?;
        self.target(false)
            .Children()?
            .InsertAtTop(&version.visual)?;
        Ok(bottom)
    }

    fn guide_steps(&mut self, phase: ConnectionPhase) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let current = match phase {
            ConnectionPhase::Pairing => 1,
            ConnectionPhase::Connecting
            | ConnectionPhase::DeveloperMode
            | ConnectionPhase::RetryRequired => 2,
            _ => 0,
        };
        let description = if phase == ConnectionPhase::DeveloperMode {
            DEVELOPER_HELP
        } else {
            STEPS[current].1
        };
        let description = scene.wrapped_text(
            &tr!(language, description),
            TextStyle {
                font: Font::Regular,
                size: 12.0,
                max_width: INNER - 70.0,
                color: THEME.muted,
            },
            5,
        )?;
        let base_height = 46.0;
        let expansion = description.size.Y + 6.0;
        let card_height = base_height * STEPS.len() as f32 + expansion + 12.0;
        background(
            scene,
            self.target(true),
            Rect::new(PAD, 2.0, INNER, card_height),
            16.0,
            THEME.card,
        )?;
        let mut y = 8.0;
        for (index, (step_title, _)) in STEPS.into_iter().enumerate() {
            let active = index == current;
            let height = base_height + if active { expansion } else { 0.0 };
            if active {
                background(
                    scene,
                    self.target(true),
                    Rect::new(PAD + 6.0, y, INNER - 12.0, height),
                    12.0,
                    THEME.white,
                )?;
            }
            self.step_number(index, current, y)?;
            let source = if active && phase == ConnectionPhase::DeveloperMode {
                "开启开发者模式并重启手机"
            } else {
                step_title
            };
            label(
                scene,
                self.target(true),
                &tr!(language, source),
                Rect::new(PAD + 54.0, y, INNER - 66.0, base_height),
                if active {
                    Font::Semibold
                } else {
                    Font::Regular
                },
                13.5,
                if index < current {
                    THEME.muted
                } else {
                    THEME.ink
                },
            )?;
            if active {
                description.visual.SetOffset(xy(PAD + 54.0, y + 39.0))?;
                self.target(true)
                    .Children()?
                    .InsertAtTop(&description.visual)?;
            }
            y += height;
        }
        y += 18.0;
        self.body.detail = Some(TextSlot::new(
            scene,
            self.target(true),
            Rect::new(PAD + 4.0, y, INNER - 8.0, 56.0),
            3,
        )?);
        if guide_detail_visible(self.state) {
            y += 58.0;
        }
        if phase.busy() {
            self.add_progress(y)?;
            y += 17.0;
        }
        if self.state.connection().needs_help() {
            y = self.help_row(y + 4.0)? + 4.0;
        }
        Ok(y)
    }

    /// Two equal buttons: copy a diagnostic report, or copy it and open a GitHub issue.
    /// Returns the bottom of the row.
    pub(super) fn help_row(&mut self, y: f32) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let width = (INNER - 8.0) / 2.0;
        let buttons = [
            ("复制诊断信息", SettingsAction::CopyDiagnostics),
            ("反馈问题", SettingsAction::ReportIssue),
        ];
        for (index, (source, action)) in buttons.into_iter().enumerate() {
            let name = tr!(language, source).into_owned();
            let button = Button::text(
                scene,
                Rect::new(PAD + index as f32 * (width + 8.0), y, width, 32.0),
                ButtonStyle::Neutral,
                &name,
                &THEME,
            )?;
            self.add_button(button, name, action, true, ControlRole::Button)?;
        }
        Ok(y + 32.0)
    }

    fn step_number(&self, index: usize, current: usize, y: f32) -> Result<()> {
        let (scene, target) = (self.scene, self.target(true));
        let bounds = Rect::new(PAD + 18.0, y + 11.0, 24.0, 24.0);
        if index < current {
            background(scene, target, bounds, 12.0, THEME.accent)?;
            return icon(scene, target, bounds, '\u{E73E}', 14.0, THEME.white);
        }
        let active = index == current;
        if active {
            background(
                scene,
                target,
                Rect::new(bounds.x - 5.0, bounds.y - 5.0, 34.0, 34.0),
                17.0,
                THEME.tint,
            )?;
        }
        let border = if active {
            THEME.accent
        } else {
            THEME.switch_off
        };
        background(scene, target, bounds, 12.0, border)?;
        let inset = if active { 2.0 } else { 1.5 };
        background(
            scene,
            target,
            Rect::new(
                bounds.x + inset,
                bounds.y + inset,
                24.0 - 2.0 * inset,
                24.0 - 2.0 * inset,
            ),
            12.0 - inset,
            if active { THEME.white } else { THEME.card },
        )?;
        let surface = scene.text(
            &(index + 1).to_string(),
            TextStyle {
                font: if active {
                    Font::Semibold
                } else {
                    Font::Regular
                },
                size: 12.0,
                max_width: bounds.width,
                color: if active { THEME.accent } else { THEME.muted },
            },
        )?;
        center(&surface, bounds)?;
        target.Children()?.InsertAtTop(&surface.visual)
    }

    fn driver_box(&mut self, driver: DriverState) -> Result<()> {
        let (scene, language) = (self.scene, self.language);
        let note = scene.wrapped_text(
            &tr!(
                language,
                if driver == DriverState::ServiceUnavailable {
                    SERVICE_HELP
                } else {
                    DRIVER_HELP
                }
            ),
            TextStyle {
                font: Font::Regular,
                size: 12.0,
                max_width: INNER - 28.0,
                color: THEME.disconnected.text,
            },
            4,
        )?;
        let height = 70.0 + note.size.Y;
        let y = GUIDE_VIEWPORT.bottom() - height;
        let buttons_y = y + 24.0 + note.size.Y;
        background(
            scene,
            self.target(false),
            Rect::new(PAD, y, INNER, height),
            16.0,
            THEME.disconnected.background,
        )?;
        note.visual.SetOffset(xy(PAD + 14.0, y + 12.0))?;
        self.target(false).Children()?.InsertAtTop(&note.visual)?;
        let name = tr!(
            language,
            if driver == DriverState::Installing {
                "正在处理…"
            } else if driver == DriverState::ServiceUnavailable {
                "修复连接服务"
            } else {
                "安装连接组件"
            },
        )
        .into_owned();
        let install = Button::text(
            scene,
            Rect::new(PAD + 14.0, buttons_y, 143.0, 32.0),
            ButtonStyle::Neutral,
            &name,
            &THEME,
        )?;
        self.add_button(
            install,
            name,
            SettingsAction::InstallDriver,
            false,
            ControlRole::Button,
        )?;
        if matches!(
            driver,
            DriverState::Failed | DriverState::ServiceUnavailable
        ) {
            let name = tr!(language, "在 Microsoft Store 中查找").into_owned();
            let store = Button::text(
                scene,
                Rect::new(PAD + 165.0, buttons_y, INNER - 179.0, 32.0),
                ButtonStyle::Neutral,
                &name,
                &THEME,
            )?;
            self.add_button(
                store,
                name,
                SettingsAction::OpenAppleDevicesStore,
                false,
                ControlRole::Button,
            )?;
        }
        Ok(())
    }

    fn guide_done(&mut self) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let check = Rect::new((WIDTH - 72.0) / 2.0, 22.0, 72.0, 72.0);
        background(scene, self.target(true), check, 36.0, THEME.tint)?;
        icon(
            scene,
            self.target(true),
            check,
            '\u{E73E}',
            34.0,
            THEME.accent,
        )?;
        let title = scene.text(
            &tr!(language, "现在可以拔掉数据线"),
            TextStyle {
                font: Font::Bold,
                size: 16.0,
                max_width: INNER,
                color: THEME.ink,
            },
        )?;
        center(&title, Rect::new(PAD, 111.0, INNER, 30.0))?;
        self.target(true).Children()?.InsertAtTop(&title.visual)?;
        let hint = scene.wrapped_text(
            &tr!(
                language,
                "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接，在手机上复制即可粘贴到电脑。"
            ),
            TextStyle {
                font: Font::Regular,
                size: 12.0,
                max_width: INNER - 24.0,
                color: THEME.muted,
            },
            3,
        )?;
        center(&hint, Rect::new(PAD + 12.0, 149.0, INNER - 24.0, 58.0))?;
        self.target(true).Children()?.InsertAtTop(&hint.visual)?;
        self.body.detail = Some(TextSlot::new(
            scene,
            self.target(true),
            Rect::new(PAD + 12.0, 224.0, INNER - 24.0, 39.0),
            2,
        )?);
        self.add_progress(270.0)?;
        Ok(285.0)
    }
}
