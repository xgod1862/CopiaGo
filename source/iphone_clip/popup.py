"""Tray history popup, drawn with Pillow and shown as a rounded, shadowed layered window."""
from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from functools import lru_cache
from pathlib import Path
import io
import os
import time
import tkinter as tk
from typing import Callable

from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageOps

from iphone_clip import APP_NAME
from iphone_clip.files import human_size
from iphone_clip.i18n import tr
from iphone_clip.layered import LayeredWindow

SUPERSAMPLE = 4
WHITE = (255, 255, 255, 255)
INK = (23, 32, 43, 255)
MUTED = (93, 104, 117, 255)
CARD = (244, 246, 249, 255)
CARD_HOVER = (235, 239, 243, 255)
CONTROL = (241, 243, 246, 255)
CONTROL_HOVER = (228, 233, 238, 255)
ACCENT = (15, 110, 90, 255)
ACCENT_HOVER = (11, 88, 72, 255)
TINT = (227, 241, 236, 255)
LINK_TINT = (230, 235, 243, 255)
LINK_INK = (61, 90, 138, 255)
DANGER_TINT = (253, 236, 232, 255)
DANGER = (176, 42, 27, 255)
SOFT_INK = (59, 70, 82, 255)
SWITCH_OFF = (211, 217, 224, 255)
CHIPS = {  # state: (background, text, dot, label)
    "ok": ((227, 241, 236, 255), (12, 90, 73, 255), (31, 157, 118, 255), "已连接"),
    "busy": ((238, 241, 244, 255), MUTED, (138, 151, 164, 255), "连接中"),
    "warn": ((251, 240, 220, 255), (138, 83, 0, 255), (208, 138, 26, 255), "未连接"),
}
TONES = {"ok": (12, 90, 73, 255), "muted": MUTED, "warn": (138, 83, 0, 255)}

CLOSING = "，。、；：！？）」』”’…》"
WIDTH, RADIUS, PAD, LIST_HEIGHT, ROW, GAP, SHADOW_MARGIN = 392, 24, 18, 310, 60, 8, 40


def short_time(created: str) -> str:
    try:
        moment = datetime.fromisoformat(created)
    except ValueError:
        return ""
    today = datetime.now().date()
    days = (today - moment.date()).days
    if days == 0:
        return moment.strftime("%H:%M")
    if days == 1:
        return tr("昨天 {time}", time=moment.strftime("%H:%M"))
    return moment.strftime("%m-%d" if moment.year == today.year else "%Y-%m-%d")


@lru_cache(maxsize=32)
def font(pixels: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    path = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts", "msyhbd.ttc" if bold else "msyh.ttc")
    try:
        return ImageFont.truetype(path, pixels)
    except OSError:
        bundled = Path(__file__).with_name("assets") / "NotoSansSC.ttf"
        try:
            face = ImageFont.truetype(bundled, pixels)
            face.set_variation_by_axes([700 if bold else 400])
            return face
        except OSError:
            return ImageFont.load_default(pixels)


def fit(text: str, face: ImageFont.FreeTypeFont, width: float) -> str:
    """Trim text to width, ending with an ellipsis when it had to be cut."""
    if face.getlength(text) <= width:
        return text
    low, high = 0, len(text)
    while low < high:
        middle = (low + high + 1) // 2
        if face.getlength(text[:middle] + "…") <= width:
            low = middle
        else:
            high = middle - 1
    return text[:low].rstrip() + "…"


def wrap(text: str, face: ImageFont.FreeTypeFont, width: float, max_lines: int) -> list[str]:
    """Wrap CJK by character and keep Latin words together when they fit."""
    lines, current = [], ""
    for character in text:
        # Keep closing punctuation with its preceding text.
        if current and face.getlength(current + character) > width and character not in CLOSING:
            split = current.rfind(" ")
            if split > 0 and character.isascii() and not character.isspace() and current[split + 1:].isascii():
                lines.append(current[:split].rstrip())
                current = current[split + 1:] + character
            else:
                lines.append(current.rstrip())
                current = character.lstrip()
        else:
            current += character
    if current:
        lines.append(current.rstrip())
    if len(lines) > max_lines:
        lines = lines[:max_lines - 1] + [fit(lines[max_lines - 1] + "…", face, width)]
    return lines or [""]


@lru_cache(maxsize=256)
def shape(width: int, height: int, radius: float, fill: tuple, outline: tuple | None = None,
          stroke: float = 0) -> Image.Image:
    """Anti-aliased rounded rectangle, drawn large and scaled down."""
    k = SUPERSAMPLE
    image = Image.new("RGBA", (width * k, height * k), (0, 0, 0, 0))
    ImageDraw.Draw(image).rounded_rectangle((0, 0, width * k - 1, height * k - 1), round(radius * k), fill=fill,
                                            outline=outline, width=round(stroke * k) if outline else 0)
    return image.resize((width, height), Image.LANCZOS)


@lru_cache(maxsize=48)
def shadow(width: int, height: int, radius: int, margin: int, blur: float, offset: int, color: tuple) -> Image.Image:
    """Soft drop shadow for a (width x height) rounded box, padded by margin on every side."""
    image = Image.new("RGBA", (width + margin * 2, height + margin * 2), (0, 0, 0, 0))
    ImageDraw.Draw(image).rounded_rectangle((margin, margin + offset, margin + width, margin + height + offset),
                                            radius, fill=color)
    return image.filter(ImageFilter.GaussianBlur(blur))


@lru_cache(maxsize=64)
def icon(name: str, size: int, color: tuple, background: tuple | None = None) -> Image.Image:
    """Stroke icons on a 24-unit grid."""
    k = SUPERSAMPLE
    image = Image.new("RGBA", (size * k, size * k), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    unit = size * k / 24
    weight = max(2, round(2 * unit))

    def at(*points):
        return [(x * unit, y * unit) for x, y in points]

    def line(*points):
        draw.line(at(*points), fill=color, width=weight, joint="curve")
        for x, y in at(points[0], points[-1]):
            draw.ellipse((x - weight / 2, y - weight / 2, x + weight / 2, y + weight / 2), fill=color)

    def box(x0, y0, x1, y1, radius, fill=None):
        draw.rounded_rectangle(at((x0, y0), (x1, y1)), radius * unit, outline=color, width=weight, fill=fill)

    if name == "text":
        line((5, 6), (19, 6))
        line((5, 12), (19, 12))
        line((5, 18), (14, 18))
    elif name == "image":
        box(3, 4, 21, 20, 2.5)
        draw.ellipse(at((7, 8), (11, 12)), outline=color, width=weight)
        line((20, 16), (15.5, 11.5), (7, 20))
    elif name == "link":
        box(2, 9, 13, 15, 3)
        box(11, 9, 22, 15, 3)
        image = image.rotate(45, resample=Image.BICUBIC, center=(12 * unit, 12 * unit))
    elif name == "copy":
        box(3, 3, 15, 15, 2.5)
        box(9, 9, 21, 21, 2.5, fill=background)
    elif name == "trash":
        line((4, 7), (20, 7))
        line((9, 7), (9, 4), (15, 4), (15, 7))
        line((6, 7), (7, 20), (17, 20), (18, 7))
    elif name == "sliders":
        line((4, 7), (12, 7))
        line((19, 7), (20, 7))
        draw.ellipse(at((13, 4.5), (18, 9.5)), outline=color, width=weight)
        line((4, 17), (6, 17))
        line((13, 17), (20, 17))
        draw.ellipse(at((7, 14.5), (12, 19.5)), outline=color, width=weight)
    elif name == "check":
        line((5, 12.5), (10, 17.5), (20, 7))
    elif name == "close":
        line((6, 6), (18, 18))
        line((18, 6), (6, 18))
    elif name == "back":
        line((15, 5), (8, 12), (15, 19))
    elif name == "phone":
        box(6.5, 2, 17.5, 22, 3)
        line((11, 18.5), (13, 18.5))
    elif name == "file":
        line((5, 3), (14, 3), (19, 8), (19, 21), (5, 21), (5, 3))
        line((14, 3), (14, 8), (19, 8))
    elif name == "upload":
        line((12, 16), (12, 4))
        line((7, 9), (12, 4), (17, 9))
        line((5, 20), (19, 20))
    elif name == "clipboard":
        box(5, 5, 19, 21, 2.5)
        box(9, 3, 15, 7, 1.5, fill=background)
    return image.resize((size, size), Image.LANCZOS)


@dataclass
class Row:
    entry: object
    y: int
    height: int
    lines: list[str]


# Settings page: one section per direction, then general. Rows are (kind, key, label).
SECTIONS = (
    (None, (("types", "language", "语言"),)),
    ("iPhone → 电脑", (("switch", "pull", "自动同步到电脑"), ("types", "pull_types", "同步内容"),
                     ("types", "ask_limit", "大文件先询问"), ("switch", "history", "保留 iPhone 剪贴板历史"))),
    ("电脑 → iPhone", (("switch", "push", "自动同步到 iPhone"), ("types", "push_types", "同步内容"),
                     ("switch", "pc_history", "保留电脑剪贴板历史"))),
    ("通用", (("switch", "startup", "登录 Windows 时启动"), ("switch", "notify", "显示同步、断开和错误通知"),
            ("switch", "update_check", "启动时检查新版本"))),
)
CHOICES = {  # chip rows: key -> (value, label)
    "language": (("system", "跟随系统"), ("zh-CN", "简体中文"), ("en", "English")),
    "pull_types": (("text", "文字"), ("image", "图片"), ("file", "文件")),
    "push_types": (("text", "文字"), ("image", "图片")),
    "ask_limit": ((10, "10MB"), (20, "20MB"), (50, "50MB"), (0, "不询问")),
}
TABS = (("phone", "iPhone"), ("pc", "电脑"))
TAB_SPACE = 42  # the tab switch between the header and the list
EMPTY_TEXT = {  # tab: ((title, hint) when on, (title, hint) when off, button)
    "phone": (("等待从 iPhone 同步", "在 iPhone 上复制的文字和图片会出现在这里"),
              ("iPhone 剪贴板历史未开启", "开启后加密保存之后从手机收到的内容，可在设置中随时关闭"),
              "开启 iPhone 剪贴板历史"),
    "pc": (("等待你在电脑上复制", "在电脑上复制的文字和图片会出现在这里，可随时发到 iPhone"),
           ("电脑剪贴板历史未开启", "开启后记录在电脑上复制的内容（跳过密码等敏感内容），可随时发到 iPhone"),
           "开启电脑剪贴板历史"),
}
STEPS = (
    ("用数据线连接并解锁 iPhone", "使用 USB 数据线连接这台电脑，并保持手机解锁。"),
    ("点「连接 iPhone」并信任此电脑", "手机弹出提示时点「信任」，并输入锁屏密码。"),
    ("准备开发者服务", "请保持电脑联网、iPhone 解锁和数据线连接。"),
    ("拔掉数据线，开始使用", "之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接。"),
)
PHASE_STEP = {"idle": 0, "pairing": 1, "preparing": 2, "developer": 2, "retry": 2}
DEVELOPER_HELP = (
    "开发者模式未开启。在 iPhone「设置 → 隐私与安全性 → 开发者模式」中打开开关，按提示重启手机；"
    "重启后解锁，在弹窗中点「打开」，然后点下方按钮。"
)
CONTINUE_LABELS = {"developer": "我已开启，继续连接", "retry": "继续连接"}
IDLE_PROMPT = "连接并信任你的 iPhone"


class HistoryPopup:
    """Owns drawing, hit-testing and input for the history and settings pages; the caller owns the data."""

    def __init__(self, master: tk.Misc, scale: float, *, on_copy: Callable[[int], None],
                 on_delete: Callable[[int], None], on_clear: Callable[[str], None],
                 on_enable_history: Callable[[str], None], on_toggle: Callable[[str], None],
                 on_forget: Callable[[], None], on_type: Callable[[str, str], None] = lambda *_: None,
                 on_pair: Callable[[], None] = lambda: None,
                 on_install_driver: Callable[[], None] = lambda: None, on_open_store: Callable[[], None] = lambda: None,
                 on_finish: Callable[[], None] = lambda: None,                  on_send_item: Callable[[int], None] = lambda _: None,
                 on_offer: Callable[[bool], None] = lambda accept: None,
                 on_open_update: Callable[[], None] = lambda: None,
                 version: str = "") -> None:
        self.scale = scale
        self.on_copy, self.on_delete, self.on_clear = on_copy, on_delete, on_clear
        self.on_enable_history, self.on_toggle, self.on_forget = on_enable_history, on_toggle, on_forget
        self.on_pair, self.on_install_driver, self.on_open_store = on_pair, on_install_driver, on_open_store
        self.on_finish, self.on_send_item = on_finish, on_send_item
        self.on_type, self.on_offer = on_type, on_offer
        self.offer: dict | None = None  # a file from the iPhone waiting for "sync" or "ignore"
        self.settings_scroll = 0
        self.tab = "phone"
        self.user_selected = False
        self.lists: dict[str, tuple[list, bool]] = {"phone": ([], False), "pc": ([], False)}
        self.sent_id = None
        self.version = version
        self.on_open_update = on_open_update
        self.update: str | None = None  # a newer release's version, e.g. "1.2.0"
        self.page = "history"
        self.back_page = "history"
        self.onboarding = {"phase": "idle", "driver": "ok", "retry": False}
        self.settings: dict = {}
        self.shadow_window = LayeredWindow(master, click_through=True)
        self.window = LayeredWindow(master)
        self.entries: list = []
        self.history_on = False
        self.thumbnails: dict[int, Image.Image] = {}
        self.selected = 0
        self.scroll = 0
        self.hover = None
        self.pressed = None
        self.copied_id = None
        self.confirming = None  # the two-step action waiting for its second click
        self.confirm_job = None
        self.chip = "busy"
        self.detail, self.tone, self.connection = "", "muted", ""
        self.device_name, self.device_summary = "", ""  # the paired phone, e.g. "小明的 iPhone", "iPhone 15 Pro · iOS 18.1"
        self.progressing = False
        self.fraction: float | None = None
        self.hits: list[tuple[int, int, int, int, tuple]] = []
        top = self.window.top
        top.bind("<Motion>", self.motion)
        top.bind("<Leave>", lambda _: self.set_hover(None))
        top.bind("<ButtonPress-1>", self.press)
        top.bind("<ButtonRelease-1>", self.release)
        top.bind("<Double-Button-1>", self.double_click)
        top.bind("<MouseWheel>", lambda e: self.scroll_by(-e.delta / 120 * self.px(56)))
        top.bind("<Up>", lambda _: self.move(-1))
        top.bind("<Left>", lambda _: self.switch_tab("phone"))
        top.bind("<Right>", lambda _: self.switch_tab("pc"))
        top.bind("<Down>", lambda _: self.move(1))
        top.bind("<Return>", lambda _: self.primary())
        top.bind("<Delete>", lambda _: self.delete_selected())
        top.bind("<FocusOut>", lambda _: top.after(150, self.dismiss_if_unfocused))

    # --- data in -------------------------------------------------------------------------
    @property
    def visible(self) -> bool:
        return self.window.visible

    def px(self, value: float) -> int:
        return round(value * self.scale)

    def set_entries(self, lists: dict[str, tuple[list, bool]]) -> None:
        """``lists``: for "phone" and "pc", the entries and whether that history is switched on."""
        selected_id = self.entries[self.selected].id if 0 <= self.selected < len(self.entries) else None
        self.lists = lists
        entries, history_on = lists[self.tab]
        self.entries, self.history_on = entries, history_on
        all_ids = {entry.id for items, _ in lists.values() for entry in items}
        ids = [entry.id for entry in entries]
        if not self.user_selected:
            self.selected = 0  # follow the newest item until the user picks one
        elif selected_id in ids:
            self.selected = ids.index(selected_id)
        else:
            self.selected = min(self.selected, len(entries) - 1)
        self.selected = max(self.selected, 0)
        self.thumbnails = {key: value for key, value in self.thumbnails.items() if key in all_ids}
        self.render()

    def switch_tab(self, tab: str) -> None:
        if tab == self.tab or self.page != "history":
            return
        self.tab, self.selected, self.scroll, self.hover, self.user_selected = tab, 0, 0, None, False
        self.entries, self.history_on = self.lists[tab]
        self.reset_confirm()
        self.render()

    def set_offer(self, offer: dict | None) -> None:
        """``offer``: name, meta ("PPTX · 19.0 MB"), reason, busy; None removes the card."""
        if offer != self.offer:
            self.offer = offer
            if offer is not None and self.page == "history":
                self.switch_tab("phone")
            self.render()

    def banner_height(self) -> int:
        return self.px(104) if self.offer is not None and self.tab == "phone" else 0

    def sent(self, item_id: int) -> None:
        self.sent_id = item_id
        self.render()

    def set_status(self, detail: str, chip: str, tone: str, connection: str = "", *,
                   progressing: bool = False, fraction: float | None = None) -> None:
        """Update status text and the optional progress bar. An unknown fraction animates."""
        status = (detail, chip, tone, connection, progressing, fraction)
        if status != (self.detail, self.chip, self.tone, self.connection, self.progressing, self.fraction):
            self.detail, self.chip, self.tone, self.connection, self.progressing, self.fraction = status
            self.render()

    def set_device(self, name: str, summary: str = "") -> None:
        """Name the paired phone in the header chip and on the settings page; empty when none is paired."""
        if (name, summary) != (self.device_name, self.device_summary):
            self.device_name, self.device_summary = name, summary
            self.render()

    def set_update(self, version: str | None) -> None:
        """Offer a newer release in the footer of the history and settings pages; None removes it."""
        if version != self.update:
            self.update = version
            self.render()

    def animate_progress(self) -> None:
        if self.progressing and self.fraction is None and self.page in ("onboarding", "history"):
            self.render()

    def draw_progress(self, frame: Image.Image, x: int, y: int, width: int) -> None:
        height = self.px(5)
        frame.alpha_composite(shape(width, height, height / 2, TINT), (x, y))
        if self.fraction is None:
            fill = width // 4
            cycle = time.monotonic() % 3 / 1.5
            x += round((width - fill) * (1 - abs(cycle - 1)))
        else:
            fill = round(width * self.fraction)
        if fill:
            frame.alpha_composite(shape(fill, height, height / 2, ACCENT), (x, y))

    def set_settings(self, values: dict) -> None:
        if values != self.settings:
            self.settings = values
            self.render()

    def set_onboarding(self, phase: str, driver: str, retry: bool = False) -> None:
        """``retry``: the connection waits for the user's "continue" (shown on the history page too)."""
        state = {"phase": phase, "driver": driver, "retry": retry}
        if state != self.onboarding:
            self.onboarding = state
            self.render()

    def copied(self, item_id: int) -> None:
        self.copied_id = item_id
        self.render()

    # --- showing -------------------------------------------------------------------------
    def show(self, right: int, bottom: int, page: str = "history") -> None:
        """Open with the window's bottom-right corner at screen pixel (right, bottom)."""
        self.selected, self.scroll, self.hover, self.copied_id, self.page = 0, 0, None, None, page
        self.user_selected = False
        self.reset_confirm()
        frame = self.frame()
        x, y = right - frame.width, bottom - frame.height
        self.present(frame, x, y)
        self.window.top.focus_force()

    def present(self, frame: Image.Image, x: int, y: int) -> None:
        margin = self.px(SHADOW_MARGIN)
        self.shadow_window.present(self.shadow_image(frame.size), x - margin, y - margin)
        self.window.present(frame, x, y)
        self.shadow_window.place_behind(self.window)

    def render(self) -> None:
        if not self.visible:
            return
        frame = self.frame()
        x, y = self.window.position
        if frame.size != self.window.size:
            # Grow or shrink upwards so the window stays anchored above the taskbar.
            y += self.window.size[1] - frame.height
        self.present(frame, x, y)

    def open_page(self, page: str) -> None:
        self.page, self.hover, self.settings_scroll = page, None, 0
        self.reset_confirm()
        self.render()

    def hide(self) -> None:
        self.window.hide()
        self.shadow_window.hide()
        self.set_hover(None)

    def dismiss_if_unfocused(self) -> None:
        # Pairing needs the user's attention on the phone, so the guide stays until closed.
        if self.visible and self.page != "onboarding" and self.window.top.focus_displayof() is None:
            self.hide()

    def shadow_image(self, size: tuple[int, int]) -> Image.Image:
        return shadow(size[0], size[1], self.px(RADIUS), self.px(SHADOW_MARGIN), self.px(14), self.px(10),
                      (23, 32, 43, 72))

    # --- input ---------------------------------------------------------------------------
    def hit(self, x: int, y: int) -> tuple | None:
        for x0, y0, x1, y1, action in reversed(self.hits):
            if x0 <= x < x1 and y0 <= y < y1:
                return action
        return None

    def motion(self, event) -> None:
        self.set_hover(self.hit(event.x, event.y))

    def set_hover(self, action) -> None:
        if action != self.hover:
            self.hover = action
            self.window.top.configure(cursor="hand2" if action and action[0] != "select" else "")
            self.render()

    def press(self, event) -> None:
        self.pressed = self.hit(event.x, event.y)
        if self.pressed and self.pressed[0] == "select":
            self.select(self.pressed[1])

    def release(self, event) -> None:
        action, self.pressed = self.hit(event.x, event.y), None
        if not action:
            return
        name = action[0]
        if name == "copy":
            self.on_copy(self.entries[action[1]].id)
        elif name == "delete":
            self.on_delete(self.entries[action[1]].id)
        elif name == "settings":
            self.open_page("settings")
        elif name == "back":
            self.open_page(self.back_page)
        elif name == "enable":
            self.on_enable_history(self.tab)
        elif name == "tab":
            self.switch_tab(action[1])
        elif name == "send_item":
            self.on_send_item(self.entries[action[1]].id)
        elif name == "toggle":
            self.on_toggle(action[1])
        elif name == "type":
            self.on_type(action[1], action[2])
        elif name == "offer":
            self.on_offer(action[1])
        elif name == "update":
            self.on_open_update()
        elif name == "close":
            self.hide()
        elif name == "pair":
            self.on_pair()
        elif name == "install":
            self.on_install_driver()
        elif name == "store":
            self.on_open_store()
        elif name == "finish":
            self.on_finish()
        elif name == "clear":
            tab = self.tab
            self.confirm("clear", lambda: self.on_clear(tab))
        elif name == "forget":
            self.confirm("forget", self.on_forget)

    def double_click(self, event) -> None:
        action = self.hit(event.x, event.y)
        if action and action[0] == "select":
            self.on_copy(self.entries[action[1]].id)

    def confirm(self, name: str, action: Callable[[], None]) -> None:
        """Destructive actions take a second click within three seconds instead of a dialog."""
        if self.confirming == name:
            self.reset_confirm()
            action()
            return
        self.reset_confirm()
        self.confirming = name
        self.confirm_job = self.window.top.after(3000, lambda: (self.reset_confirm(), self.render()))
        self.render()

    def reset_confirm(self) -> None:
        if self.confirm_job is not None:
            self.window.top.after_cancel(self.confirm_job)
            self.confirm_job = None
        self.confirming = None

    def select(self, index: int) -> None:
        if not self.entries or self.page != "history":
            return
        self.selected = max(0, min(index, len(self.entries) - 1))
        self.user_selected = True
        rows, _ = self.layout()
        row = rows[self.selected]
        viewport = self.px(LIST_HEIGHT)
        if row.y < self.scroll:
            self.scroll = row.y
        elif row.y + row.height > self.scroll + viewport:
            self.scroll = row.y + row.height - viewport
        self.render()

    def move(self, step: int) -> None:
        self.select(self.selected + step)

    def scroll_by(self, amount: float) -> None:
        if self.page == "settings":
            self.settings_scroll += round(amount)
        elif self.page == "history":
            self.scroll += round(amount)
        else:
            return
        self.render()

    def primary(self) -> None:
        """Enter does the page's main thing."""
        if self.page == "onboarding":
            if self.onboarding["phase"] == "done":
                self.on_finish()
            elif self.onboarding["phase"] in ("idle", "developer", "retry"):
                self.on_pair()
        else:
            self.copy_selected()

    def copy_selected(self) -> None:
        if self.entries and self.page == "history":
            self.on_copy(self.entries[self.selected].id)

    def delete_selected(self) -> None:
        if self.entries and self.page == "history":
            self.on_delete(self.entries[self.selected].id)

    # --- drawing -------------------------------------------------------------------------
    def text_of(self, entry) -> str:
        text = entry.preview.decode("utf-8", errors="replace")
        return text + "…" if len(text) >= 96 else text  # stored previews are capped at 96 characters

    def layout(self) -> tuple[list[Row], int]:
        px = self.px
        card_width = px(WIDTH) - px(PAD) * 2
        title = font(px(13.5), True)
        rows, y = [], px(4) + self.banner_height()
        for index, entry in enumerate(self.entries):
            height, lines = px(ROW), []
            if entry.kind == "text" and index == self.selected:
                lines = wrap(self.text_of(entry), title, card_width - px(60) - self.actions_width(), 3)
                height = max(height, px(12) + len(lines) * px(20) + px(34))
            rows.append(Row(entry, y, height, lines))
            y += height + px(GAP)
        return rows, y

    def actions_width(self) -> int:
        """Room taken by the selected card's buttons: computer items can also be sent to the phone."""
        return self.px(132 if self.tab == "pc" else 92)

    def detail_lines(self) -> list[str]:
        """The status under the title, wrapped to at most two lines."""
        if not self.detail_visible():
            return []
        room = self.px(WIDTH) - self.px(PAD) * 2
        if self.onboarding.get("retry"):
            room -= round(font(self.px(12)).getlength(tr("继续连接"))) + self.px(32)
        return wrap(self.detail, font(self.px(12)), room, 2)

    def detail_visible(self) -> bool:
        return bool(self.detail) and not (self.tone == "ok" and self.detail.startswith(tr("已连接")))

    def frame(self) -> Image.Image:
        if self.page == "onboarding":
            return self.onboarding_frame()
        px = self.px
        width, pad = px(WIDTH), px(PAD)
        settings = self.page == "settings"
        detail_lines = self.detail_lines() if not settings else []
        tabs_top = px(64) + (len(detail_lines) * px(18) + px(6) if detail_lines else 0)
        if not settings and self.progressing:
            tabs_top += px(16)
        list_top = tabs_top + (0 if settings else px(TAB_SPACE))
        viewport = px(LIST_HEIGHT) + (px(TAB_SPACE) if settings else 0)  # same window size on both pages
        height = list_top + viewport + px(12) + px(32) + px(18)
        frame = shape(width, height, px(RADIUS), WHITE).copy()
        draw = ImageDraw.Draw(frame)
        self.hits = []

        # Header: title (or back), connection chip, settings.
        if settings:
            hovered = self.hover == ("back",)
            frame.alpha_composite(shape(px(32), px(32), px(16), CONTROL_HOVER if hovered else CONTROL), (pad, px(18)))
            frame.alpha_composite(icon("back", px(16), SOFT_INK), (pad + px(8), px(26)))
            self.hits.append((pad, px(18), pad + px(32), px(50), ("back",)))
            draw.text((pad + px(44), px(34)), tr("设置"), font=font(px(20), True), fill=INK, anchor="lm")
            chip_right = width - pad
        else:
            draw.text((pad, px(34)), tr("剪贴板"), font=font(px(20), True), fill=INK, anchor="lm")
            settings_x = width - pad - px(32)
            hovered = self.hover == ("settings",)
            frame.alpha_composite(shape(px(32), px(32), px(16), CONTROL_HOVER if hovered else CONTROL),
                                  (settings_x, px(18)))
            frame.alpha_composite(icon("sliders", px(16), SOFT_INK), (settings_x + px(8), px(26)))
            self.hits.append((settings_x, px(18), settings_x + px(32), px(50), ("settings",)))
            chip_right = settings_x - px(8)
        background, text_color, dot, label = CHIPS[self.chip]
        label = tr(label)
        chip_font = font(px(12), True)
        chip_inset = px(12) + px(7) + px(6) + px(12)  # dot, gap and padding around the label
        if self.chip == "ok" and self.device_name and not settings:
            # Say which phone is connected; the name gives way before the title would be covered.
            # The settings page names the phone in its own card, so its chip stays short.
            title_right = pad + round(font(px(20), True).getlength(tr("剪贴板"))) + px(12)
            label = fit(f"{label} · {self.device_name}", chip_font, chip_right - title_right - chip_inset)
        chip_width = chip_inset + round(chip_font.getlength(label))
        chip_x = chip_right - chip_width
        frame.alpha_composite(shape(chip_width, px(28), px(14), background), (chip_x, px(20)))
        draw.ellipse((chip_x + px(12), px(30.5), chip_x + px(19), px(37.5)), fill=dot)
        draw.text((chip_x + px(25), px(34)), label, font=chip_font, fill=text_color, anchor="lm")
        if detail_lines:
            if self.onboarding.get("retry"):
                label_width = round(font(px(12)).getlength(tr("继续连接"))) + px(24)
                self.button(frame, width - pad - label_width, px(58), label_width, px(26), tr("继续连接"), ("pair",))
            for number, line in enumerate(detail_lines):
                draw.text((pad, px(70) + number * px(18)), line, font=font(px(12)), fill=TONES[self.tone],
                          anchor="lm")

        # The two lists: what came from the iPhone, and what was copied on this computer.
        if not settings:
            inner = width - pad * 2
            if self.progressing:
                self.draw_progress(frame, pad, tabs_top - px(15), inner)
            frame.alpha_composite(shape(inner, px(32), px(16), CONTROL), (pad, tabs_top))
            half = inner // 2
            for index, (key, label) in enumerate(TABS):
                label = tr(label)
                x = pad + index * half
                count = len(self.lists[key][0])
                text = f"{label} · {count}" if count else label
                if key == self.tab:
                    frame.alpha_composite(shadow(half - px(6), px(26), px(13), px(6), px(3), px(1), (0, 0, 0, 40)),
                                          (x + px(3) - px(6), tabs_top + px(3) - px(6)))
                    frame.alpha_composite(shape(half - px(6), px(26), px(13), WHITE), (x + px(3), tabs_top + px(3)))
                    draw.text((x + half // 2, tabs_top + px(16)), text, font=font(px(12.5), True), fill=INK,
                              anchor="mm")
                else:
                    color = INK if self.hover == ("tab", key) else MUTED
                    draw.text((x + half // 2, tabs_top + px(16)), text, font=font(px(12.5)), fill=color, anchor="mm")
                    self.hits.append((x, tabs_top, x + half, tabs_top + px(32), ("tab", key)))

        # The list, drawn on its own layer so scrolled rows are clipped to the viewport.
        layer = Image.new("RGBA", (width, viewport), (0, 0, 0, 0))
        rows, content = self.layout()
        self.scroll = max(0, min(self.scroll, content - viewport))
        if settings:
            self.draw_settings(layer, list_top)
        elif rows or self.banner_height():
            self.draw_rows(layer, rows, list_top)
            if content > viewport:
                fade = Image.linear_gradient("L").resize((1, px(18))).resize((width, px(18)))
                white = Image.new("RGBA", (width, px(18)), WHITE)
                white.putalpha(fade)
                layer.alpha_composite(white, (0, viewport - px(18)))
                thumb = max(px(28), viewport * viewport // content)
                thumb_y = round(self.scroll / (content - viewport) * (viewport - thumb))
                layer.alpha_composite(shape(px(4), thumb, px(2), (23, 32, 43, 70)), (width - px(8), thumb_y))
        else:
            self.draw_empty(layer, list_top)
        frame.alpha_composite(layer, (0, list_top))

        # Footer.
        footer_y = list_top + viewport + px(12)
        note_font = font(px(12))
        if settings:
            note = f"{APP_NAME} {self.version}".strip()
            if self.update:
                note += " · " + tr("发现新版本 {version}", version=self.update)
        elif self.update:
            # Takes the place of the history note until the user updates or turns the check off.
            note = tr("发现新版本 {version}，点击查看", version=self.update)
        elif self.history_on:
            note = (tr("{count} 条 · 加密保存在本机", count=len(self.entries)) if self.entries else
                    tr("已开启 · 加密保存在本机"))
        else:
            note = tr("未保留 iPhone 剪贴板历史" if self.tab == "phone" else "未保留电脑剪贴板历史")
        right = width - pad
        if self.entries and not settings:
            label = tr("确认清空" if self.confirming == "clear" else "清空")
            button_width = round(note_font.getlength(label)) + px(28)
            button_x = right - button_width
            hovered = self.hover == ("clear",)
            if self.confirming == "clear":
                fill, color = DANGER_TINT, DANGER
            else:
                fill, color = (CONTROL_HOVER if hovered else CONTROL), (59, 70, 82, 255)
            frame.alpha_composite(shape(button_width, px(32), px(16), fill), (button_x, footer_y))
            draw.text((button_x + button_width // 2, footer_y + px(16)), label, font=note_font, fill=color,
                      anchor="mm")
            self.hits.append((button_x, footer_y, button_x + button_width, footer_y + px(32), ("clear",)))
            right = button_x - px(8)
        note = fit(note, note_font, right - pad - px(4))
        color = MUTED
        if self.update:
            hovered = self.hover == ("update",)
            color = ACCENT_HOVER if hovered else ACCENT
            note_right = pad + px(4) + round(note_font.getlength(note))
            self.hits.append((pad, footer_y, note_right + px(4), footer_y + px(32), ("update",)))
        draw.text((pad + px(4), footer_y + px(16)), note, font=note_font, fill=color, anchor="lm")
        return frame

    def draw_offer(self, layer: Image.Image, list_top: int) -> None:
        """A file from the iPhone waiting for the user's choice, above the list."""
        px = self.px
        pad = px(PAD)
        card_width = layer.width - pad * 2
        top = px(4) - self.scroll
        height = self.banner_height() - px(8)
        if top + height < 0:
            return
        draw = ImageDraw.Draw(layer)
        offer = self.offer
        layer.alpha_composite(shape(card_width, height, px(16), (255, 247, 232, 255), (240, 214, 170, 255), 1),
                              (pad, top))
        layer.alpha_composite(shape(px(36), px(36), px(10), WHITE), (pad + px(12), top + px(12)))
        layer.alpha_composite(icon("file", px(18), (138, 83, 0, 255)), (pad + px(21), top + px(21)))
        title_font, small_font = font(px(13.5), True), font(px(12))
        draw.text((pad + px(60), top + px(21)), fit(offer["name"], title_font, card_width - px(74)),
                  font=title_font, fill=INK, anchor="lm")
        draw.text((pad + px(60), top + px(40)), str(offer["meta"]), font=small_font, fill=MUTED, anchor="lm")
        button_y = top + height - px(12) - px(30)
        right = pad + card_width - px(12)
        accept_label = tr("同步中…" if offer.get("busy") else "同步到电脑")
        accept_width = round(small_font.getlength(accept_label)) + px(28)
        self.button(layer, right - accept_width, button_y, accept_width, px(30), accept_label, ("offer", True),
                    primary=True, enabled=not offer.get("busy"))
        right -= accept_width + px(8)
        if not offer.get("busy"):
            ignore_width = round(small_font.getlength(tr("忽略"))) + px(28)
            self.button(layer, right - ignore_width, button_y, ignore_width, px(30), tr("忽略"), ("offer", False))
            right -= ignore_width + px(8)
        draw.text((pad + px(14), button_y + px(15)), fit(str(offer["reason"]), small_font, right - pad - px(18)),
                  font=small_font, fill=(138, 83, 0, 255), anchor="lm")
        # self.button records hits in layer coordinates; move them into window coordinates.
        self.hits = [(x0, y0 + list_top, x1, y1 + list_top, action) if action[0] == "offer" and y1 <= layer.height
                     else (x0, y0, x1, y1, action) for x0, y0, x1, y1, action in self.hits]

    def draw_rows(self, layer: Image.Image, rows: list[Row], list_top: int) -> None:
        px = self.px
        if self.banner_height():
            self.draw_offer(layer, list_top)
        pad = px(PAD)
        card_width = layer.width - pad * 2
        viewport = layer.height
        draw = ImageDraw.Draw(layer)
        title_font, sub_font = font(px(13.5), True), font(px(12))
        for index, row in enumerate(rows):
            top = row.y - self.scroll
            if top + row.height < 0 or top > viewport:
                continue
            entry, selected = row.entry, index == self.selected
            hovered = self.hover == ("select", index)
            if selected:
                margin = px(16)
                layer.alpha_composite(shadow(card_width, row.height, px(16), margin, px(8), px(5),
                                             (15, 110, 90, 60)), (pad - margin, top - margin))
                layer.alpha_composite(shape(card_width, row.height, px(16), WHITE, ACCENT, max(1.0, px(1.5))),
                                      (pad, top))
            else:
                layer.alpha_composite(shape(card_width, row.height, px(16), CARD_HOVER if hovered else CARD),
                                      (pad, top))
            visible_top, visible_bottom = max(top, 0), min(top + row.height, viewport)
            self.hits.append((pad, list_top + visible_top, pad + card_width, list_top + visible_bottom,
                              ("select", index)))

            # Type tile.
            tile_y = top + px(12)
            text_x = pad + px(60)
            if entry.kind == "file":
                name, _, size = entry.preview.decode("utf-8", errors="replace").partition("\n")
                layer.alpha_composite(shape(px(36), px(36), px(10), (255, 247, 232, 255)), (pad + px(12), tile_y))
                layer.alpha_composite(icon("file", px(18), (138, 83, 0, 255)), (pad + px(21), tile_y + px(9)))
                title = name or tr("文件").capitalize()
            elif entry.kind == "image":
                text_x = pad + px(80)
                thumbnail = self.thumbnail(entry, px(56), px(36))
                if thumbnail is not None:
                    layer.alpha_composite(thumbnail, (pad + px(12), tile_y))
                else:
                    layer.alpha_composite(shape(px(56), px(36), px(10), TINT), (pad + px(12), tile_y))
                    layer.alpha_composite(icon("image", px(18), ACCENT), (pad + px(31), tile_y + px(9)))
                title = tr("图片").capitalize()
            else:
                text = self.text_of(entry)
                is_link = text.startswith(("http://", "https://"))
                layer.alpha_composite(shape(px(36), px(36), px(10), LINK_TINT if is_link else TINT),
                                      (pad + px(12), tile_y))
                layer.alpha_composite(icon("link" if is_link else "text", px(18), LINK_INK if is_link else ACCENT),
                                      (pad + px(21), tile_y + px(9)))
                title = text.split("://", 1)[1] if is_link else text

            # Text.
            right = pad + card_width - px(14) - (self.actions_width() - px(12) if selected else 0)
            if row.lines:
                for number, line in enumerate(row.lines):
                    draw.text((text_x, top + px(12) + number * px(20)), line, font=title_font, fill=INK)
            else:
                draw.text((text_x, top + px(21)), fit(title, title_font, right - text_x), font=title_font,
                          fill=INK, anchor="lm")
            sub = short_time(entry.created)
            if entry.kind == "file" and size.isdigit():
                sub += " · " + human_size(int(size))
            marked = entry.id in (self.copied_id, self.sent_id)
            if entry.id == self.copied_id:
                sub += " · " + tr("已复制")
            elif entry.id == self.sent_id:
                sub += " · " + tr("已发送到 iPhone")
            draw.text((text_x, top + row.height - px(19)), sub, font=sub_font, fill=ACCENT if marked else MUTED,
                      anchor="lm")

            # Actions on the selected card.
            if selected:
                button_y = top + px(14)
                copy_x = pad + card_width - px(12) - px(32)
                buttons = [(copy_x, "copy")]
                if self.tab == "pc":
                    # Computer items can be sent to the phone from the list.
                    send_x = copy_x - px(8) - px(32)
                    hovered = self.hover == ("send_item", index)
                    layer.alpha_composite(shape(px(32), px(32), px(10), (205, 230, 221, 255) if hovered else TINT),
                                          (send_x, button_y))
                    layer.alpha_composite(icon("upload", px(16), ACCENT), (send_x + px(8), button_y + px(8)))
                    buttons.append((send_x, "send_item"))
                    delete_x = send_x - px(8) - px(32)
                else:
                    delete_x = copy_x - px(8) - px(32)
                buttons.append((delete_x, "delete"))
                hovered = self.hover == ("delete", index)
                layer.alpha_composite(shape(px(32), px(32), px(10), DANGER_TINT if hovered else CONTROL),
                                      (delete_x, button_y))
                layer.alpha_composite(icon("trash", px(16), DANGER if hovered else (59, 70, 82, 255)),
                                      (delete_x + px(8), button_y + px(8)))
                fill = ACCENT_HOVER if self.hover == ("copy", index) else ACCENT
                layer.alpha_composite(shape(px(32), px(32), px(10), fill), (copy_x, button_y))
                done = entry.id == self.copied_id
                layer.alpha_composite(icon("check" if done else "copy", px(16), WHITE, fill),
                                      (copy_x + px(8), button_y + px(8)))
                for x, name in buttons:
                    if button_y >= 0 and button_y + px(32) <= viewport:
                        self.hits.append((x, list_top + button_y, x + px(32), list_top + button_y + px(32),
                                          (name, index)))

    def draw_settings(self, layer: Image.Image, list_top: int) -> None:
        """Drawn on a tall canvas, then the visible part is cut out: the page scrolls."""
        px = self.px
        pad = px(PAD)
        width, viewport = layer.width, layer.height
        card_width = width - pad * 2
        content = Image.new("RGBA", (width, px(900)), (0, 0, 0, 0))
        draw = ImageDraw.Draw(content)
        title_font, label_font, small_font = font(px(13.5), True), font(px(13.5)), font(px(12))
        hits: list[tuple[int, int, int, int, tuple]] = []

        y = px(4)
        if self.settings.get("paired", True):
            # The paired iPhone: connection detail and "forget".
            confirming = self.confirming == "forget"
            label = tr("确认忘记" if confirming else "忘记此 iPhone")
            button_width = round(small_font.getlength(label)) + px(28)
            text_width = card_width - px(60) - button_width - px(24)
            lines = wrap(self.connection or self.detail or tr("未连接"), small_font, text_width, 2)
            # Model and system go last: by then the text is below the button and can use the full width.
            summary = fit(self.device_summary, small_font, card_width - px(60) - px(16)) if self.device_summary else ""
            extra = px(18) if summary else 0
            card_height = max(px(68), px(16) + px(20) + px(6) + extra + len(lines) * px(18) + px(14))
            content.alpha_composite(shape(card_width, card_height, px(16), CARD), (pad, y))
            content.alpha_composite(shape(px(36), px(36), px(10), TINT), (pad + px(12), y + px(14)))
            content.alpha_composite(icon("phone", px(18), ACCENT), (pad + px(21), y + px(23)))
            title = fit(self.device_name or "iPhone", title_font, text_width)
            draw.text((pad + px(60), y + px(14)), title, font=title_font, fill=INK)
            for number, line in enumerate(lines):
                tone = {"ok": "ok", "warn": "warn"}.get(self.chip, "muted") if self.connection else self.tone
                draw.text((pad + px(60), y + px(40) + number * px(18)), line, font=small_font, fill=TONES[tone])
            if summary:
                draw.text((pad + px(60), y + px(40) + len(lines) * px(18)), summary, font=small_font, fill=MUTED)
            button_x, button_y = pad + card_width - px(12) - button_width, y + px(16)
            hovered = self.hover == ("forget",)
            content.alpha_composite(shape(button_width, px(32), px(16), DANGER_TINT if hovered or confirming else WHITE),
                                    (button_x, button_y))
            draw.text((button_x + button_width // 2, button_y + px(16)), label, font=small_font, fill=DANGER,
                      anchor="mm")
            hits.append((button_x, button_y, button_x + button_width, button_y + px(32), ("forget",)))
            y += card_height

        row_height = px(44)
        for section, rows in SECTIONS:
            y += px(14)
            if section:
                draw.text((pad + px(6), y + px(9)), tr(section), font=font(px(12), True), fill=MUTED,
                          anchor="lm")
                y += px(24)
            content.alpha_composite(shape(card_width, row_height * len(rows), px(16), CARD), (pad, y))
            hovered_row = None
            for number, (kind, key, text) in enumerate(rows):
                top = y + number * row_height
                if kind == "switch":
                    on, enabled = self.settings.get(key, (False, True))
                    if enabled and self.hover == ("toggle", key):
                        hovered_row = number
                        content.alpha_composite(shape(card_width - px(8), row_height - px(8), px(12), CARD_HOVER),
                                                (pad + px(4), top + px(4)))
                    switch_x, switch_y = pad + card_width - px(16) - px(44), top + (row_height - px(26)) // 2
                    label = fit(tr(text), label_font, switch_x - pad - px(24))
                    draw.text((pad + px(16), top + row_height // 2), label, font=label_font,
                              fill=INK if enabled else MUTED, anchor="lm")
                    track = ACCENT if on else SWITCH_OFF
                    if not enabled:
                        track = track[:3] + (110,)
                    content.alpha_composite(shape(px(44), px(26), px(13), track), (switch_x, switch_y))
                    knob_x = switch_x + (px(21) if on else px(3))
                    content.alpha_composite(shadow(px(20), px(20), px(10), px(4), px(1.5), px(1), (0, 0, 0, 55)),
                                            (knob_x - px(4), switch_y + px(3) - px(4)))
                    content.alpha_composite(shape(px(20), px(20), px(10), WHITE), (knob_x, switch_y + px(3)))
                    if enabled:
                        hits.append((pad, top, pad + card_width, top + row_height, ("toggle", key)))
                else:
                    # Language and size limits are single-choice. At least one content type stays selected.
                    single = key in ("language", "ask_limit")
                    current = self.settings.get(key, "system" if key == "language" else ())
                    chosen = (current,) if single else current
                    chip_right = pad + card_width - px(14)
                    for value, chip_label in reversed(CHOICES[key]):
                        chip_label = tr(chip_label)
                        selected = value in chosen
                        chip_font = font(px(12 if single else 12.5), selected)
                        chip_width = round(chip_font.getlength(chip_label)) + px(16 if single else 30)
                        chip_x, chip_y = chip_right - chip_width, top + (row_height - px(28)) // 2
                        locked = selected and (single or len(chosen) == 1)  # keep one choice
                        hovered = not locked and self.hover == ("type", key, value)
                        if selected:
                            fill = (205, 230, 221, 255) if hovered else TINT
                        else:
                            fill = CONTROL_HOVER if hovered else WHITE
                        content.alpha_composite(shape(chip_width, px(28), px(14), fill), (chip_x, chip_y))
                        draw.text((chip_x + chip_width // 2, chip_y + px(14)), chip_label, font=chip_font,
                                  fill=(12, 90, 73, 255) if selected else MUTED, anchor="mm")
                        if not locked:
                            hits.append((chip_x, chip_y, chip_x + chip_width, chip_y + px(28), ("type", key, value)))
                        chip_right = chip_x - px(4 if single else 6)
                    label = fit(tr(text), label_font, chip_right - pad - px(20))
                    draw.text((pad + px(16), top + row_height // 2), label, font=label_font, fill=INK, anchor="lm")
                if number and hovered_row not in (number, number - 1):
                    draw.line((pad + px(16), top, pad + card_width - px(16), top), fill=(226, 231, 236, 255))
            y += row_height * len(rows)
        total = y + px(8)

        self.settings_scroll = max(0, min(self.settings_scroll, total - viewport))
        offset = self.settings_scroll
        layer.alpha_composite(content.crop((0, offset, width, offset + viewport)))
        for x0, y0, x1, y1, action in hits:
            top, bottom = max(y0 - offset, 0), min(y1 - offset, viewport)
            if bottom > top:
                self.hits.append((x0, list_top + top, x1, list_top + bottom, action))
        if total > viewport:
            fade = Image.linear_gradient("L").resize((1, px(18))).resize((width, px(18)))
            white = Image.new("RGBA", (width, px(18)), WHITE)
            white.putalpha(fade)
            if offset < total - viewport:
                layer.alpha_composite(white, (0, viewport - px(18)))
            if offset > 0:
                layer.alpha_composite(white.transpose(Image.FLIP_TOP_BOTTOM), (0, 0))
            thumb = max(px(28), viewport * viewport // total)
            thumb_y = round(offset / (total - viewport) * (viewport - thumb))
            layer.alpha_composite(shape(px(4), thumb, px(2), (23, 32, 43, 70)), (width - px(8), thumb_y))

    def button(self, frame: Image.Image, x: int, y: int, width: int, height: int, label: str, action: tuple,
               *, primary: bool = False, enabled: bool = True, danger: bool = False) -> None:
        px = self.px
        hovered = enabled and self.hover == action
        if primary:
            fill = (ACCENT_HOVER if hovered else ACCENT) if enabled else ACCENT[:3] + (120,)
            color = WHITE
        elif danger:
            fill, color = (DANGER_TINT if hovered else CONTROL), DANGER
        else:
            fill, color = (CONTROL_HOVER if hovered else CONTROL), SOFT_INK
        frame.alpha_composite(shape(width, height, height / 2, fill), (x, y))
        face = font(px(14 if primary else 12.5), primary)
        ImageDraw.Draw(frame).text((x + width // 2, y + height // 2), fit(label, face, width - px(16)),
                                   font=face, fill=color, anchor="mm")
        if enabled:
            self.hits.append((x, y, x + width, y + height, action))

    def onboarding_frame(self) -> Image.Image:
        """First-run pairing guide; it grows with its content."""
        px = self.px
        width, pad = px(WIDTH), px(PAD)
        inner = width - pad * 2
        phase, driver = self.onboarding["phase"], self.onboarding["driver"]
        done = phase == "done"
        title_font, body_font, small_font = font(px(13.5), True), font(px(13.5)), font(px(12))
        canvas = Image.new("RGBA", (width, px(900)), (0, 0, 0, 0))
        draw = ImageDraw.Draw(canvas)
        self.hits = []

        # Header with a close button: the guide does not hide when focus moves to the phone.
        draw.text((pad, px(34)), tr("配对完成" if done else "连接 iPhone"), font=font(px(20), True), fill=INK,
                  anchor="lm")
        close_x = width - pad - px(32)
        canvas.alpha_composite(shape(px(32), px(32), px(16), CONTROL_HOVER if self.hover == ("close",) else CONTROL),
                               (close_x, px(18)))
        canvas.alpha_composite(icon("close", px(14), SOFT_INK), (close_x + px(9), px(27)))
        self.hits.append((close_x, px(18), close_x + px(32), px(50), ("close",)))
        settings_x = close_x - px(40)
        hovered = self.hover == ("settings",)
        canvas.alpha_composite(shape(px(32), px(32), px(16), CONTROL_HOVER if hovered else CONTROL),
                               (settings_x, px(18)))
        canvas.alpha_composite(icon("sliders", px(16), SOFT_INK), (settings_x + px(8), px(26)))
        self.hits.append((settings_x, px(18), settings_x + px(32), px(50), ("settings",)))
        y = px(66)

        if done:
            canvas.alpha_composite(shape(px(72), px(72), px(36), TINT), (width // 2 - px(36), y + px(8)))
            canvas.alpha_composite(icon("check", px(34), ACCENT), (width // 2 - px(17), y + px(27)))
            y += px(100)
            draw.text((width // 2, y + px(12)), tr("现在可以拔掉数据线"), font=font(px(16), True), fill=INK, anchor="mm")
            y += px(36)
            for line in wrap(tr("之后 iPhone 和电脑在同一 Wi-Fi 下会自动连接，在手机上复制即可粘贴到电脑。"),
                             small_font, inner - px(24), 3):
                draw.text((width // 2, y + px(9)), line, font=small_font, fill=MUTED, anchor="mm")
                y += px(20)
            y += px(8)
            if self.detail:
                chip_text = fit(self.detail, small_font, inner - px(40))
                chip_width = round(small_font.getlength(chip_text)) + px(40)
                background, text_color, dot, _ = CHIPS[self.chip]
                chip_x = width // 2 - chip_width // 2
                canvas.alpha_composite(shape(chip_width, px(28), px(14), background), (chip_x, y))
                draw.ellipse((chip_x + px(13), y + px(10.5), chip_x + px(20), y + px(17.5)), fill=dot)
                draw.text((chip_x + px(27), y + px(14)), chip_text, font=small_font, fill=text_color, anchor="lm")
                y += px(28)
            y += px(24)
            self.button(canvas, pad, y, inner, px(46), tr("开始使用"), ("finish",), primary=True)
            y += px(46)
        else:
            # Steps, with the current one expanded.
            current = PHASE_STEP.get(phase, 0)
            rows = []
            for index, (title, description) in enumerate(STEPS):
                if index == current and phase == "developer":
                    title, description = "开启开发者模式并重启手机", DEVELOPER_HELP
                lines = wrap(tr(description), small_font, inner - px(70), 5) if index == current else []
                rows.append((index, tr(title), lines, px(46) + len(lines) * px(18) + (px(6) if lines else 0)))
            card_height = sum(row[3] for row in rows) + px(12)
            canvas.alpha_composite(shape(inner, card_height, px(16), CARD), (pad, y))
            row_y = y + px(6)
            for index, title, lines, height in rows:
                if index == current:
                    canvas.alpha_composite(shape(inner - px(12), height, px(12), WHITE), (pad + px(6), row_y))
                cx, cy = pad + px(30), row_y + px(23)
                if index < current:
                    draw.ellipse((cx - px(12), cy - px(12), cx + px(12), cy + px(12)), fill=ACCENT)
                    canvas.alpha_composite(icon("check", px(14), WHITE), (cx - px(7), cy - px(7)))
                elif index == current:
                    draw.ellipse((cx - px(17), cy - px(17), cx + px(17), cy + px(17)), fill=TINT)
                    draw.ellipse((cx - px(12), cy - px(12), cx + px(12), cy + px(12)), fill=WHITE, outline=ACCENT,
                                 width=max(2, px(2)))
                    draw.text((cx, cy), str(index + 1), font=font(px(12), True), fill=ACCENT, anchor="mm")
                else:
                    draw.ellipse((cx - px(12), cy - px(12), cx + px(12), cy + px(12)), outline=(196, 204, 212, 255),
                                 width=max(1, px(1.5)))
                    draw.text((cx, cy), str(index + 1), font=font(px(12)), fill=MUTED, anchor="mm")
                text_x = pad + px(54)
                draw.text((text_x, cy), fit(title, title_font if index == current else body_font, inner - px(66)),
                          font=title_font if index == current else body_font,
                          fill=INK if index >= current else MUTED, anchor="lm")
                for number, line in enumerate(lines):
                    draw.text((text_x, row_y + px(40) + number * px(18)), line, font=small_font, fill=MUTED)
                row_y += height
            y += card_height + px(12)

            # What is happening right now, unless the expanded step already says it.
            if self.detail and self.detail != tr(IDLE_PROMPT) and phase != "developer":
                for line in wrap(self.detail, small_font, inner - px(8), 3):
                    draw.text((pad + px(4), y + px(9)), line, font=small_font, fill=TONES[self.tone], anchor="lm")
                    y += px(20)
                y += px(8)

            if self.progressing:
                self.draw_progress(canvas, pad, y, inner)
                y += px(17)

            # Apple's device service, when it is missing.
            if driver != "ok":
                note = "需要先安装 Apple 设备连接组件。将通过 Windows 包管理器安装 Apple Mobile Device Support，并代表你接受其许可协议。"
                lines = wrap(tr(note), small_font, inner - px(28), 4)
                box_height = px(14) + len(lines) * px(18) + px(10) + px(34) + px(14)
                canvas.alpha_composite(shape(inner, box_height, px(16), (251, 240, 220, 255)), (pad, y))
                for number, line in enumerate(lines):
                    draw.text((pad + px(14), y + px(14) + number * px(18)), line, font=small_font,
                              fill=(110, 66, 0, 255))
                button_y = y + px(14) + len(lines) * px(18) + px(10)
                label = tr("正在安装…" if driver == "installing" else "安装连接组件")
                install_width = round(small_font.getlength(label)) + px(32)
                self.button(canvas, pad + px(14), button_y, install_width, px(34), label, ("install",),
                            enabled=driver != "installing")
                if driver == "failed":
                    store_label = tr("在 Microsoft Store 中查找")
                    self.button(canvas, pad + px(22) + install_width, button_y,
                                round(small_font.getlength(store_label)) + px(32), px(34), store_label, ("store",))
                y += box_height + px(12)

            busy = phase in ("pairing", "preparing")
            label = tr("正在连接…" if busy else CONTINUE_LABELS.get(phase, "连接 iPhone"))
            self.button(canvas, pad, y, inner, px(46), label, ("pair",), primary=True, enabled=not busy)
            y += px(46)

        y += px(14)
        draw.text((width // 2, y + px(10)), f"{APP_NAME} {self.version}".strip(), font=small_font,
                  fill=(150, 160, 170, 255), anchor="mm")
        height = y + px(20) + px(18)
        frame = shape(width, height, px(RADIUS), WHITE).copy()
        frame.alpha_composite(canvas.crop((0, 0, width, height)))
        return frame

    def draw_empty(self, layer: Image.Image, list_top: int) -> None:
        px = self.px
        draw = ImageDraw.Draw(layer)
        center = layer.width // 2
        y = px(48)
        layer.alpha_composite(shape(px(56), px(56), px(28), TINT), (center - px(28), y))
        layer.alpha_composite(icon("clipboard", px(26), ACCENT, TINT), (center - px(13), y + px(15)))
        when_on, when_off, enable_label = EMPTY_TEXT[self.tab]
        enable_label = tr(enable_label)
        lines = when_on if self.history_on else when_off
        draw.text((center, y + px(84)), tr(lines[0]), font=font(px(14), True), fill=INK, anchor="mm")
        sub_font = font(px(12))
        for number, line in enumerate(wrap(tr(lines[1]), sub_font, layer.width - px(PAD) * 4, 2)):
            draw.text((center, y + px(110) + number * px(20)), line, font=sub_font, fill=MUTED, anchor="mm")
        if not self.history_on:
            button_y = y + px(160)
            label_font = font(px(13), True)
            button_width = round(label_font.getlength(enable_label)) + px(40)
            hovered = self.hover == ("enable",)
            layer.alpha_composite(shape(button_width, px(38), px(19), ACCENT_HOVER if hovered else ACCENT),
                                  (center - button_width // 2, button_y))
            draw.text((center, button_y + px(19)), enable_label, font=label_font, fill=WHITE, anchor="mm")
            self.hits.append((center - button_width // 2, list_top + button_y, center + button_width // 2,
                              list_top + button_y + px(38), ("enable",)))

    def thumbnail(self, entry, width: int, height: int) -> Image.Image | None:
        if entry.id not in self.thumbnails:
            try:
                with Image.open(io.BytesIO(entry.preview)) as picture:
                    fitted = ImageOps.fit(picture.convert("RGBA"), (width, height), Image.LANCZOS)
                mask = shape(width, height, self.px(10), WHITE).getchannel("A")
                fitted.putalpha(Image.composite(fitted.getchannel("A"), Image.new("L", mask.size, 0), mask))
                self.thumbnails[entry.id] = fitted
            except Exception:
                self.thumbnails[entry.id] = None
        return self.thumbnails[entry.id]
