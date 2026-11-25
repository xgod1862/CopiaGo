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
pub(super) const DEVELOPER_HELP: &str = concat!(
    "打开 iPhone「设置 → 隐私与安全性」，在底部找到「开发者模式」并打开，按提示重启。",
    "重启后解锁，在弹窗中点「打开」，再点下方按钮。若仍没有该开关，可能是公司或学校管理的设备。"
);
const DRIVER_HELP: &str = "需要先安装 Apple 设备连接组件。将通过 Windows 包管理器安装 Apple Mobile Device Support，并代表你接受其许可协议。";
const SERVICE_HELP: &str = "Apple 连接服务不可用或无法读取设备。修复将请求管理员权限，启动或重启该服务并恢复为自动启动，可能暂时中断其他 Apple 软件的设备连接。";
const USB_DRIVER_HELP: &str = "电脑检测到了 iPhone，但 Apple 的 USB 驱动没有正常工作。修复将请求管理员权限，为这台电脑重新安装 Apple 自带的驱动。";
const APPLE_DEVICES_HELP: &str = "这台电脑使用 Microsoft Store 版 Apple Devices，它的设备连接进程没有运行。打开 Apple Devices 即可启动它，不需要管理员权限，也不需要另外安装组件。";

pub(super) fn driver_help(driver: DriverState) -> &'static str {
    match driver {
        DriverState::ServiceUnavailable => SERVICE_HELP,
        DriverState::UsbDriver => USB_DRIVER_HELP,
        DriverState::AppleDevicesApp => APPLE_DEVICES_HELP,
        DriverState::Installing => "正在处理…",
        _ => DRIVER_HELP,
    }
}

impl Builder<'_> {
    pub(super) fn guide(&mut self, complete: bool, start: f32) -> Result<f32> {
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
            self.guide_done(start)?
        } else {
            self.guide_steps(phase, start)?
        };
        let (caption, action) = if complete {
            ("开始使用", SettingsAction::FinishPairing)
        } else if phase.busy() {
            ("正在连接…", SettingsAction::RequestPair)
        } else if phase.can_continue() {
            (continue_label(phase), SettingsAction::Retry)
        } else {
            ("连接 iPhone", SettingsAction::RequestPair)
        };
        let footer_bottom = if self.body.scroll_all {
            bottom + 75.0
        } else {
            self.body.height
        };
        let title = tr!(language, caption).into_owned();
        let primary = Button::text(
            scene,
            Rect::new(PAD, footer_bottom - 67.0, INNER, 44.0),
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
        center(&version, Rect::new(PAD, footer_bottom - 21.0, INNER, 19.0))?;
        self.target(false)
            .Children()?
            .InsertAtTop(&version.visual)?;
        Ok(if self.body.scroll_all {
            footer_bottom
        } else {
            bottom
        })
    }

    fn guide_steps(&mut self, phase: ConnectionPhase, start: f32) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let current = match phase {
            ConnectionPhase::Pairing => 1,
            ConnectionPhase::Connecting
            | ConnectionPhase::DeveloperMode
            | ConnectionPhase::RetryRequired => 2,
            _ => 0,
        };
        let width = INNER - 66.0;
        let description = self.measure.lines(
            &tr!(language, STEPS[current].1),
            width - 4.0,
            Font::Regular,
            12.0,
        )?;
        let expansion = description.len() as f32 * 18.0 + 6.0;
        let mut steps = Vec::new();
        for (index, (title, _)) in STEPS.into_iter().enumerate() {
            let active = index == current;
            let font = if active {
                Font::Semibold
            } else {
                Font::Regular
            };
            let source = if active && phase == ConnectionPhase::DeveloperMode {
                "开启开发者模式并重启手机"
            } else {
                title
            };
            let lines = self
                .measure
                .lines(&tr!(language, source), width - 4.0, font, 13.5)?;
            let title_height = (lines.len() as f32 * 20.0 + 12.0).max(46.0);
            steps.push((lines, font, title_height));
        }
        let card_height = steps.iter().map(|(_, _, h)| h).sum::<f32>() + expansion + 12.0;
        background(
            scene,
            self.target(true),
            Rect::new(PAD, start + 2.0, INNER, card_height),
            16.0,
            THEME.card,
        )?;
        let mut y = start + 8.0;
        for (index, (titles, font, base_height)) in steps.into_iter().enumerate() {
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
            let title_y = y + (base_height - titles.len() as f32 * 20.0) / 2.0;
            for (i, line) in titles.iter().enumerate() {
                label(
                    scene,
                    self.target(true),
                    line,
                    Rect::new(PAD + 54.0, title_y + i as f32 * 20.0, width, 20.0),
                    font,
                    13.5,
                    if index < current {
                        THEME.muted
                    } else {
                        THEME.ink
                    },
                )?;
            }
            if active {
                for (i, line) in description.iter().enumerate() {
                    label(
                        scene,
                        self.target(true),
                        line,
                        Rect::new(
                            PAD + 54.0,
                            y + base_height - 4.0 + i as f32 * 18.0,
                            width,
                            18.0,
                        ),
                        Font::Regular,
                        12.0,
                        THEME.muted,
                    )?;
                }
            }
            y += height;
        }
        Ok(y + 8.0)
    }

    /// Two equal buttons: copy a diagnostic report, or copy it and open a GitHub issue.
    /// Returns the bottom of the row.
    pub(super) fn help_row(&mut self, y: f32, scrolled: bool) -> Result<f32> {
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
            self.add_button(button, name, action, scrolled, ControlRole::Button)?;
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

    pub(super) fn driver_buttons(
        &mut self,
        driver: DriverState,
        buttons_y: f32,
        scrolled: bool,
    ) -> Result<()> {
        let (scene, language) = (self.scene, self.language);
        let name = tr!(
            language,
            match driver {
                DriverState::Installing => "正在处理…",
                DriverState::ServiceUnavailable => "修复连接服务",
                DriverState::UsbDriver => "修复 Apple 驱动",
                DriverState::AppleDevicesApp => "打开 Apple Devices",
                _ => "安装连接组件",
            },
        )
        .into_owned();
        let install = Button::text(
            scene,
            Rect::new(PAD, buttons_y, (INNER - 8.0) / 2.0, 32.0),
            ButtonStyle::Neutral,
            &name,
            &THEME,
        )?;
        self.add_button(
            install,
            name,
            SettingsAction::InstallDriver,
            scrolled,
            ControlRole::Button,
        )?;
        if matches!(
            driver,
            DriverState::Missing
                | DriverState::Failed
                | DriverState::ServiceUnavailable
                | DriverState::UsbDriver
        ) {
            // Microsoft Store when present, otherwise Apple's own installer: works on
            // systems without winget or the Store.
            let name = tr!(language, "手动下载安装").into_owned();
            let store = Button::text(
                scene,
                Rect::new(
                    PAD + (INNER + 8.0) / 2.0,
                    buttons_y,
                    (INNER - 8.0) / 2.0,
                    32.0,
                ),
                ButtonStyle::Neutral,
                &name,
                &THEME,
            )?;
            self.add_button(
                store,
                name,
                SettingsAction::OpenAppleDevicesStore,
                scrolled,
                ControlRole::Button,
            )?;
        }
        Ok(())
    }

    fn guide_done(&mut self, start: f32) -> Result<f32> {
        let (scene, language) = (self.scene, self.language);
        let check = Rect::new((WIDTH - 72.0) / 2.0, start + 22.0, 72.0, 72.0);
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
        center(&title, Rect::new(PAD, start + 111.0, INNER, 30.0))?;
        self.target(true).Children()?.InsertAtTop(&title.visual)?;
        let hint_width = INNER - 24.0;
        let hint = self.measure.lines(
            &tr!(
                language,
                "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接，在手机上复制即可粘贴到电脑。"
            ),
            hint_width - 4.0,
            Font::Regular,
            12.0,
        )?;
        let mut y = start + 158.0;
        for line in hint {
            let surface = scene.text(
                &line,
                TextStyle {
                    font: Font::Regular,
                    size: 12.0,
                    max_width: hint_width,
                    color: THEME.muted,
                },
            )?;
            center(&surface, Rect::new(PAD + 12.0, y, hint_width, 18.0))?;
            self.target(true).Children()?.InsertAtTop(&surface.visual)?;
            y += 18.0;
        }
        Ok(y + 18.0)
    }
}
