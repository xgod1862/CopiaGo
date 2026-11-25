//! The diagnostic report users copy or paste into a GitHub issue.
//!
//! It contains versions, connection states, the Apple service state and recent
//! diagnostic log lines. It never contains clipboard content, the phone's name, serial
//! numbers or file paths, because users paste it into public issues.

use std::fmt::Write as _;

use copiago_core::{
    diagnostic,
    i18n::{Language, Message, message},
};
use copiago_ui::settings_page::ConnectionPhase;

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
        let render = |message: Option<&Message>| {
            message.map_or_else(String::new, |message| {
                message.render(Language::ZhCn).into_owned()
            })
        };
        let detail = render(connection.detail.as_ref());
        let notice = render(self.settings.notice());
        let service = service::inspect();
        let mut text = String::new();
        let _ = writeln!(text, "CopiaGo 诊断信息");
        let _ = writeln!(text, "version={VERSION} {}", driver::system_summary());
        let _ = writeln!(
            text,
            "phase={:?} driver={:?} paired={}",
            connection.phase,
            connection.driver,
            connection.device.is_some()
        );
        if let Some(device) = &connection.device {
            // Model and iOS version only: the device name often contains a person's name.
            let _ = writeln!(text, "iphone={} ios={}", device.model, device.os_version);
        }
        let _ = writeln!(text, "service: {}", service.log_fields());
        let _ = writeln!(text, "detail={detail}");
        if !notice.is_empty() {
            let _ = writeln!(text, "notice={notice}");
        }
        let _ = writeln!(text, "--- recent log ---");
        for line in diagnostic::recent_lines(LOG_LINES, LOG_BYTES) {
            let _ = writeln!(text, "{line}");
        }
        text
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
    fn encodes_issue_links() {
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_encode("问"), "%E9%97%AE");
        let url = issue_url(ConnectionPhase::RetryRequired);
        assert!(url.starts_with("https://github.com/xgod1862/CopiaGo/issues/new?title=%E8%BF%9E"));
        assert!(!url.contains(' ') && !url.contains('\n'));
    }
}
