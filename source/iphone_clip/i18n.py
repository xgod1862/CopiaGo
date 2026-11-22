"""Interface translations and deferred status messages."""
from __future__ import annotations

import ctypes
from dataclasses import dataclass


LANGUAGE_OPTIONS = ("system", "zh-CN", "en")


def windows_language() -> str:
    """Use the Windows display language of the signed-in user."""
    try:
        language_id = ctypes.windll.kernel32.GetUserDefaultUILanguage()
    except (AttributeError, OSError):
        return "en"
    return "zh-CN" if (language_id & 0x03FF) == 0x04 else "en"


_language = windows_language()


def use_language(preference: str) -> None:
    global _language
    _language = preference if preference in ("zh-CN", "en") else windows_language()


def tr(text: str, **values: object) -> str:
    template = ENGLISH.get(text, text) if _language == "en" else text
    return template.format(**values) if values else template


@dataclass(frozen=True)
class Message:
    """Keep messages translatable until they are drawn or sent to Windows."""

    text: str
    values: dict[str, object]

    def __str__(self) -> str:
        return tr(self.text, **self.values)


def message(text: str, **values: object) -> Message:
    return Message(text, values)


ENGLISH = {
    "未连接": "Not connected",
    "已连接": "Connected",
    "连接中": "Connecting",
    "昨天 {time}": "Yesterday {time}",
    "语言": "Language",
    "跟随系统": "System",
    "iPhone → 电脑": "iPhone → PC",
    "电脑 → iPhone": "PC → iPhone",
    "自动同步到电脑": "Auto-sync to PC",
    "自动同步到 iPhone": "Auto-sync to iPhone",
    "同步内容": "Content types",
    "大文件先询问": "Large files",
    "保留 iPhone 剪贴板历史": "Keep iPhone clipboard history",
    "保留电脑剪贴板历史": "Keep PC clipboard history",
    "通用": "General",
    "登录 Windows 时启动": "Start with Windows",
    "显示同步、断开和错误通知": "Sync, connection and error alerts",
    "文字": "Text",
    "文本": "text",
    "图片": "Image",
    "文件": "File",
    "不询问": "Never",
    "电脑": "PC",
    "等待从 iPhone 同步": "Waiting for iPhone copies",
    "在 iPhone 上复制的文字和图片会出现在这里": "Text and images copied on your iPhone will appear here.",
    "iPhone 剪贴板历史未开启": "iPhone clipboard history is off",
    "开启后加密保存之后从手机收到的内容，可在设置中随时关闭": "Enable it to save future items from your iPhone, encrypted on this PC. You can turn it off in Settings.",
    "开启 iPhone 剪贴板历史": "Enable iPhone clipboard history",
    "等待你在电脑上复制": "Waiting for PC copies",
    "在电脑上复制的文字和图片会出现在这里，可随时发到 iPhone": "Text and images copied on this PC will appear here. Send an item to your iPhone when needed.",
    "电脑剪贴板历史未开启": "PC clipboard history is off",
    "开启后记录在电脑上复制的内容（跳过密码等敏感内容），可随时发到 iPhone": "Save future PC copies to send to your iPhone. Items marked as sensitive are skipped.",
    "开启电脑剪贴板历史": "Enable PC clipboard history",
    "用数据线连接并解锁 iPhone": "Connect and unlock your iPhone",
    "使用 USB 数据线连接这台电脑，并保持手机解锁。": "Connect your iPhone to this PC with a USB cable and keep it unlocked.",
    "点「连接 iPhone」并信任此电脑": "Connect and trust this PC",
    "手机弹出提示时点「信任」，并输入锁屏密码。": "Tap Trust on your iPhone when prompted and enter your passcode.",
    "准备开发者服务": "Prepare developer services",
    "首次需要下载文件，可能要几分钟，请保持联网和手机解锁。": "The first setup may take a few minutes to download files. Stay online and keep the iPhone unlocked.",
    "拔掉数据线，开始使用": "Unplug the cable",
    "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接。": "Your iPhone and PC will connect automatically on the same local network.",
    "开发者模式未开启。在 iPhone「设置 → 隐私与安全性 → 开发者模式」中打开开关，按提示重启手机；重启后解锁，在弹窗中点「打开」，然后点下方按钮。": "On your iPhone, open Settings → Privacy & Security → Developer Mode. Turn it on and restart the phone. Unlock it, tap Turn On when prompted, then continue below.",
    "我已开启，继续连接": "Developer Mode is on. Continue",
    "继续连接": "Continue",
    "连接并信任你的 iPhone": "Connect and trust your iPhone",
    "设置": "Settings",
    "剪贴板": "Clipboard",
    "{count} 条 · 加密保存在本机": "{count} saved · encrypted on this PC",
    "已开启 · 加密保存在本机": "History on · encrypted on this PC",
    "未保留 iPhone 剪贴板历史": "iPhone history is off",
    "未保留电脑剪贴板历史": "PC history is off",
    "确认清空": "Confirm clear",
    "清空": "Clear",
    "同步中…": "Syncing…",
    "同步到电脑": "Sync to PC",
    "忽略": "Ignore",
    "已复制": "Copied",
    "已发送到 iPhone": "Sent to iPhone",
    "确认忘记": "Confirm forget",
    "忘记此 iPhone": "Forget this iPhone",
    "配对完成": "Pairing complete",
    "连接 iPhone": "Connect iPhone",
    "现在可以拔掉数据线": "You can unplug the cable",
    "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接，在手机上复制即可粘贴到电脑。": "Your iPhone and PC will connect on the same local network. Copy on your iPhone and paste on your PC.",
    "开始使用": "Get started",
    "开启开发者模式并重启手机": "Enable Developer Mode and restart",
    "需要先安装 Apple 设备连接组件。将通过 Windows 包管理器安装 Apple Mobile Device Support，并代表你接受其许可协议。": "Apple Mobile Device Support is required. Windows Package Manager will install it and accept its license agreement on your behalf.",
    "正在安装…": "Installing…",
    "安装连接组件": "Install Apple support",
    "在 Microsoft Store 中查找": "Open Microsoft Store",
    "正在连接…": "Connecting…",
    "正在连接 iPhone…": "Connecting to iPhone…",
    "剪贴板历史": "Clipboard history",
    "使用 iPhone 上最新复制的内容": "Use latest iPhone copy",
    "连接与设置": "Connection & settings",
    "退出": "Quit",
    "无法删除配对记录": "Could not remove the pairing record.",
    "已忘记 iPhone。请用数据线连接后重新配对": "iPhone forgotten. Reconnect the USB cable to pair again.",
    "正在安装 Apple 连接组件，请完成 Windows 提示。": "Installing Apple device support. Follow the Windows prompts.",
    "连接组件已就绪，可以连接 iPhone。": "Apple device support is ready. You can connect your iPhone.",
    "安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。": "Installation did not finish. Install Apple Devices from Microsoft Store and try again.",
    "无法运行 Windows 包管理器。可以在 Microsoft Store 安装「Apple Devices」后重试。": "Could not run Windows Package Manager. Install Apple Devices from Microsoft Store and try again.",
    "无法打开 Microsoft Store": "Could not open Microsoft Store.",
    "无法读取历史": "Could not read clipboard history.",
    "无法复制这条历史": "Could not copy this history item.",
    "无法删除这条历史": "Could not delete this history item.",
    "无法清空历史": "Could not clear clipboard history.",
    "无法保存同步设置": "Could not save sync settings.",
    "至少保留一种同步内容": "Keep at least one content type enabled.",
    "无法保存历史设置": "Could not save history settings.",
    "无法保存通知设置": "Could not save notification settings.",
    "无法保存语言设置": "Could not save the language preference.",
    "iPhone 未连接，无法发送": "Connect your iPhone before sending items.",
    "无法读取这条历史": "Could not read this history item.",
    "发送到 iPhone 失败": "Could not send to iPhone.",
    "已发送{kind}到 iPhone": "Sent {kind} to iPhone.",
    "无法更改开机启动": "Could not change the startup setting.",
    "同步进程已停止，请重新打开应用": "Sync has stopped. Reopen CopiaGo.",
    "大小未知": "Unknown size",
    "「{name}」{size}，超过 512 MB 上限，未同步": "“{name}” ({size}) exceeds the 512 MB limit and was skipped.",
    "超过 {limit} MB，要同步到电脑吗？": "Over {limit} MB. Sync to PC?",
    "大小未知，要同步到电脑吗？": "Unknown size. Sync to PC?",
    "已关闭自动同步，需要时点「同步到电脑」": "Auto-sync is off. Select Sync to PC to transfer.",
    "收到文件「{name}」{size}，等你选择是否同步": "Waiting for your decision on “{name}” ({size}).",
    "iPhone 上复制了「{name}」（{size}）。打开剪贴板面板选择是否同步": "“{name}” ({size}) was copied on your iPhone. Open the tray panel to choose whether to sync it.",
    "已忽略「{name}」": "Ignored “{name}”.",
    "正在同步「{name}」…": "Syncing “{name}”…",
    "正在同步「{name}」{size}…": "Syncing “{name}” ({size})…",
    "iPhone 未连接，无法同步文件": "Connect your iPhone before syncing a file.",
    "手机上已复制了别的内容，这个文件不再可用": "The file is no longer available because something else was copied on your iPhone.",
    "「{name}」太大，未能同步": "“{name}” was too large to sync.",
    "同步「{name}」失败": "Could not sync “{name}”.",
    "无法把「{name}」放进电脑剪贴板": "Could not put “{name}” on the Windows clipboard.",
    "已同步「{name}」· 可直接粘贴{note}": "Synced “{name}” · Ready to paste{note}",
    "iPhone 上复制了新的{kind} · 未自动写入电脑，可在面板或托盘菜单中使用{note}": "New {kind} copied on iPhone · Available from the panel or tray menu{note}",
    "iPhone 断线期间复制的{kind}未覆盖电脑剪贴板，可从托盘菜单使用{note}": "The PC clipboard was kept. Use the tray menu to retrieve {kind} copied while the iPhone was disconnected{note}",
    "图片过大，未同步到电脑": "The image was too large to sync to this PC.",
    "写入 Windows 剪贴板失败": "Could not write to the Windows clipboard.",
    "已同步{kind} · 可直接粘贴{note}": "Synced {kind} · Ready to paste{note}",
    " · 项目过大，未加入历史": " · Item too large for history",
    " · 历史保存失败": " · Could not save to history",
    "已使用 iPhone 上的{kind} · 可直接粘贴": "Loaded {kind} from iPhone · Ready to paste",
    "已重新连接 iPhone": "Reconnected to iPhone.",
    "iPhone 已断开 {minutes} 分钟，正在自动重连。请确认手机已解锁并在同一局域网": "iPhone disconnected {minutes} minutes ago. Reconnecting. Keep your iPhone unlocked and on the same local network.",
    "iPhone 已连接成功。复制即可同步到电脑。": "iPhone connected. Copy on your iPhone to sync to this PC.",
    "iPhone 已连接成功。现在可以拔掉数据线，之后在同一 Wi-Fi 下自动连接。": "iPhone connected. Unplug the cable to continue over Wi-Fi.",
    "已通过 Wi-Fi 连接 iPhone，在手机上复制即可同步到电脑": "iPhone connected over Wi-Fi. Copy on your iPhone to sync to this PC.",
    "{status}（上次连接 {time}）": "{status} (last connected at {time})",
    "未找到 USB 连接的 iPhone。请用数据线连接并解锁后重试": "No USB-connected iPhone found. Connect and unlock your iPhone, then try again.",
    "请暂时只连接一台 iPhone": "Connect only one iPhone to continue.",
    "视频": "Video",
    "文件或其他内容": "File or other content",
    "等待 Apple 设备连接服务或 iPhone…": "Waiting for the Apple device service or your iPhone…",
    "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」": "Enable Developer Mode in Settings → Privacy & Security on your iPhone. Restart and unlock the phone, then select Continue.",
    "iPhone 上选择了「不信任」。请重新插拔数据线，在提示时点「信任」，再点「连接 iPhone」": "Trust was declined on the iPhone. Reconnect the USB cable, tap Trust when asked, then select Connect iPhone.",
    "请先解锁 iPhone，再点「连接 iPhone」": "Unlock your iPhone, then select Connect iPhone.",
    "未完成配对：请在 iPhone 上点「信任」并输入锁屏密码，然后再点「连接 iPhone」": "Pairing is pending. Tap Trust on your iPhone and enter your passcode, then select Connect iPhone again.",
    "配对未完成：请保持 iPhone 解锁并连着数据线，然后再点「连接 iPhone」": "Pairing did not finish. Keep your iPhone connected and unlocked, then select Connect iPhone again.",
    "等待 iPhone 连接；请检查解锁、信任及同一局域网": "Waiting for iPhone. Make sure it is unlocked, trusts this PC, and is on the same local network.",
    "正在配对：请解锁 iPhone，出现提示时点「信任」并输入锁屏密码…": "Pairing… Unlock your iPhone, tap Trust when prompted, and enter your passcode.",
    "已配对多台 iPhone。请在设置中点「忘记此 iPhone」后重新配对": "More than one iPhone is paired. Select Forget this iPhone in Settings, then pair again.",
    "连接未完成：请保持 iPhone 解锁并连着数据线，然后点「继续连接」": "Connection did not finish. Keep your iPhone connected and unlocked, then select Continue.",
    "正在建立无线连接…": "Connecting over Wi-Fi…",
    "正在建立USB连接…": "Connecting over USB…",
    "正在准备 iPhone 开发者服务。首次需要下载文件，可能要几分钟，请保持联网…": "Preparing iPhone developer services. The first setup may need a few minutes to download files. Stay online…",
    "已连接（无线）": "Connected (Wi-Fi)",
    "已连接（USB） · 可以拔掉数据线，之后将通过无线连接": "Connected (USB) · Unplug the cable to continue wirelessly",
    "iPhone 上复制了{kind}，目前只同步文字、图片和「文件」App 里的文件": "Unsupported item ({kind}). CopiaGo syncs text, images, and files from the Files app.",
    "iPhone 文件 {timestamp}.{extension}": "iPhone file {timestamp}.{extension}",
}

