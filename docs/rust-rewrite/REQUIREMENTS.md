# CopiaGo Rust 版开发需求文档

版本：v1.0（2026-11-23）
依据：`poc-rust/REPORT.md`（协议原型与动画演示的实测结论）、Python 版 1.2.0 源码（`source/iphone_clip/`）
配套：`docs/rust-rewrite/TASKS.md`（可直接分派给编码代理的任务卡）

现有功能的行为以 Python 1.2.0 源码为准。遇到差异时按 `source/iphone_clip/` 调整实现和验收。

---

## 1. 背景与目标

CopiaGo 是 Windows 托盘应用，通过 iOS 17+ 开发者隧道（CoreDevice / RemoteXPC 剪贴板服务 + notification proxy）在 iPhone 和电脑之间同步剪贴板。用户先用 USB 配对，之后可以走 Wi‑Fi。

Python 版瘦身后：安装包 25 MB、安装后 74 MB、连上手机空闲约 71 MB 私有内存。解释器和 cryptography、用户态 TCP 栈等的固定开销决定了它到不了用户目标。Rust 原型已经实测证明可行：

| 指标 | 用户目标 | Python 1.2.0 | Rust 原型实测 | Rust 正式版目标（本需求的验收线） |
|---|---|---|---|---|
| 安装包 | ≤ 10 MB | 25 MB | 协议层 exe 1.99 MB | **≤ 6 MB** |
| 安装后 | ≤ 30 MB | 74 MB | 1.99 MB | **≤ 12 MB**（不含运行时下载的开发者镜像） |
| 空闲内存（任务管理器"内存"列） | ≤ 30 MB | ~71 MB | 协议层 1.4 MB；弹窗演示 2.7–4.6 MB | **托盘空闲 ≤ 10 MB；弹窗打开 ≤ 20 MB；任何瞬时峰值 ≤ 30 MB** |
| 冷启动到连上（Wi‑Fi） | — | 数秒 | ~1.05 s | **≤ 3 s**（手机已解锁、同一局域网） |

**本项目目标**：用 Rust 重写 CopiaGo，**功能与 Python 1.2.0 对齐**，新增流畅的动画反馈，达到上表指标，并**无缝接管 Python 版的用户数据**（配对、历史、设置），用户升级后不需要重新配对。

### 1.1 非目标（本期不做）

- macOS / Linux 版本。
- iOS 17 以下的设备（lockdown 旧版剪贴板方案）。
- 新的同步类型（如视频以外的大文件分片续传、多台 iPhone 同时连接）。
- 修改 Python 版代码。Python 版在 Rust 版发布前继续维护，两者共存于仓库。

---

## 2. 已验证的技术结论（来自原型，开发时直接沿用）

| 领域 | 结论 | 证据 |
|---|---|---|
| 设备协议 | `idevice` crate **钉死 `=0.1.68`**（MIT）。usbmux、lockdown、CoreDeviceProxy、jktcp 用户态 TCP、RSD、`PasteboardServiceClient`（PULL/SET/RESOLVE/AUTONOTIFY）、`NotificationProxyClient`、RemotePairing（pair/pair‑verify/createListener）、原生 TLS‑PSK 全部可用 | `poc-rust/src/main.rs` |
| TLS 后端 | `default-features = false` + `ring`，比默认的 aws‑lc 小 | `poc-rust/Cargo.toml` |
| 异步运行时 | tokio `current_thread`，进程 2–4 个线程 | 原型实测 |
| Wi‑Fi 发现 | `_remotepairing._tcp` mDNS。原型用的 `mdns-sd` 占约 0.5 MB，**正式版改用 Windows `DnsServiceBrowse`（dnsapi.dll）**，需同时兼容 Python 版 `windows_discovery.py` 解决的多网卡/VPN 问题 | REPORT 体积细节 |
| 配对记录复用 | `remote_<udid>.plist` = `b"ICB-DPAPI1\0"` + `CryptProtectData`；键 `public_key`/`private_key`/`remote_unlock_host_key`/`peer_alt_irk`；缺 `identifier`，按 `uuid3(NAMESPACE_DNS, hostname).upper()` 推导后 pair‑verify 通过 | `poc-rust/src/main.rs::load_rp_record` |
| 剪贴板变化检测 | 订阅 Darwin 通知 `com.apple.pasteboard.notify.changed`，收到后 `PULL promiseSecondary`；**不要用 AUTONOTIFY**（Notes 复制会让守护进程卡 ~64 s） | Python `device.py::_watch`、pymobiledevice3 模块说明（仅参考协议） |
| 开发者镜像 | idevice 支持 personalized（`mobile_image_mounter::mount_personalized_rsd` + TSS）和 Cryptex1（`cryptexd` + `tss`）| REPORT 第 5 步 |
| UI 技术栈 | Win32 窗口（`WS_EX_NOREDIRECTIONBITMAP`）+ Windows.UI.Composition（desktop interop）+ Direct2D/DirectWrite 画进合成表面；动画全部交给 DWM | `poc-rust/anim-demo/` |
| GPU 设备 | **D2D 必须用 WARP**。硬件 D3D 设备会加载显卡用户态驱动，多 15–50 MB 私有内存、+16 个线程 | REPORT 动画演示 |
| 已知坑 | ① `LayerVisual.Shadow` 会盖住本层内容 → 阴影层只放轮廓，内容层放在其上；② windows‑rs 无法构造 `IReference<Vector3>` → Vector3 弹簧改成按分量（`Offset.Y`、`Scale.X`）的标量弹簧；③ 不同机器 `LayerVisual` 行为可能不同，需在 Win10 22H2 / Win11 各验证一次 | anim-demo 源码注释 |
| 系统 SQLite | Win10+ 自带 `winsqlite3.dll`，可用它实现历史库，0 体积增量 | 本机已确认 |
| HEIC | WIC 需要 HEVC 扩展才能解 HEIC（本机没有），**需要兜底方案** | REPORT 功能清单 |

---

## 3. 总体架构

### 3.1 仓库与 crate 划分

新建 Cargo workspace `copiago-rs/`（仓库根目录下，与 `source/` 并列；`poc-rust/` 保留作参考，不并入）：

```
copiago-rs/
  Cargo.toml                # workspace，统一 profile：opt-level="z", lto, codegen-units=1, strip, panic="abort"
  crates/
    copiago-core/           # 纯逻辑，无 Win32 UI：模型、设置、历史、文件、i18n、数据目录、DPAPI 封装
    copiago-device/         # idevice 封装：连接管理、配对、隧道、剪贴板读写、通知、Wi-Fi 发现
    copiago-ddi/            # 开发者镜像：下载、校验、TSS 签名、挂载
    copiago-win/            # Windows 集成：剪贴板读写/监听、托盘、开机启动、单实例、通知气泡、WinHTTP
    copiago-ui/             # Composition + D2D 弹窗、动画工具、设置页
    copiago-app/            # 可执行文件：组装、线程模型、事件循环
  xtask/ 或 scripts/        # 打包、安装器、体积/内存检查脚本
```

依赖方向：`app → ui, win, device, ddi, core`；`device, ddi, ui, win → core`；`core` 不依赖其他 crate。`core` 和 `device` 的核心逻辑要能在没有手机的情况下单测（用 trait 抽象设备）。

### 3.2 线程模型

| 线程 | 职责 | 机制 |
|---|---|---|
| UI 线程（主线程） | Win32 消息循环、DispatcherQueue、托盘、弹窗、系统剪贴板读写 | `GetMessageW` 循环；`CreateDispatcherQueueController(DQTYPE_THREAD_CURRENT)` |
| 设备线程 | tokio `current_thread` 运行时：连接、隧道、剪贴板拉取/推送、通知订阅、镜像下载 | 独立 `std::thread` |
| 通信 | UI → 设备：`tokio::sync::mpsc` 命令；设备 → UI：`std::sync::mpsc` 事件 + `PostMessageW(WM_APP_EVENT)` 唤醒 | 不共享可变状态 |

### 3.3 事件与命令（接口契约）

设备层对外只暴露两个枚举（名称供参考，实施时可调整，但语义必须覆盖）：

- `DeviceEvent`：`Status(ConnState, Message)`、`DeviceInfo(DeviceInfo)`、`Clip { clip, while_disconnected }`、`FileOffer(FileOffer)`、`Unsupported { kind }`、`SetupProgress(DdiStage, Progress)`、`PairingPrompt(Message)`、`Error(Message)`。
- `DeviceCommand`：`Push(Clip)`、`FetchOffer(FileOffer)`、`RequestPair`、`Forget`、`Retry`、`Shutdown`。

`ConnState` 至少包括：`Idle`（未配对）、`Setup`（等待 USB 配对）、`Pairing`、`Waiting`（已配对，未找到设备）、`Connecting`、`Developer`（镜像准备中/失败）、`Connected { wireless }`、`Unsupported`。与 Python 版 `device.py` 中 `status()` 的状态名一一对应。

---

## 4. 功能需求

编号规则：`F-<模块>-<序号>`。每条都要有自动化测试或手工验收项（见第 8 节）。"与 Python 一致"指行为与 `source/iphone_clip/` 对应函数一致，实现时以源码为准。

### 4.1 数据目录与迁移（core）

| 编号 | 需求 |
|---|---|
| F-DATA-1 | 数据目录解析规则与 `settings.py::default_data_dir` / `data_dir` 完全一致：环境变量 `ICB_DATA_DIR` 优先；安装版为 `<exe 目录>\data\<账户名>`（账户名按 `account_folder_name` 清洗非法字符）；exe 旁有 `portable-data` 文件时为 `<exe 目录>\data`；开发运行用 `%LOCALAPPDATA%` 旧目录。 |
| F-DATA-2 | 新建目录时调用与 `restrict_to_current_user` 等效的 ACL 设置（受保护 DACL，仅当前用户 + SYSTEM，子项继承）。 |
| F-DATA-3 | 直接读取 Python 版数据，不做格式转换：`settings.json`、`history.sqlite3`、`device-data\remote_*.plist`、`device-data\<udid>.plist`、`device-data\device-info.bin`、`device-data\developer-image-source`、`device-data\Xcode_iOS_DDI_Cryptex\`、`received\`。 |
| F-DATA-4 | 首次以 Rust 版打开 RP 配对记录时，把推导出的 `identifier` 写回记录（DPAPI 重新封装，原子替换），之后不再依赖主机名；电脑改名不影响 Wi‑Fi 连接。保留 Python 版能读的所有原字段。 |
| F-DATA-5 | 写文件一律"写临时文件 + `ReplaceFileW`/`MoveFileExW` 原子替换"。 |

### 4.2 加密（core）

| 编号 | 需求 |
|---|---|
| F-SEC-1 | DPAPI 封装：`protect(data) = b"ICB-DPAPI1\0" + CryptProtectData(data, "iPhone Clipboard pairing record", flags=CRYPTPROTECT_UI_FORBIDDEN)`；`unprotect` 没有魔数时原样返回（兼容明文旧记录）。 |
| F-SEC-2 | 历史库 `preview`/`payload` 列用 `CryptProtectData(data, "iPhone Clipboard")`（无魔数，与 `history.py::encrypt` 一致）。 |
| F-SEC-3 | `device-info.bin` = `protect(JSON{name, model, os_version})`。 |
| F-SEC-4 | 私钥等敏感数据在内存中用完即清零（`zeroize`），日志中不得出现剪贴板内容、配对密钥、设备名以外的个人信息。 |

### 4.3 设置（core）

`settings.json` 键与默认值必须与 Python 一致：

| 键 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `language` | `"system" \| "zh-CN" \| "en"` | `system` | 界面语言 |
| `history` | bool | `false`（仅 `true` 视为开启） | 保存 iPhone 历史 |
| `pc_history` | bool | `false` | 保存电脑侧历史 |
| `pull` | bool | `true`（仅 `false` 视为关闭） | iPhone → 电脑自动同步 |
| `push` | bool | `false` | 电脑 → iPhone 自动同步 |
| `pull_types` | list ⊆ `text,image,file` | 全部 | 自动拉取的类型 |
| `push_types` | list ⊆ `text,image` | 全部 | 自动推送的类型 |
| `file_ask_mb` | `10\|20\|50\|0` | `20` | 超过此大小的文件需用户确认，0 = 不询问 |
| `notifications` | bool | `true` | 系统通知 |
| `update_check` | bool | `true` | 启动检查更新 |

| 编号 | 需求 |
|---|---|
| F-SET-1 | 读取时容忍未知键和非法值（回落默认），写入时保留未知键。 |
| F-SET-2 | 开机启动：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的值，与 `ui.py::startup_command` 同名同格式，便于两版互相识别。 |

### 4.4 i18n（core）

| 编号 | 需求 |
|---|---|
| F-I18N-1 | 中文为源字符串，英文对照表从 Python `i18n.py::ENGLISH` 迁移，**全部条目**都要迁移，并加一个测试保证 Rust 版不少于 Python 版的条目数。 |
| F-I18N-2 | `system` 时按 `GetUserDefaultUILanguage` 判断：主语言为中文 → `zh-CN`，否则 `en`。 |
| F-I18N-3 | 支持命名占位符（`{kind}`、`{time}` 等），与 Python 模板一致。 |

### 4.5 设备连接（device）

| 编号 | 需求 |
|---|---|
| F-DEV-1 | **USB 发现**：usbmux 列设备，只取 `Connection::Usb`。配对时有多台 USB 设备，提示"请暂时只连接一台 iPhone"。 |
| F-DEV-2 | **USB 配对**（与 `device.py::pair_over_usb` 一致）：lockdown 自动配对（手机弹"信任"），读取 DeviceName/ProductType/ProductVersion 存入 device-info；若 `EnableWifiConnections` 为假则打开；通过 `RemotePairingLockdownService` 完成 RPPairing，保存 `remote_<udid>.plist`（DPAPI，含 `identifier`）。各阶段通过 `PairingPrompt` 推送提示文案。整体超时 120 s。 |
| F-DEV-3 | lockdown 配对记录优先从 usbmuxd 取，失败时回落到 `device-data\<udid>.plist`（原型 `Provider` 已实现）。 |
| F-DEV-4 | **连接策略**（同 `device.py` 的连接主循环）：已配对时先走 Wi‑Fi（mDNS → pair‑verify → createListener → TLS‑PSK）。无线连接失败时，若同一已配对手机在 USB 上可用，再试 CoreDeviceProxy。中止、重置或镜像流程的下载/签名错误按原状态处理。无线安装阶段错误允许尝试 USB，普通 USB 失败仍保留已知的安装错误。连接后状态文案："已连接（USB） · 可以拔掉数据线，之后将通过无线连接" / "已连接（无线）"。 |
| F-DEV-5 | **断线重连**：隧道断开、通知流结束、或连续 3 次元数据拉取失败（每 2 s 一次）时判定断线。普通连接失败等待 4 s 后重试。已配对时，开发者模式、镜像流程或首次配置的挂起错误保留提示并等待用户重试。USB 插拔更新回退资格，正常无线会话保持连接。 |
| F-DEV-6 | **睡眠/唤醒**：收到 `WM_POWERBROADCAST(PBT_APMRESUMEAUTOMATIC)` 或网络变化（`NotifyIpInterfaceChange`）时主动重建连接。 |
| F-DEV-7 | **RP 控制连接**必须在隧道生命周期内持有（原型用 `mem::forget` 只是权宜），隧道关闭时一并关闭。 |
| F-DEV-8 | 服务列表中没有 `com.apple.coredevice.pasteboardservice` 时进入开发者镜像流程（F-DDI），完成后重连一次；仍没有则报"install"类错误（与 `DeveloperSetupError("install")` 对应）。 |
| F-DEV-9 | 连接后读取设备信息：RSD 的 ProductType/ProductVersion + 隧道内 lockdown 的 DeviceName（3 s 超时，失败用已保存的名字），发出 `DeviceInfo` 事件并保存。机型名映射表从 `device_info.py::model_name` 迁移。 |
| F-DEV-10 | **忘记设备**：删除所有 `remote_*.plist` 和 device-info，断开当前连接，回到 `Setup`。 |
| F-DEV-11 | **Wi‑Fi 发现**用 `DnsServiceBrowse` + `DnsServiceResolve`，在每块 IPv4 网卡上查询（排除 127./169.254./0.），IPv4 优先。用 idevice `remote_pairing::PeerDevice::validate_auth_tag` 校验 `_remotepairing._tcp` 的 TXT 与配对记录，匹配结果用于选择候选。旧记录或广播缺少匹配材料时仍可尝试 `pair-verify`。所有候选均由 `pair-verify` 完成认证。每轮发现最多等待 5 s，连接重试按 F-DEV-5。 |

### 4.6 剪贴板同步（device + win）

| 编号 | 需求 |
|---|---|
| F-CLIP-1 | **变化检测**（与 `device.py::_watch` 一致）：notification proxy 订阅 `com.apple.pasteboard.notify.changed`、`com.apple.springboard.lockstate`、`com.apple.mobile.keybagd.lock_status`；另外每 2 s 用 `PULL allPromised` 拉一次元数据兜底；用 `(nonce, changeCount)` 作为变化标识。 |
| F-CLIP-2 | **拉取策略**（与 `read_clip` 一致）：先 `allPromised` 只取类型（8 s 超时）→ 是文件且有内容类型 → 走文件描述（F-FILE）；没有文本/图片类型 → 判定为不支持，按 `unsupported_kind` 提示；否则 `promiseSecondary`（15 s）；主类型不是图片但声明了图片类型时再 `allResolved`（30 s）。超时只跳过这次复制，不断开连接。 |
| F-CLIP-3 | 文本过滤：去掉 `￼`、空白后为空的文本不算内容；图片优先级 PNG > JPEG > HEIC > TIFF > GIF > WebP；Notes/Safari 的 `com.apple.webarchive` 里的首张图片作为兜底。 |
| F-CLIP-4 | **断线期间的复制**：重连后发现 `(nonce, changeCount)` 与上次不同，以 `while_disconnected = true` 投递。宿主按 `pull_types` 过滤。关闭拉取，或旧首包遇到电脑剪贴板已被其他程序更新时，保存给托盘菜单"使用 iPhone 上最新复制的内容"。拉取开启且电脑剪贴板未被其他程序更新时，旧首包照常同步。 |
| F-CLIP-5 | **推送**（电脑 → iPhone）：监听 Windows 剪贴板（`AddClipboardFormatListener`），`push` 开启且类型在 `push_types` 中时 SET 到手机；文本用 `public.utf8-plain-text`/`public.plain-text`/`public.text` 三个 UTI；图片转 PNG 以 `public.png` 发送。SET 超时 20 s。 |
| F-CLIP-6 | **回声抑制**：本机推送后 120 s 内，若手机回来的内容指纹（`clipboard.py::fingerprint`）与刚推送的相同则丢弃；推送回复中的 changeCount 记为 `last_change`。同理，写入 Windows 剪贴板时要打标记，避免被自己的监听再推回手机。 |
| F-CLIP-7 | **写入 Windows 剪贴板**：文本 `CF_UNICODETEXT`；图片同时提供 `CF_DIBV5`（含透明）和 `PNG` 注册格式；文件见 F-FILE-4。打开剪贴板按 `clipboard.py::_open` 最多尝试 8 次，失败后依次等待 50、100、150、200、250、300、350 ms。 |
| F-CLIP-8 | 图片限制：编码后 ≤ 64 MB、像素 ≤ 5000 万（48MP 照片可通过），超出则提示并跳过。 |
| F-CLIP-9 | 同一时刻对剪贴板服务的请求串行化（Python 用 `rsd_lock`）。可以像 pymobiledevice3 `PasteboardMonitor` 那样每个请求单独开一条服务连接，避免一个慢回复阻塞其他请求。 |

### 4.7 文件（device + win + core）

| 编号 | 需求 |
|---|---|
| F-FILE-1 | 手机"文件"App 复制的文件：用 `thresholdData(256 KB)` 拉描述，得到文件名（从文件描述符解析，规则同 `files.py::name_from_descriptor`）、UTI、大小；≤ 256 KB 的文件直接随描述带回。 |
| F-FILE-2 | 大小 > `file_ask_mb` 时，弹窗显示"文件待接收"卡片，用户点击后才传输；≤ 阈值时自动传输。上限 512 MB（`MAX_FILE_BYTES`）。超时 = min(900 s, 60 s + 大小 / 1 MB/s)。 |
| F-FILE-3 | 传输前重新 PULL 元数据，changeCount 变化则提示"iPhone 上已复制了别的内容"（`OfferExpired`）。 |
| F-FILE-4 | 文件保存到 `received\<时间戳目录>\<安全文件名>`（命名规则同 `safe_name`），以 `CF_HDROP` 写入剪贴板，可直接粘贴到资源管理器。超过 24 h 的目录启动时清理（`KEEP_SECONDS`）。 |
| F-FILE-5 | 512 MB 的文件不能整块放进内存：RESOLVE 返回的数据流式写盘。若 idevice 0.1.68 的 `resolve` 只能整块返回，需评估并在 TASKS 中单列（见 T-DEV-07）。 |

### 4.8 历史（core）

| 编号 | 需求 |
|---|---|
| F-HIST-1 | 与 Python 共用 `history.sqlite3`：表 `items(id INTEGER PRIMARY KEY, created TEXT, kind TEXT, preview BLOB, payload BLOB, source TEXT DEFAULT 'phone')`，`PRAGMA secure_delete = ON`；没有 `source` 列时自动 `ALTER TABLE`。 |
| F-HIST-2 | 两个列表 `phone`/`pc` 各自限制：≤ 50 条、≤ 128 MB，单条 ≤ 32 MB；与上一条内容相同则不重复插入（逻辑同 `history.py::add`）。 |
| F-HIST-3 | 实现优先用系统 `winsqlite3.dll`（动态加载）。若它在目标系统上不可用或 API 不全，再改用 `rusqlite` bundled，并在体积报告里记录增量。 |
| F-HIST-4 | 关闭"保存历史"时清空对应列表。 |

### 4.9 图片处理（win）

| 编号 | 需求 |
|---|---|
| F-IMG-1 | 解码、缩放、编码统一用 WIC：PNG/JPEG/TIFF/GIF/BMP/WebP（WebP 需系统扩展，没有时回落为不支持）。 |
| F-IMG-2 | HEIC：先试 WIC；失败时用兜底解码器。兜底方案二选一，由 T-IMG-02 评估后定：(a) 附带 libheif + libde265 DLL（LGPL，动态链接，约 +1.5–2.5 MB）；(b) 拉取时优先请求手机同时提供的 `public.jpeg`/`public.png`，HEIC 只在没有其他格式时才需要解码。 |
| F-IMG-3 | 缩略图按目标尺寸解码（WIC `IWICBitmapScaler` / JPEG 的 `IWICBitmapSourceTransform`），禁止把 48MP 原图解成 RGBA 后再缩小；用完立即释放，保证瞬时峰值 ≤ 30 MB。 |

### 4.10 开发者镜像（ddi）

| 编号 | 需求 |
|---|---|
| F-DDI-1 | 镜像来源与固定哈希沿用 `developer_setup.py`：仓库 `doronz88/DeveloperDiskImage`，固定提交 `DDI_REF`，三个镜像源（raw.githubusercontent.com、ghfast.top、fastly.jsdelivr.net），记住上次成功的源（`developer-image-source` 文件）。`PINNED_FILES` 里的大小和 SHA‑256 必须逐一校验。 |
| F-DDI-2 | 下载用 WinHTTP：断点续传（`Range`）、30 s 无数据视为停滞并换源、总超时 15 min、可取消、进度事件（已下载/总量/速度）。先下到 staging 目录，校验通过后原子移动。 |
| F-DDI-3 | 签名与挂载：iOS 17+ 用 Cryptex1（`cryptexd` + `tss`，ticket 键 `Cryptex1,Ticket`）；回退为 personalized（`mobile_image_mounter`，`ApImg4Ticket`）。TSS 签名超时 45 s，安装超时 240 s。已挂载视为成功。 |
| F-DDI-4 | 每个阶段（download/sign/install）的结果写入 `developer-setup.log`（与 `record_setup_event` 格式一致），失败时给出可操作的文案（网络、Apple 服务器、手机未解锁、开发者模式未开等）。 |
| F-DDI-5 | 检查开发者模式状态（`query_developer_mode_status`），未开启时引导用户到"设置 > 隐私与安全性 > 开发者模式"。 |

### 4.11 托盘与系统集成（win）

| 编号 | 需求 |
|---|---|
| F-TRAY-1 | `Shell_NotifyIconW` 托盘图标，按连接状态切换图标（已连接/未连接/错误），支持深浅色任务栏。图标资源从 `source/iphone_clip/assets` 迁移为 `.ico`。 |
| F-TRAY-2 | 左键单击打开历史弹窗；右键菜单：剪贴板历史（默认项）、使用 iPhone 上最新复制的内容（有待处理内容时才可用）、连接与设置、退出。 |
| F-TRAY-3 | Explorer 重启（`TaskbarCreated` 消息）后重新添加图标；任务栏刚启动时添加失败，按 1.5 s 间隔重试 10 次（`promote_tray`）。 |
| F-TRAY-4 | 单实例：命名互斥量；第二个实例启动时通知已运行实例打开弹窗然后退出。 |
| F-TRAY-5 | 系统通知（`notifications` 开启时）：收到手机内容、文件待接收、连接失败等，用托盘气泡（`NIF_INFO`），不引入 WinRT Toast 依赖。 |
| F-TRAY-6 | 高 DPI：进程声明 Per‑Monitor V2；托盘图标和弹窗随所在显示器 DPI 变化（`WM_DPICHANGED`）。 |

### 4.12 弹窗 UI（ui）

视觉沿用 Python 版 `popup.py`：宽 392、圆角 24、内边距 18、行高 60、行距 8、列表视口 310；配色 `INK (23,32,43)`、`MUTED (93,104,117)`、`CARD (244,246,249)`、`ACCENT (15,110,90)` 等，状态胶囊三色见 `CHIPS`。字体 Microsoft YaHei UI，图标 Segoe Fluent Icons（Win10 回落 Segoe MDL2 Assets）。

| 编号 | 需求 |
|---|---|
| F-UI-1 | 弹窗贴近托盘位置弹出（按任务栏方向计算，支持任务栏在四个边、多显示器）；失去焦点 150 ms 后自动隐藏（`dismiss_if_unfocused`）；Esc 关闭。 |
| F-UI-2 | **历史页**：顶部应用名 + 状态胶囊 + 状态说明；两个标签页"来自 iPhone / 来自电脑"（`pc_history` 开启时显示第二个）；列表项显示类型图标、预览（文本截断/图片缩略图/文件名+大小）、时间（今天 HH:MM、昨天、MM-DD、跨年 YYYY-MM-DD，同 `short_time`）；点击条目复制到电脑剪贴板（iPhone 列表）或推送到手机（电脑列表）；右键或悬停按钮可删除单条；底部"清空"。 |
| F-UI-3 | **连接与设置页**：配对引导（未配对时）、设备卡片（名称/机型/系统版本）、忘记设备（二次确认，3 s 内再点一次生效，同 `confirm_job`）；全部设置项（4.3 节）的开关和选项；语言切换即时生效；关于与版本号、检查更新结果。 |
| F-UI-4 | **状态反馈**：连接中、镜像下载/签名/安装进度条、文件待接收卡片（带"接收"按钮和大小）、不支持类型提示、错误提示（可重试）。 |
| F-UI-5 | 列表很长时可滚动（鼠标滚轮、触控板），滚动有惯性动画。 |
| F-UI-6 | 键盘可用：Tab 在可交互元素间移动，Enter/Space 激活，焦点有可见样式。 |
| F-UI-7 | 弹窗隐藏时释放全部 GPU 资源（视觉树、表面、D2D/D3D 设备），只保留托盘所需部分（anim-demo 已实现此模式）。 |
| F-UI-8 | 亮/暗色：首期只做亮色（与 Python 版一致），但颜色集中定义成主题结构，便于以后加暗色。 |

### 4.13 动画（ui）

动画全部由合成器驱动，UI 线程只负责设置目标值。参数取 anim-demo 中已调好的值，除非设计另有要求。

| 编号 | 场景 | 规格 |
|---|---|---|
| F-ANI-1 | 弹窗出现 | 从下方 36 px 处弹簧上浮（周期 70 ms，阻尼 0.72），缩放 0.94→1，透明度 0→1（220 ms）；任务栏在其他边时从对应方向进入 |
| F-ANI-2 | 弹窗隐藏 | 位移 24 px、缩放 →0.97、透明度 →0（180–200 ms），结束后隐藏窗口并释放资源 |
| F-ANI-3 | 状态切换 | 胶囊背景色和圆点颜色 320 ms 渐变（RGB 插值）；胶囊宽度跟着文字变化并保持右对齐；文字交叉淡化（新文字从 +6 px 淡入，旧文字向 −6 px 淡出） |
| F-ANI-4 | 连接中 | 圆点呼吸（透明度 1→0.25→1、缩放 1→1.35→1，1100 ms 循环）；状态稳定时圆点弹一下（1.6→1，弹簧阻尼 0.45） |
| F-ANI-5 | 进度 | 进度条宽度连续缓动（700 ms）；完成后停留约 0.5 s 再淡出；不确定进度时用往返流光 |
| F-ANI-6 | 新条目 | 从 −18 px、透明度 0、缩放 0.97 进入；已有条目用隐式 Offset 动画（420 ms）下移；超出上限的条目淡出后移除 |
| F-ANI-7 | 删除条目 | 条目淡出并收起高度，下方条目上移补位 |
| F-ANI-8 | 悬停/按压 | 背景色 120–180 ms 渐变；按下弹簧缩到 0.94–0.98，松开回弹（周期 55 ms，阻尼 0.5） |
| F-ANI-9 | 反馈提示 | "已复制 / 已推送到 iPhone"等提示条弹出（缩放 0.8→1 弹簧）、停留约 1.2 s、淡出，总长 1.8 s |
| F-ANI-10 | 页面切换 | 历史页 ↔ 设置页横向滑动 + 交叉淡化（约 300 ms）；标签页指示条滑动到当前标签 |
| F-ANI-11 | 开关控件 | 设置开关的滑块位移和底色同步动画 |
| F-ANI-12 | 缩略图 | 加载完成后淡入（200 ms） |
| F-ANI-13 | 无障碍 | 系统开启"减少动画"（`SPI_GETCLIENTAREAANIMATION` 为假）时，所有动画时长设为 0 或改为简单淡入淡出 |

公共缓动：`cubic-bezier(0.1, 0.9, 0.2, 1.0)`。要把动画封装成工具函数（`animate_scalar/color/v2/v3`、`spring`、`implicit_offset`），参考 `poc-rust/anim-demo/src/main.rs`。

### 4.14 更新检查（win）

| 编号 | 需求 |
|---|---|
| F-UPD-1 | 启动 15 s 后（`UPDATE_CHECK_DELAY_MS`）用 WinHTTP 请求 `https://api.github.com/repos/<REPOSITORY>/releases/latest`，比较版本号，有新版时在弹窗显示提示，点击打开发布页（只允许 `https://github.com/<REPOSITORY>/releases/` 前缀，与 `updater.py` 一致）。 |
| F-UPD-2 | `update_check` 关闭时不发任何请求；失败静默，不重试轰炸。 |

### 4.15 日志与诊断（core）

| 编号 | 需求 |
|---|---|
| F-LOG-1 | 环境变量 `ICB_DIAGNOSTIC_FILE` 指定时写诊断事件（与 `diagnostic_event` 同类事件名：`paired_records=`、`tunnel_open`、`monitor_ready`、`pull timeout types=`、`unsupported types=` 等），只记录类型和大小，**绝不记录内容**。 |
| F-LOG-2 | 崩溃（panic=abort 前的 panic hook）写一行到数据目录下的 `crash.log`，带版本号和位置。 |

### 4.16 安装与分发

| 编号 | 需求 |
|---|---|
| F-PKG-1 | 继续用 Inno Setup（沿用 `setup.iss` 的安装位置、按账户分数据目录、中文向导 `ChineseSimplified.isl`），exe 内嵌版本资源和清单（Per‑Monitor V2、Common Controls 6、`supportedOS` Win10/11）。 |
| F-PKG-2 | 便携版：zip，内含 exe 和 `portable-data` 标记文件。 |
| F-PKG-3 | 升级覆盖安装 Python 版时：自动结束旧进程，保留 `data\`，删除旧的 `_internal\` 和 Python exe；安装后第一次启动能直接连上已配对的手机（F-DATA-3/4）。 |
| F-PKG-4 | CI（GitHub Actions，windows-latest）：`cargo fmt --check`、`clippy -D warnings`、`cargo test`、release 构建、输出 exe 体积，超过预算（exe 6 MB）则失败。 |
| F-PKG-5 | 代码签名步骤预留（证书由维护者提供，CI 用 secret）。 |

---

## 5. 非功能需求

| 编号 | 需求 |
|---|---|
| N-1 | 指标见第 1 节表格，每个阶段结束都要实测并写入 `copiago-rs/METRICS.md`（测法与 REPORT 一致：`Get-Counter '\Process(copiago)\Working Set - Private'`、Private Bytes、线程数）。 |
| N-2 | 支持 Windows 10 21H2+ 与 Windows 11，x64；ARM64 作为后续目标，代码中不得写死 x64。 |
| N-3 | 空闲 CPU：已连接且无复制时，平均 < 0.1%（2 s 元数据轮询除外）。 |
| N-4 | 连续运行 24 h，内存无持续增长（每小时采样一次，增长 < 2 MB）。 |
| N-5 | 所有 `unsafe` 块要有 `// SAFETY:` 注释；`clippy::undocumented_unsafe_blocks` 开启。 |
| N-6 | 依赖许可证仅限 MIT / Apache‑2.0 / BSD / ISC / Zlib / Unicode；LGPL 仅允许以动态链接 DLL 形式附带（HEIC 兜底）。**禁止**引入或逐行翻译 GPL 代码（pymobiledevice3 只能参考协议）。用 `cargo-deny` 检查。 |
| N-7 | 隐私：不上传任何数据；网络请求只有 GitHub（更新检查、镜像）、镜像源、Apple TSS。 |

---

## 6. 分阶段计划

每个阶段都要满足退出条件才能进入下一阶段。人日按单个熟练 Rust + Win32 开发者估算；交给编码代理时按 TASKS.md 拆分。

### M0 · 工程骨架（2–3 人日）
- workspace、profile、CI、`cargo-deny`、体积预算检查、`METRICS.md` 模板。
- `copiago-core`：数据目录（F-DATA-1/2）、DPAPI（F-SEC-1~3）、设置（F-SET-1）、i18n（F-I18N-*）。
- **退出条件**：CI 绿；在 Python 版真实数据目录上读出设置、device-info，单测覆盖解析规则。

### M1 · 无界面同步内核（8–11 人日）
- `copiago-device`：USB/Wi‑Fi 连接、隧道、RSD、剪贴板拉取（F-CLIP-1~3）、推送（F-CLIP-5/6）、通知、断线重连（F-DEV-4~7）、设备信息（F-DEV-9）、`DnsServiceBrowse` 发现（F-DEV-11）、RP 记录写回 identifier（F-DATA-4）。
- `copiago-win`：Windows 剪贴板读写和监听（F-CLIP-7）。
- 一个临时的命令行宿主 `copiago-app --headless`：双向同步文本和图片，打日志。
- **退出条件**：用 Python 版已配对的手机，USB 和 Wi‑Fi 都能双向同步文本/PNG。接线且无线可用时保持 Wi‑Fi，无线失败时回退同一台手机的 USB，回退会话断开后在 Wi‑Fi 可用时恢复无线连接。手机锁屏/解锁、电脑睡眠/唤醒后自动恢复，空闲 30 min 内存 ≤ 5 MB。

### M2 · 配对、文件、历史（6–8 人日）
- USB 配对流程（F-DEV-1~3）、忘记设备（F-DEV-10）。
- 文件 offer/传输/保存/清理（F-FILE-*）、历史库（F-HIST-*）、断线期间复制（F-CLIP-4）。
- 图片：WIC 解码/缩放/编码、HEIC 方案定案并实现（F-IMG-*）。
- **退出条件**：删除配对后能重新配对；"文件"App 复制 1 MB/100 MB 文件能粘贴到资源管理器；历史与 Python 版互通（Python 写的记录 Rust 能读，反之亦然）。

### M3 · 开发者镜像（3–5 人日）
- F-DDI-1~5 全部。
- **退出条件**：在没有挂载镜像的手机上（重启手机即可卸载）首次连接时自动下载、签名、挂载并连上；断网、换源、取消都表现正确。

### M4 · 托盘与弹窗 UI（10–14 人日）
- 托盘（F-TRAY-*）、弹窗框架与布局（F-UI-*）、动画全集（F-ANI-*）、设置页、更新检查（F-UPD-*）。
- 用 M1–M3 的事件驱动 UI，替换 headless 宿主。
- **退出条件**：功能与 Python 1.2.0 逐项对照通过（第 8 节清单）；弹窗打开 ≤ 20 MB、托盘空闲 ≤ 10 MB；150%/200% DPI、多显示器、任务栏四个方向都正常。

### M5 · 打包、迁移、发布（3–5 人日）
- 安装器、便携版、覆盖安装 Python 版的迁移（F-PKG-*）、崩溃日志、24 h 稳定性测试（N-4）。
- **退出条件**：安装包 ≤ 6 MB、安装后 ≤ 12 MB；从 Python 1.2.0 覆盖安装后，不重新配对就能连上；发布 2.0.0-beta。

**合计约 32–46 人日**（与 REPORT 估算的 29–47 人日一致，另加 M0 和动画相关的 3–5 人日）。

---

## 7. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| idevice 0.x 破坏性升级 | 升级时大量改动 | 钉死版本；`copiago-device` 内部封装，idevice 类型不外泄到其他 crate；升级单独排期 |
| idevice 单人维护，新 iOS 支持滞后 | 新系统发布后可能连不上 | 关注上游；必要时 fork 到组织内修补；保留 Python 版作为应急方案直到 Rust 版稳定两个 iOS 大版本 |
| jktcp 大文件吞吐/稳定性未验证 | 512 MB 文件传输可能慢或失败 | M2 首个任务做 100 MB/500 MB 压测（T-DEV-07），不达标则降低上限或改用 RESOLVE 分块 |
| `LayerVisual` 等 Composition 行为在不同机器上有差异 | 视觉错误 | 关键效果做截图回归；Win10 22H2 与 Win11 各留一台测试机 |
| HEIC 依赖 LGPL 库 | 许可证与体积 | 优先方案 (b)：请求替代格式；必须附带时只动态链接 |
| 主机名推导 identifier | 改名后连不上 | F-DATA-4 首次运行写回 |
| 编码代理误用 GPL 代码 | 许可证污染 | 任务卡明确禁止；评审时检查 |

---

## 8. 验收清单（与 Python 1.2.0 对照）

每项都要在真机上手工验证一次，自动化测试能覆盖的部分也要有测试。

- [ ] 首次安装 → 托盘出现 → 弹窗提示连接 USB → 手机点"信任"并输入密码 → 配对成功 → 拔线后无线连接成功
- [ ] 接线且 Wi‑Fi 可用时走无线。无线失败后回退同一已配对手机的 USB，断开回退会话后在 Wi‑Fi 可用时恢复无线。
- [ ] iPhone 复制文字 → 1 s 内出现在电脑剪贴板和历史
- [ ] iPhone 复制照片（HEIC 来源）、截图（PNG）、Safari 图片、Notes 里的图片 → 电脑可粘贴
- [ ] "文件"App 复制小文件（< 256 KB）自动接收；大文件按 `file_ask_mb` 询问；512 MB 以上拒绝并提示
- [ ] 复制视频等不支持的类型 → 提示"目前只同步文字、图片和「文件」App 里的文件"
- [ ] 电脑复制文字/图片 → 手机可粘贴（`push` 开启时），不会回声
- [ ] 断线期间手机复制 → 重连后托盘菜单"使用 iPhone 上最新复制的内容"可用
- [ ] 手机重启（镜像被卸载）→ 自动重新挂载镜像
- [ ] 电脑睡眠唤醒、切换 Wi‑Fi、开关 VPN → 自动恢复
- [ ] 所有设置项生效且重启后保留；语言切换即时生效
- [ ] 忘记设备 → 需要重新 USB 配对
- [ ] 历史：两个列表、上限、删除、清空、关闭保存后清空
- [ ] 开机启动开关与注册表一致
- [ ] 更新检查提示与跳转
- [ ] Python 1.2.0 数据目录原样接管
- [ ] 指标：安装包、安装后体积、托盘空闲内存、弹窗内存、24 h 内存无增长
