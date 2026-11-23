"""Tray history and first-run pairing."""
from __future__ import annotations

import asyncio
import contextlib
import ctypes
from datetime import datetime
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import threading
import time
import tkinter as tk
import traceback
import winreg

from PIL import Image, ImageOps
import pystray
import win32api
import win32con

from iphone_clip import APP_NAME, __version__
from iphone_clip.clipboard import ClipboardWriter, Clip, ClipTooLarge, fingerprint
from iphone_clip import files
from iphone_clip.developer_setup import DownloadProgress, PROGRESS
from iphone_clip.device import (Bridge, CONNECTED_WIFI, FileTooLarge, IDLE_PROMPT, OfferExpired,
                                configure_pairing_storage,
                                forget_paired_devices, paired_identifiers)
from iphone_clip.history import History
from iphone_clip.i18n import Message, message, tr, use_language
from iphone_clip.popup import HistoryPopup
from iphone_clip.settings import (data_dir, history_enabled, language_preference, notifications_enabled,
                                  pc_history_enabled, set_language_preference,
                                  pull_enabled, push_enabled, set_history_enabled, set_notifications_enabled,
                                  set_pc_history_enabled, set_pull_enabled, set_push_enabled, set_sync_types,
                                  sync_types, file_ask_mb, set_file_ask_mb)

RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_VALUE = "iPhoneClipboardPrototype"
# Status tones, mapped onto the panel's colours.
MUTED, ACCENT, WARN = "muted", "ok", "warn"
ICON_GRACE = 30  # seconds before a dropped connection turns the tray icon grey
DISCONNECT_NOTICE = 5 * 60
MAX_TRAY_LOG_BYTES = 512 * 1024
STORE_SEARCH = "ms-windows-store://search/?query=Apple%20Devices"
NOTIFY_ICONS = r"Control Panel\NotifyIconSettings"  # Windows 11 per-app tray icon settings


def log_tray_error(operation: str, error: Exception) -> None:
    """Keep a short local traceback without exception text, locals or clipboard data."""
    try:
        path = data_dir() / "tray-errors.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists() and path.stat().st_size >= MAX_TRAY_LOG_BYTES:
            path.replace(path.with_suffix(".log.1"))
        os_code = getattr(error, "winerror", None)
        if os_code is None and isinstance(error, OSError):
            os_code = error.errno
        code = f" (OS error {os_code})" if os_code is not None else ""
        timestamp = datetime.now().isoformat(timespec="seconds")
        # This file does not use the Python logger because the bridge disables library logging.
        with path.open("a", encoding="utf-8") as log:
            log.write(f"{timestamp} {operation}: {type(error).__name__}{code}\n")
            for frame, line in traceback.walk_tb(error.__traceback__):
                log.write(f"  {Path(frame.f_code.co_filename).name}:{line} in {frame.f_code.co_name}\n")
    except OSError:
        pass  # A read-only or full data directory must not interrupt the tray.


def startup_command() -> str | None:
    return f'"{sys.executable}" --minimized' if getattr(sys, "frozen", False) else None


def startup_enabled() -> bool:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as key:
            command, _ = winreg.QueryValueEx(key, RUN_VALUE)
            return command == startup_command()
    except OSError:
        return False


def apple_driver_installed() -> bool:
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Services\Apple Mobile Device Service"):
            return True
    except OSError:
        pass
    # Store versions of Apple Devices and iTunes run the same service from an app package.
    try:
        with socket.create_connection(("127.0.0.1", 27015), timeout=0.3):
            return True
    except OSError:
        return False


def enable_dpi_awareness() -> None:
    # System-aware rather than per-monitor: Tk does not relayout when a window changes monitors.
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(1)
    except (AttributeError, OSError):
        with contextlib.suppress(AttributeError, OSError):
            ctypes.windll.user32.SetProcessDPIAware()


def expand_known_folder(path: str) -> str:
    """Explorer stores some paths as "{known-folder-GUID}\\rest"; turn them back into real paths."""
    if not path.startswith("{") or "}" not in path:
        return path
    guid, rest = path[1:].split("}", 1)

    class GUID(ctypes.Structure):
        _fields_ = [("data1", ctypes.c_ulong), ("data2", ctypes.c_ushort), ("data3", ctypes.c_ushort),
                    ("data4", ctypes.c_ubyte * 8)]

    folder_id = GUID()
    if ctypes.oledll.ole32.CLSIDFromString("{" + guid + "}", ctypes.byref(folder_id)) != 0:
        return path
    buffer = ctypes.c_wchar_p()
    try:
        ctypes.oledll.shell32.SHGetKnownFolderPath(ctypes.byref(folder_id), 0, None, ctypes.byref(buffer))
    except OSError:
        return path
    try:
        return buffer.value + rest
    finally:
        ctypes.windll.ole32.CoTaskMemFree(buffer)


def promote_tray_icon(executable: str) -> bool | None:
    """Keep this app's tray icon out of the Windows 11 overflow menu, unless the user already chose.

    Returns True when the icon was just promoted, False when there is nothing to do, and None while
    Explorer has not registered the icon yet.
    """
    try:
        settings = winreg.OpenKey(winreg.HKEY_CURRENT_USER, NOTIFY_ICONS)
    except OSError:
        return False  # Windows 10 keeps no per-icon setting here.
    target = os.path.normcase(os.path.abspath(executable))
    with settings:
        index = 0
        while True:
            try:
                name = winreg.EnumKey(settings, index)
            except OSError:
                return None
            index += 1
            try:
                with winreg.OpenKey(settings, name, 0, winreg.KEY_READ | winreg.KEY_SET_VALUE) as key:
                    path, _ = winreg.QueryValueEx(key, "ExecutablePath")
                    if os.path.normcase(os.path.abspath(expand_known_folder(path))) != target:
                        continue
                    try:
                        winreg.QueryValueEx(key, "IsPromoted")
                        return False  # Explorer writes this only when the user moves the icon.
                    except FileNotFoundError:
                        winreg.SetValueEx(key, "IsPromoted", 0, winreg.REG_DWORD, 1)
                        return True
            except OSError:
                continue


def tray_icon(connected: bool) -> Image.Image:
    icon_path = Path(__file__).with_name("assets") / "jade.ico"
    with Image.open(icon_path) as icon:
        image = icon.ico.getimage((64, 64)).convert("RGBA")
    if connected:
        return image
    muted = ImageOps.grayscale(image).convert("RGBA")
    muted.putalpha(image.getchannel("A"))
    return muted


class Window:
    def __init__(self, minimized: bool, wake) -> None:
        self.language = language_preference()
        use_language(self.language)
        self.root = tk.Tk()
        self.root.withdraw()
        self.root.bind_all("<Escape>", lambda _: self.hide())
        self.scale = self.root.winfo_fpixels("1i") / 96
        self.events = queue.SimpleQueue()
        self.status = message("正在连接 iPhone…")
        self.startup_var = tk.BooleanVar(value=startup_enabled())
        self.save_history = tk.BooleanVar(value=history_enabled())
        self.notify_var = tk.BooleanVar(value=notifications_enabled())
        self.push_var = tk.BooleanVar(value=push_enabled())
        self.pull_var = tk.BooleanVar(value=pull_enabled())
        # Kinds of content each direction syncs automatically; read from the bridge thread too.
        self.types = {"pull": sync_types("pull"), "push": sync_types("push")}
        self.ask_mb = file_ask_mb()
        self.offer = None  # a large file from the iPhone waiting for the user's choice
        try:
            files.cleanup()
        except OSError:
            pass
        self.pull_switch = threading.Event()  # read from the bridge thread
        if self.pull_var.get():
            self.pull_switch.set()
        self.pc_history_var = tk.BooleanVar(value=pc_history_enabled())
        self.save_switch = threading.Event()
        if self.save_history.get():
            self.save_switch.set()
        self.history = History(data_dir() / "history.sqlite3")
        self.paired = False
        self.popup = None
        self.tray = None
        self.closed = False
        self.connected = False
        self.icon_connected = False
        self.disconnected_at = None  # monotonic time of the last drop
        self.last_connected = None  # "HH:MM", shown while reconnecting
        self.disconnect_notified = False
        self.last_alert = None
        self.pending_clip = None  # copied on the iPhone while disconnected, not written
        self.driver_failed = False
        self.driver_ready = True
        self.installing = False
        self.status_color = MUTED
        self.bridge_state = ""
        self.connection_message = ""  # the bridge's latest word on the connection, for the settings page
        self.just_paired = False  # paired in this session; the guide shows "unplug the cable" until dismissed
        self.awaiting_wireless = False  # announce the first wireless connection after USB pairing
        self.wake = wake
        self.writer = ClipboardWriter(int(self.root.winfo_id()))
        self.watch_sequence = self.writer._current_sequence()  # copies made before start are not sent
        self.watch_ticks = 0
        self.last_sent = None
        self.bridge = Bridge(self.post_status, self.receive_clip, lambda offer: self.events.put(("offer", offer)))
        configure_pairing_storage()
        self.paired = bool(paired_identifiers())
        if not self.paired:
            self.bridge.hold_failures.set()
        self.popup = HistoryPopup(self.root, self.scale, on_copy=self.copy_item, on_delete=self.delete_item,
                                  on_clear=self.clear_items, on_enable_history=self.enable_history,
                                  on_toggle=self.toggle_setting, on_forget=self.forget_from_popup,
                                  on_type=self.toggle_type, on_offer=self.answer_offer,
                                  on_pair=self.start_pairing, on_install_driver=self.install_driver,
                                  on_open_store=self.open_store, on_finish=self.finish_onboarding,
                                  on_send_item=self.send_item,
                                  version=__version__)
        self.update_popup_status()
        self.tray = pystray.Icon("iPhoneClipboardPrototype", tray_icon(False), APP_NAME, self.tray_menu())
        self.tray.run_detached()
        if getattr(sys, "frozen", False):
            self.root.after(1500, lambda: self.promote_tray(10))
        threading.Thread(target=self.run_bridge, name="iphone-clipboard", daemon=True).start()
        if not self.paired or not minimized:
            self.show()
        self.root.after(100, self.drain)

    def tray_menu(self) -> pystray.Menu:
        return pystray.Menu(
            pystray.MenuItem(tr("剪贴板历史"), lambda: self.events.put(("show", "")), default=True),
            pystray.MenuItem(tr("使用 iPhone 上最新复制的内容"), lambda: self.events.put(("use_pending", "")),
                             visible=lambda _: self.pending_clip is not None),
            pystray.MenuItem(tr("连接与设置"), lambda: self.events.put(("open_setup", ""))),
            pystray.MenuItem(tr("退出"), lambda: self.events.put(("quit", ""))),
        )

    def px(self, value: float) -> int:
        return round(value * self.scale)

    def finish_onboarding(self):
        self.just_paired = False
        self.update_onboarding()
        self.popup.open_page("history")

    def update_onboarding(self):
        # Follow the bridge's state only: it no longer retries on its own while setting up,
        # so each phase holds until the user acts.
        if self.paired and self.just_paired:
            phase = "done"
        elif self.paired:
            phase = "idle"
        else:
            phase = {"pairing": "pairing", "connecting": "preparing", "developer": "developer",
                     "hold": "retry"}.get(self.bridge_state, "idle")
        if self.installing:
            driver = "installing"
        elif self.driver_ready:
            driver = "ok"
        else:
            driver = "failed" if self.driver_failed else "missing"
        self.popup.back_page = "onboarding" if not self.paired or self.just_paired else "history"
        self.popup.set_onboarding(phase, driver, retry=self.bridge_state in ("developer", "hold"))
        self.popup.set_settings(self.settings_state())

    def promote_tray(self, attempts):
        try:
            promoted = promote_tray_icon(sys.executable)
        except OSError:
            return
        if promoted:
            # Re-adding the icon makes Explorer apply the setting straight away.
            self.tray.visible = False
            self.tray.visible = True
        elif promoted is None and attempts > 1:
            self.root.after(1500, lambda: self.promote_tray(attempts - 1))

    def start_pairing(self):
        # Continuing after a pause keeps the guide on its step; only a fresh start asks for trust.
        self.bridge_state = "connecting" if self.bridge_state in ("developer", "hold") else "pairing"
        self.update_onboarding()
        self.bridge.request_pair()

    def forget_from_popup(self):
        # The popup already asked for a second click.
        if self.forget():
            self.update_onboarding()
            self.popup.open_page("onboarding")

    def forget(self) -> bool:
        try:
            forget_paired_devices()
        except OSError:
            self.set_status(message("无法删除配对记录"))
            return False
        self.bridge.request_reset()
        self.bridge.hold_failures.set()
        self.paired = False
        self.just_paired = False
        self.awaiting_wireless = False
        self.connected = False
        self.disconnected_at = None
        self.last_connected = None
        self.pending_clip = None
        self.tray.update_menu()
        self.set_icon(False)
        self.bridge_state, self.connection_message = "setup", IDLE_PROMPT
        self.set_status(message("已忘记 iPhone。请用数据线连接后重新配对"))
        return True

    def install_driver(self):
        if self.installing:
            return
        self.installing = True
        self.set_status(message("正在安装 Apple 连接组件，请完成 Windows 提示。"))
        self.update_onboarding()

        def install():
            try:
                result = subprocess.run(
                    ["winget.exe", "install", "--id", "Apple.AppleMobileDeviceSupport", "--exact",
                     "--source", "winget", "--silent", "--accept-package-agreements",
                     "--accept-source-agreements", "--disable-interactivity"],
                    creationflags=subprocess.CREATE_NO_WINDOW, capture_output=True, timeout=300,
                )
                ready = result.returncode == 0 or apple_driver_installed()
                status = message("连接组件已就绪，可以连接 iPhone。" if ready else
                                 "安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。")
            except (OSError, subprocess.TimeoutExpired):
                ready = False
                status = message("无法运行 Windows 包管理器。可以在 Microsoft Store 安装「Apple Devices」后重试。")
            self.driver_failed = not ready
            self.events.put(("driver", status))

        threading.Thread(target=install, name="apple-driver-setup", daemon=True).start()

    def open_store(self):
        try:
            os.startfile(STORE_SEARCH)
        except OSError:
            self.set_status(message("无法打开 Microsoft Store"))

    def refresh(self):
        if self.popup.visible:
            self.load_entries()

    def load_entries(self):
        try:
            phone, pc = self.history.entries("phone"), self.history.entries("pc")
        except Exception:
            phone, pc = [], []
            self.set_status(message("无法读取历史"))
        self.popup.set_entries({"phone": (phone, self.save_history.get()), "pc": (pc, self.pc_history_var.get())})

    def copy_item(self, item_id):
        try:
            clip = self.history.get(item_id)
            if clip.file_data is not None:
                self.writer.put_files([files.save_received(clip.file_name, clip.file_data)])
            else:
                self.writer.put(clip)
        except Exception:
            self.set_status(message("无法复制这条历史"))
            return
        self.popup.copied(item_id)
        self.root.after(450, self.hide)

    def delete_item(self, item_id):
        try:
            self.history.remove(item_id)
        except Exception:
            self.set_status(message("无法删除这条历史"))
            return
        self.refresh()

    def clear_items(self, source):
        try:
            self.history.remove(source=source)
        except Exception:
            self.set_status(message("无法清空历史"))
            return
        self.refresh()

    def enable_history(self, source="phone"):
        if source == "pc":
            self.pc_history_var.set(True)
            self.change_pc_history()
        else:
            self.save_history.set(True)
            self.change_history()

    def settings_state(self):
        return {"language": self.language, "paired": self.paired,
                "startup": (self.startup_var.get(), bool(startup_command())),
                "history": (self.save_history.get(), True),
                "pc_history": (self.pc_history_var.get(), True),
                "pull": (self.pull_var.get(), True),
                "pull_types": self.types["pull"], "push_types": self.types["push"], "ask_limit": self.ask_mb,
                "notify": (self.notify_var.get(), True),
                "push": (self.push_var.get(), True)}

    def toggle_setting(self, name):
        variable, apply = {"startup": (self.startup_var, self.change_startup),
                           "history": (self.save_history, self.change_history),
                           "pc_history": (self.pc_history_var, self.change_pc_history),
                           "pull": (self.pull_var, self.change_pull),
                           "notify": (self.notify_var, self.change_notifications),
                           "push": (self.push_var, self.change_push)}[name]
        variable.set(not variable.get())
        apply()  # Reverts the variable when the change cannot be saved.
        self.popup.set_settings(self.settings_state())

    def toggle_type(self, key, kind):
        if key == "language":
            self.change_language(kind)
            return
        if key == "ask_limit":
            try:
                set_file_ask_mb(kind)
            except OSError:
                self.set_status(message("无法保存同步设置"))
                return
            self.ask_mb = kind
            self.popup.set_settings(self.settings_state())
            return
        direction = key.removesuffix("_types")
        chosen = set(self.types[direction]) ^ {kind}
        if not chosen:
            self.status_color = MUTED
            self.set_status(message("至少保留一种同步内容"))
            return
        try:
            set_sync_types(direction, chosen)
        except OSError:
            self.set_status(message("无法保存同步设置"))
            return
        self.types[direction] = sync_types(direction)
        self.popup.set_settings(self.settings_state())

    def change_language(self, preference: str) -> None:
        try:
            set_language_preference(preference)
        except OSError:
            self.set_status(message("无法保存语言设置"))
            return
        self.language = preference
        use_language(preference)
        self.popup.set_settings(self.settings_state())
        self.update_popup_status()
        self.tray.menu = self.tray_menu()
        self.update_tray_title()

    def set_status(self, status: Message) -> None:
        self.status = status
        self.update_popup_status()
        self.update_tray_title()

    def update_tray_title(self) -> None:
        status = self.status
        if isinstance(self.connection_message, DownloadProgress) and self.bridge_state == "connecting":
            status = message(PROGRESS["download"])
        title = f"{APP_NAME} · {status}"[:127]
        if self.tray.title != title:
            self.tray.title = title

    def update_popup_status(self):
        if self.connected:
            chip = "ok"
        elif self.bridge_state in ("waiting", "error", "developer", "hold"):
            chip = "warn"
        else:
            chip = "busy"
        connection = self.connection_message
        fraction = connection.fraction if isinstance(connection, DownloadProgress) else None
        self.popup.set_status(str(self.status), chip, self.status_color, str(connection),
                              progressing=self.bridge_state in ("pairing", "connecting"), fraction=fraction)

    def change_history(self):
        enabled = self.save_history.get()
        try:
            set_history_enabled(enabled)
        except OSError:
            self.save_history.set(self.save_switch.is_set())
            self.set_status(message("无法保存历史设置"))
        else:
            self.save_switch.set() if enabled else self.save_switch.clear()
        # The empty list's button and the settings switch are the same setting; keep both in step.
        self.popup.set_settings(self.settings_state())
        self.refresh()

    def change_pc_history(self):
        try:
            set_pc_history_enabled(self.pc_history_var.get())
        except OSError:
            self.pc_history_var.set(not self.pc_history_var.get())
            self.set_status(message("无法保存历史设置"))
        else:
            # Record from the next copy on; what is on the clipboard now is not taken in.
            self.watch_sequence = self.writer._current_sequence()
        self.popup.set_settings(self.settings_state())
        self.refresh()

    def change_pull(self):
        try:
            set_pull_enabled(self.pull_var.get())
        except OSError:
            self.pull_var.set(not self.pull_var.get())
            self.set_status(message("无法保存同步设置"))
            return
        self.pull_switch.set() if self.pull_var.get() else self.pull_switch.clear()

    def change_notifications(self):
        try:
            set_notifications_enabled(self.notify_var.get())
        except OSError:
            self.notify_var.set(not self.notify_var.get())
            self.set_status(message("无法保存通知设置"))

    def change_push(self):
        try:
            set_push_enabled(self.push_var.get())
        except OSError:
            self.push_var.set(not self.push_var.get())
            self.set_status(message("无法保存同步设置"))
            return
        # Start from the clipboard as it is now; turning the switch on sends nothing by itself.
        self.watch_sequence = self.writer._current_sequence()

    def check_pc_clipboard(self):
        """A new copy on this computer: record it and, with automatic sync, send it to the iPhone."""
        sequence = self.writer._current_sequence()
        if sequence is None or sequence == self.watch_sequence:
            return
        self.watch_sequence = sequence
        if sequence == self.writer.sequence:
            return  # this app just wrote it (from the phone or from the history)
        record = self.pc_history_var.get()
        push = self.push_var.get() and self.connected
        if not (record or push):
            return
        try:
            clip, sensitive = self.writer.read()
        except Exception:
            return
        if sensitive or clip is None:
            return
        if ("image" if clip.image else "text") not in self.types["push"]:
            return  # this kind is not synced from the computer; manual sending still works
        if record:
            try:
                self.history.add(clip, "pc")
            except Exception:
                pass
            self.refresh()
        if push:
            self.push(clip, manual=False)

    def send_item(self, item_id):
        """Send one item of the computer history to the iPhone."""
        if not self.connected:
            self.report_send(message("iPhone 未连接，无法发送"), True)
            return
        try:
            clip = self.history.get(item_id)
        except Exception:
            self.report_send(message("无法读取这条历史"), True)
            return
        self.push(clip, manual=True, item_id=item_id)

    def report_send(self, message, manual):
        if manual:
            self.status_color = WARN
            self.set_status(message)
            if not self.popup.visible:
                self.notify(message)

    def push(self, clip, *, manual, item_id=None):
        mark = fingerprint(clip)
        if not manual and mark == self.last_sent:
            return
        self.last_sent = mark
        kind = message("图片" if clip.image else "文本")
        try:
            future = self.bridge.push(clip)
        except ConnectionError:
            self.report_send(message("iPhone 未连接，无法发送"), manual)
            return
        state = "pushed_manual" if manual else "pushed"

        def done(result):
            if result.cancelled() or result.exception() is not None:
                self.events.put(("push_error", message("发送到 iPhone 失败")))
                return
            if item_id is not None:
                self.events.put(("sent_item", str(item_id)))
            self.events.put((state, message("已发送{kind}到 iPhone", kind=kind)))

        future.add_done_callback(done)

    def change_startup(self):
        try:
            with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
                if self.startup_var.get() and startup_command():
                    winreg.SetValueEx(key, RUN_VALUE, 0, winreg.REG_SZ, startup_command())
                else:
                    try:
                        winreg.DeleteValue(key, RUN_VALUE)
                    except FileNotFoundError:
                        pass
        except OSError:
            self.startup_var.set(startup_enabled())
            self.set_status(message("无法更改开机启动"))

    def run_bridge(self):
        try:
            asyncio.run(self.bridge.run())
        except Exception:
            self.post_status("error", message("同步进程已停止，请重新打开应用"))

    def post_status(self, state, message):
        self.events.put((state, message))

    def handle_offer(self, offer):
        """A file was copied on the iPhone: sync it, ask first, or leave it, as the settings say."""
        self.clear_offer()
        name = files.short_name(offer.name)
        size = files.human_size(offer.size) if offer.size else message("大小未知")
        extension = offer.name.rpartition(".")[2].upper()
        meta = message("{extension} · {size}", extension=extension, size=size)
        if "file" not in self.types["pull"]:
            return  # turned off in settings: nothing to say
        if offer.size > files.MAX_FILE_BYTES:
            self.status_color = WARN
            self.set_status(message("「{name}」{size}，超过 512 MB 上限，未同步", name=name, size=size))
            return
        limit = self.ask_mb * 1024 * 1024
        if self.pull_switch.is_set() and (not limit or (offer.size and offer.size <= limit)):
            self.fetch_offer(offer)
            return
        if self.pull_switch.is_set():
            reason = (message("超过 {limit} MB，要同步到电脑吗？", limit=self.ask_mb) if offer.size else
                      message("大小未知，要同步到电脑吗？"))
        else:
            reason = message("已关闭自动同步，需要时点「同步到电脑」")
        self.offer = offer
        self.popup.set_offer({"name": offer.name, "meta": meta, "reason": reason, "busy": False})
        self.status_color = MUTED
        self.set_status(message("收到文件「{name}」{size}，等你选择是否同步", name=name, size=size))
        if self.pull_switch.is_set() and not self.popup.visible:
            self.notify(message("iPhone 上复制了「{name}」（{size}）。打开剪贴板面板选择是否同步", name=name, size=size))

    def answer_offer(self, accept):
        offer = self.offer
        if offer is None:
            return
        if accept:
            self.fetch_offer(offer)
        else:
            self.clear_offer()
            self.status_color = MUTED
            self.set_status(message("已忽略「{name}」", name=files.short_name(offer.name)))

    def clear_offer(self):
        self.offer = None
        self.popup.set_offer(None)

    def fetch_offer(self, offer):
        if self.popup.offer is not None:
            self.popup.set_offer({**self.popup.offer, "busy": True})
        name = files.short_name(offer.name)
        self.status_color = MUTED
        self.set_status(message("正在同步「{name}」{size}…", name=name, size=files.human_size(offer.size))
                        if offer.size else message("正在同步「{name}」…", name=name))
        try:
            future = self.bridge.fetch(offer)
        except ConnectionError:
            self.events.put(("file_error", (offer, message("iPhone 未连接，无法同步文件"))))
            return

        def done(result):
            error = None if result.cancelled() else result.exception()
            if result.cancelled() or error is not None:
                if isinstance(error, OfferExpired):
                    status = message("手机上已复制了别的内容，这个文件不再可用")
                elif isinstance(error, FileTooLarge):
                    status = message("「{name}」太大，未能同步", name=name)
                else:
                    status = message("同步「{name}」失败", name=name)
                self.events.put(("file_error", (offer, status)))
            else:
                self.events.put(("file_ready", (offer, result.result())))

        future.add_done_callback(done)

    def file_ready(self, offer, clip):
        try:
            path = files.save_received(clip.file_name or offer.name, clip.file_data)
            self.writer.put_files([path])
        except Exception:
            self.file_failed(offer, message("无法把「{name}」放进电脑剪贴板", name=files.short_name(offer.name)))
            return
        note = self.save_history_item(clip) if "file" in self.types["pull"] else ""
        self.clear_offer()
        status = message("已同步「{name}」· 可直接粘贴{note}", name=files.short_name(offer.name), note=note)
        self.status_color = ACCENT
        self.set_status(status)
        self.refresh()
        if not self.popup.visible:
            self.notify(status)

    def file_failed(self, offer, message):
        if self.offer is offer:
            self.clear_offer()
        self.status_color = WARN
        self.set_status(message)
        self.notify(message, alert=True)

    def receive_clip(self, clip: Clip, stale: bool = False):
        kind = message("图片" if clip.image else "文本")
        if ("image" if clip.image else "text") not in self.types["pull"]:
            return  # the user does not sync this kind from the iPhone: no clipboard, no history, no notice
        if not self.pull_switch.is_set():
            # Automatic sync is off: keep it at hand (history, tray menu) without touching the PC clipboard.
            self.pending_clip = clip
            self.events.put(("held", message("iPhone 上复制了新的{kind} · 未自动写入电脑，可在面板或托盘菜单中使用{note}",
                                            kind=kind, note=self.save_history_item(clip))))
            return
        if stale and self.writer.changed_elsewhere():
            # Something was copied on this PC after the iPhone item; do not replace it with the older one.
            self.pending_clip = clip
            self.events.put(("pending", message("iPhone 断线期间复制的{kind}未覆盖电脑剪贴板，可从托盘菜单使用{note}",
                                               kind=kind, note=self.save_history_item(clip))))
            return
        self.pending_clip = None
        try:
            self.writer.put(clip)
        except ClipTooLarge:
            self.events.put(("sync_error", message("图片过大，未同步到电脑")))
            return
        except Exception:
            self.events.put(("sync_error", message("写入 Windows 剪贴板失败")))
            return
        self.events.put(("synced", message("已同步{kind} · 可直接粘贴{note}",
                                      kind=kind, note=self.save_history_item(clip))))

    def save_history_item(self, clip: Clip) -> Message | str:
        if not self.save_switch.is_set():
            return ""
        try:
            return "" if self.history.add(clip) else message(" · 项目过大，未加入历史")
        except Exception:
            return message(" · 历史保存失败")

    def use_pending(self):
        clip, self.pending_clip = self.pending_clip, None
        self.tray.update_menu()
        if clip is None:
            return
        try:
            self.writer.put(clip)
            kind = message("图片" if clip.image else "文本")
            self.set_status(message("已使用 iPhone 上的{kind} · 可直接粘贴", kind=kind))
        except Exception:
            self.set_status(message("写入 Windows 剪贴板失败"))

    def notify(self, message, *, alert=False, always=False):
        """Show a Windows notification; an alert repeats only when its text changes.

        ``always`` is for first-run guidance, which shows even with notifications switched off.
        """
        if alert:
            if message == self.last_alert:
                return
            self.last_alert = message
        if not (always or self.notify_var.get()):
            return
        try:
            self.tray.notify(str(message), APP_NAME)
        except Exception:
            pass

    def set_icon(self, connected):
        if connected != self.icon_connected:
            self.icon_connected = connected
            self.tray.icon = tray_icon(connected)

    def mark_connected(self):
        if self.disconnect_notified:
            self.notify(message("已重新连接 iPhone"))
        self.connected = True
        self.disconnected_at = None
        self.disconnect_notified = False
        self.last_alert = None
        self.set_icon(True)

    def mark_disconnected(self):
        if self.connected:
            self.connected = False
            self.disconnected_at = time.monotonic()
            self.last_connected = datetime.now().strftime("%H:%M")

    def check_connection(self):
        """Wait out brief drops (e.g. iPhone sleeping) before changing the icon or notifying."""
        if self.connected or self.disconnected_at is None:
            return
        elapsed = time.monotonic() - self.disconnected_at
        if elapsed >= ICON_GRACE:
            self.set_icon(False)
        if elapsed >= DISCONNECT_NOTICE and not self.disconnect_notified and self.bridge_state not in ("developer", "hold"):
            self.disconnect_notified = True
            self.notify(message("iPhone 已断开 {minutes} 分钟，正在自动重连。请确认手机已解锁并在同一局域网",
                                minutes=DISCONNECT_NOTICE // 60))

    def drain(self):
        """Keep consuming queued actions after an individual poll fails."""
        try:
            self._drain_tick()
        except Exception as error:
            log_tray_error("drain", error)
        finally:
            if not self.closed:
                self.root.after(100, self.drain)

    def _drain_tick(self):
        if self.wake():
            self.show()
        while True:
            try:
                state, event = self.events.get_nowait()
            except queue.Empty:
                break
            if state == "show":
                self.show()
            elif state == "quit":
                self.quit()
                return
            elif state == "open_setup":
                self.show("settings")
            elif state == "use_pending":
                self.use_pending()
            elif state == "sent_item":
                self.popup.sent(int(event))
            elif state == "offer":
                self.handle_offer(event)
            elif state == "file_ready":
                self.file_ready(*event)
            elif state == "file_error":
                self.file_failed(*event)
            elif state == "connected" and self.bridge.reset_requested.is_set():
                continue  # A connection that was just forgotten.
            else:
                if state == "driver":
                    self.installing = False
                    self.driver_ready = apple_driver_installed()
                if state == "connected" and not self.paired:
                    self.paired = True
                    self.bridge.hold_failures.clear()  # from now on, reconnect automatically
                    self.just_paired = True
                    wireless = event == CONNECTED_WIFI
                    self.awaiting_wireless = not wireless
                    self.notify(message("iPhone 已连接成功。复制即可同步到电脑。" if wireless else
                                        "iPhone 已连接成功。现在可以拔掉数据线，之后在同一 Wi-Fi 下自动连接。"),
                                always=True)
                    if not self.popup.visible:
                        self.show()
                elif state == "connected" and self.awaiting_wireless and event == CONNECTED_WIFI:
                    self.awaiting_wireless = False
                    self.notify(message("已通过 Wi-Fi 连接 iPhone，在手机上复制即可同步到电脑"), always=True)
                if state in ("synced", "held", "pending", "unsupported") and self.offer is not None:
                    self.clear_offer()  # something newer was copied on the iPhone
                if state == "held":
                    self.tray.update_menu()
                    self.refresh()  # quietly: the user chose not to be interrupted by iPhone copies
                if state in ("synced", "pending"):
                    self.last_alert = None
                    self.tray.update_menu()
                    if self.popup.visible:
                        self.refresh()
                    else:
                        self.notify(event)
                if state in ("error", "sync_error", "push_error") or (state in ("developer", "hold") and self.paired):
                    self.notify(event, alert=True)
                if state == "pushed_manual" and not self.popup.visible:
                    self.notify(event)
                if state == "connected":
                    self.mark_connected()
                elif state in ("waiting", "error", "developer", "hold"):
                    self.mark_disconnected()
                if state in ("setup", "pairing", "connecting", "connected", "waiting", "error", "developer", "hold"):
                    self.bridge_state = state
                    self.connection_message = event
                if state in ("connecting", "waiting", "error") and not self.connected and self.last_connected:
                    event = message("{status}（上次连接 {time}）", status=event, time=self.last_connected)
                if state in ("waiting", "error", "sync_error", "push_error", "developer", "hold") or (
                        state == "setup" and event != IDLE_PROMPT):
                    color = WARN
                elif state == "unsupported":
                    color = MUTED
                else:
                    color = ACCENT if self.connected else MUTED
                self.status_color = color
                self.set_status(event)
                self.update_onboarding()
        self.check_connection()
        self.watch_ticks += 1
        if self.watch_ticks % 5 == 0:
            self.check_pc_clipboard()
        if self.watch_ticks % 2 == 0:
            self.popup.animate_progress()

    def show(self, page="history"):
        if not self.paired or self.just_paired:
            if page != "settings":
                page = "onboarding"
            self.driver_ready = self.paired or apple_driver_installed()
            self.update_onboarding()
        monitor = win32api.MonitorFromPoint(win32api.GetCursorPos(), win32con.MONITOR_DEFAULTTONEAREST)
        _, _, right, bottom = win32api.GetMonitorInfo(monitor)["Work"]
        self.load_entries()
        self.popup.set_settings(self.settings_state())
        self.popup.show(right - self.px(12), bottom - self.px(12), page)

    def hide(self):
        if self.popup is not None:
            self.popup.hide()

    def quit(self):
        self.closed = True
        for operation, stop in (("bridge.stop", self.bridge.stop), ("tray.stop", self.tray.stop)):
            try:
                stop()
            except Exception as error:
                log_tray_error(operation, error)
        self.root.destroy()


def run(minimized=False, wake=lambda: False):
    enable_dpi_awareness()
    window = Window(minimized, wake)
    window.root.mainloop()
