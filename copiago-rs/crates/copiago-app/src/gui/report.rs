//! The diagnostic report users copy or paste into a GitHub issue.
//!
//! It contains versions, connection states, the Apple service state and recent
//! diagnostic log lines. It never contains clipboard content, the phone's name, serial
//! numbers or file paths, because users paste it into public issues.

use std::fmt::Write as _;

use copiago_core::{
    diagnostic,
    i18n::{Message, message},
};
use copiago_ui::settings_page::{ConnectionPhase, ConnectionState};

use super::{Host, driver};
use crate::{VERSION, service};

const ISSUE_URL: &str = "https://github.com/xgod1862/CopiaGo/issues/new";
/// Enough to cover a pairing attempt and its retries while staying pasteable.
const LOG_LINES: usize = 150;
const LOG_BYTES: u64 = 32 * 1024;

impl Host<'_> {
    /// Copies the report to the clipboard and tells the user how to send it.
    pub(super) fn copy_diagnostics(&mut self) {
        let notice = if self.write_diagnostics() {
            message!("诊断信息已复制。可以直接粘贴发给开发者，或点「反馈问题」提交到 GitHub。")
        } else {
            message!("无法复制诊断信息，请稍后重试。")
        };
        self.settings.set_notice(Some(notice));
    }

    /// Copies the report, then opens a prefilled GitHub issue. Reporting there needs a
    /// GitHub account; the copied report also works for any other channel.
    pub(super) fn report_issue(&mut self) {
        let copied = self.write_diagnostics();
        let phase = self.settings.connection().phase;
        let opened = driver::open_url(&issue_url(phase)).is_ok();
        diagnostic::event(format_args!(
            "event=report_issue copied={copied} opened={opened}"
        ));
        let notice: Message = match (opened, copied) {
            (true, true) => {
                message!("已复制诊断信息并打开 GitHub。登录后在问题描述里按 Ctrl+V 粘贴即可提交。")
            }
            (true, false) => message!(
                "已打开 GitHub，但诊断信息复制失败。请先点「复制诊断信息」，再粘贴到问题描述里。"
            ),
            (false, true) => message!("无法打开浏览器。诊断信息已复制，可以直接粘贴发给开发者。"),
            (false, false) => message!("无法打开浏览器，也无法复制诊断信息，请稍后重试。"),
        };
        self.settings.set_notice(Some(notice));
    }

    fn write_diagnostics(&mut self) -> bool {
        let report = self.diagnostic_report();
        let copied = self.write_local_text(report).is_ok();
        diagnostic::event(format_args!("event=diagnostics_copied ok={copied}"));
        copied
    }

    fn diagnostic_report(&self) -> String {
        let connection = self.settings.connection();
        let service = service::inspect();
        let mut text = String::new();
        let _ = writeln!(text, "CopiaGo 诊断信息");
        let _ = writeln!(text, "version={VERSION} {}", driver::system_summary());
        write_connection(&mut text, connection, self.connection.vpn_adapter_active);
        if let Some(notice) = self
            .settings
            .repair_error()
            .or_else(|| self.settings.notice())
            .or_else(|| self.settings.diagnosis())
            .and_then(Message::literal_text)
        {
            let _ = writeln!(text, "notice={notice}");
        }
        let _ = writeln!(text, "service: {}", service.log_fields());
        let _ = writeln!(text, "--- recent log ---");
        for line in diagnostic::recent_lines(LOG_LINES, LOG_BYTES) {
            let _ = writeln!(text, "{}", redact_port_owner(&line));
        }
        text
    }
}

/// Keeps the program that holds the port out of past log lines: only the current owner,
/// in the `service:` summary, is exported. The local log keeps it for this PC.
fn redact_port_owner(line: &str) -> std::borrow::Cow<'_, str> {
    const KEY: &str = "port_owner=";
    let Some(start) = line.find(KEY).map(|index| index + KEY.len()) else {
        return line.into();
    };
    // The file name may contain spaces, so the value ends at the next known field.
    let end = line[start..]
        .find(" store_app=")
        .map_or(line.len(), |offset| start + offset);
    if &line[start..end] == "-" {
        return line.into();
    }
    format!("{}<omitted>{}", &line[..start], &line[end..]).into()
}

/// Export structured fields and fixed status text. Formatted UI messages can contain names.
fn write_connection(text: &mut String, connection: &ConnectionState, vpn_active: bool) {
    let _ = writeln!(
        text,
        "phase={:?} driver={:?} paired={}",
        connection.phase,
        connection.driver,
        connection.device.is_some()
    );
    if vpn_active && connection.phase == ConnectionPhase::Waiting {
        let _ = writeln!(text, "vpn_adapter_active=true");
    }
    if let Some(detail) = connection.detail.as_ref().and_then(Message::literal_text) {
        let _ = writeln!(text, "detail={detail}");
    }
    if let Some((stage, _)) = connection.progress {
        let _ = writeln!(text, "setup_stage={stage:?}");
    }
    if let Some(device) = &connection.device {
        let _ = writeln!(text, "iphone={} ios={}", device.model, device.os_version);
    }
}

fn issue_url(phase: ConnectionPhase) -> String {
    let title = match phase {
        ConnectionPhase::Connected { .. } => "使用问题：",
        _ => "连接问题：",
    };
    let body = concat!(
        "**遇到的问题**\n",
        "（卡在哪一步、看到什么提示、已经试过哪些办法）\n\n",
        "**诊断信息**\n",
        "（CopiaGo 已把诊断信息复制到剪贴板，请在这里按 Ctrl+V 粘贴）\n"
    );
    format!(
        "{ISSUE_URL}?title={}&body={}",
        percent_encode(title),
        percent_encode(body)
    )
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_connection_report_omits_names_from_ui_messages() {
        let connection = ConnectionState {
            phase: ConnectionPhase::Waiting,
            detail: Some(message!("VPN {adapter}", adapter = "private-project")),
            device: Some(copiago_core::DeviceInfo {
                name: "private-phone".into(),
                model: "iPhone 15".into(),
                os_version: "17.0".into(),
            }),
            ..ConnectionState::default()
        };
        let mut text = String::new();
        write_connection(&mut text, &connection, true);
        assert!(text.contains("vpn_adapter_active=true"));
        assert!(text.contains("iphone=iPhone 15 ios=17.0"));
        assert!(!text.contains("private-"));
    }

    #[test]
    fn past_log_lines_omit_the_port_owner() {
        let line = "t event=service_check diagnosis=PortTaken listening=true port_owner=My Game Server.exe store_app=false";
        assert_eq!(
            redact_port_owner(line),
            "t event=service_check diagnosis=PortTaken listening=true port_owner=<omitted> store_app=false"
        );
        // Lines written before the store_app field existed end with the owner.
        assert_eq!(
            redact_port_owner("t port_owner=srcds.exe"),
            "t port_owner=<omitted>"
        );
        let none = "t listening=false port_owner=- store_app=false";
        assert_eq!(redact_port_owner(none), none);
        assert_eq!(redact_port_owner("t event=connected"), "t event=connected");
    }

    #[test]
    fn encodes_issue_links() {
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_encode("问"), "%E9%97%AE");
        let url = issue_url(ConnectionPhase::RetryRequired);
        assert!(url.starts_with("https://github.com/xgod1862/CopiaGo/issues/new?title=%E8%BF%9E"));
        assert!(!url.contains(' ') && !url.contains('\n'));
    }
}
