# CopiaGo Rust 可行性原型报告

日期：2026-11-23　测试机：Windows 11 Pro 26300，Rust 1.100.0（x86_64-pc-windows-msvc，VS 2022 Build Tools）
测试设备：iPhone16,2（iOS 27.2.1 / 24B156），已被 Python 版 CopiaGo 配对。

## 结论

**可以做到，而且余量很大。** 原型打通了 Python 版用到的全部协议链路：USB 和 Wi‑Fi 两条隧道、RSD、剪贴板读写、变化通知。它直接读取 Python 版现有的 DPAPI 配对记录，不用重新配对。

| 指标 | 用户目标 | Python 版现状 | Rust 原型实测 | 完整重写预估 |
|---|---|---|---|---|
| 安装包 | ≤ 10 MB | 25 MB | exe 1.99 MB（zip 后 1.09 MB） | 3–6 MB |
| 安装后 | ≤ 30 MB | 74 MB | 1.99 MB（单个 exe） | 5–12 MB（主要取决于安装器和 HEIC 解码库） |
| 运行时内存（任务管理器"内存"列） | ≤ 30 MB | 约 71 MB 私有 | **1.3–1.4 MB**（Private Bytes 1.7–1.8 MB，工作集 7.6–8.8 MB） | 托盘空闲 4–10 MB；弹窗显示时 5–10 MB（见动画演示实测） |

原型没有 UI，所以内存数字是协议层的下限。不过加上托盘和 Direct2D 弹窗，空闲时也很难超过 10–15 MB。三项指标都能达标，内存一项比目标低一个数量级。

## 每一步的结果

| # | 步骤 | 结果 | 说明 |
|---|---|---|---|
| 1 | usbmux 列设备 + lockdown 信息 | ✅ 通过 | 通过 Apple Mobile Device Service（127.0.0.1:27015）列出设备。lockdown 不开会话时读 `ProductType`，用 usbmuxd 里的配对记录开 TLS 会话后读 `DeviceName`/`ProductVersion`/`BuildVersion`/`HardwareModel`/`EnableWifiConnections` 都正常。AMDS 里没有记录时，会回退到 Python 版 `device-data\<udid>.plist`（代码已写好；本机 AMDS 里有记录，所以回退路径没有实际触发） |
| 2 | 复用 Python 版配对记录 | ✅ 通过 | `remote_<udid>.plist` = `ICB-DPAPI1\0` + `CryptProtectData`，用 `CryptUnprotectData` 解开后是 `public_key`/`private_key`/`remote_unlock_host_key`/`peer_alt_irk`。idevice 的 `RpPairingFile` 还需要 `identifier` 字段：pymobiledevice3 用 `uuid3(NAMESPACE_DNS, platform.node()).upper()` 生成，按同样规则补上之后 pair‑verify 一次通过。原型没有自己实现 RPPairing，因为 idevice 已有 `RemotePairingClient::pair`（USB 经 `RemotePairingLockdownService`），完整重写时直接用即可 |
| 3 | CoreDevice 隧道 + RSD + 剪贴板服务 | ✅ 通过 | USB：`CoreDeviceProxy` → jktcp 用户态 TCP → RSD，隧道 0.06–0.10 s，握手 0.02–0.03 s。Wi‑Fi：见第 6 步。RSD 列出 85 个服务，`com.apple.coredevice.pasteboardservice` 在列（本机的开发者镜像已由 Python 版挂载），`notification_proxy.shim.remote`、`mobile_image_mounter.shim.remote` 也都在 |
| 4 | 剪贴板读/写 | ✅ 通过 | idevice 0.1.68 自带 `core_device::PasteboardServiceClient`（PULL/SET/RESOLVE/AUTONOTIFY），不用自己写 XPC。实测：`PULL allPromised`（只取类型）、`PULL promiseSecondary`（取文本）、`SET` 写中文和 emoji 文本后回读一致，都在几十毫秒内完成。另外验证了 Python 版的变化检测方式：通过 notification proxy 订阅 `com.apple.pasteboard.notify.changed`，别的连接执行 SET 后 0.02–0.08 s 收到通知。空闲 130 s 后再 PULL，隧道仍然可用 |
| 5 | 开发者镜像挂载（只评估） | ✅ 支持 | idevice 有 `mobile_image_mounter::mount_personalized_rsd`：先查设备上的 personalization manifest，没有就向 Apple TSS 请求签名（`tss` 特性，自带 HTTP 客户端，用 rustls + webpki-roots）。iOS 17+ 的 Cryptex1 DDI（Python 版现在走的 `Xcode_iOS_DDI_Cryptex` 路径）由 `cryptexd` 特性支持，`Cryptex1Assets::load` 读取解包后的镜像目录。下载（多镜像源、断点续传、哈希校验）要自己写 |
| 6 | Wi‑Fi | ✅ 通过 | mDNS 发现 `_remotepairing._tcp`（0.26–0.32 s 收到应答，TXT 有 `identifier`/`authTag`/`ver=26`）→ TCP 49152 → RPPairing pair‑verify（0.15–0.23 s）→ `createListener` → TLS‑PSK（idevice 自带 TLS 1.2 PSK 实现，不依赖 OpenSSL）→ CDTunnel → jktcp → RSD。**从启动 exe 到读出剪贴板总共约 1.05 s。** 测试时 Python 版 CopiaGo 一直连着同一台手机，两条隧道同时存在没有冲突 |

## 体积细节

- `cargo build --release`，profile 是 `opt-level="z"`、`lto=true`、`codegen-units=1`、`strip=true`、`panic="abort"`。
- 完整原型（含 Wi‑Fi/mDNS）：**1,993,216 字节**；去掉 `mdns-sd` 后 1,491,456 字节。mdns-sd 一个依赖就占约 0.5 MB，正式版可以换成 Windows 自带的 `DnsServiceBrowse`（dnsapi.dll，Win10 1809+），几乎不增加体积。
- 用到的 crate：idevice 0.1.68（MIT）、tokio（current_thread 运行时）、ring（rustls 加密后端，体积比默认的 aws-lc 小）、jktcp、plist、windows-sys。依赖树约 180 项，冷编译约 30 s。
- 进程只有 2–4 个线程、约 140 个句柄。

## 内存细节（空闲 2 分钟）

| 链路 | 任务管理器"内存"（专用工作集） | Private Bytes | 工作集 | 峰值工作集 |
|---|---|---|---|---|
| Wi‑Fi | 1.4 MB | 1.8 MB | 8.8 MB | 8.9 MB |
| USB | 1.3 MB | 1.7 MB | 7.6 MB | 7.7 MB |

工作集里约 6 MB 是系统 DLL 的共享页（ntdll/kernel32/ws2_32/crypt32 等），不算本进程的私有占用。空闲期间每 15 s 采样一次，数值没有增长。外部用 `Get-Counter '\Process(copiago-poc)\Working Set - Private'` 测的结果和进程内的 `GetProcessMemoryInfo` 一致。

## 完整重写：功能清单、工作量与最终体积

| 模块 | 对应 Python 代码 | 方案 | 体积增量 | 工作量（人日） |
|---|---|---|---|---|
| 设备连接层：重连、USB→Wi‑Fi 切换、USB RPPairing、拉取/推送、文件 offer/RESOLVE、回声抑制 | device.py 695 行、device_info.py | idevice（原型已验证） | 已含在 2 MB 里 | 6–9 |
| 开发者镜像：下载（多源、续传、固定哈希）、TSS 签名、cryptexd 挂载 | developer_setup.py 589 行 | idevice `cryptexd`+`tss`，下载用 WinHTTP | +0.3–0.6 MB | 3–5 |
| 托盘图标、菜单、设置面板 | ui.py 1024 行 | `Shell_NotifyIconW` + Win32 | +0.1–0.2 MB | 3–5 |
| 自绘弹窗、历史列表、缩略图、动画 | popup.py 1217 行、layered.py | 分层窗口 + Direct2D/DirectWrite/WIC（`windows` crate） | +0.3–0.6 MB | 8–12 |
| Windows 剪贴板：文本、图片 CF_DIB/PNG、文件 CF_HDROP，变化监听 | clipboard.py、files.py | Win32 | +0.05 MB | 2–3 |
| SQLite 历史 | history.py | 系统自带的 `winsqlite3.dll`（本机已确认有）则 0 增量；用 rusqlite bundled 约 +1.0 MB | 0–1.0 MB | 1–2 |
| HEIC 解码 | pillow-heif | 先用 WIC；WIC 需要 HEVC 扩展（本机只装了 HEIF 扩展，没有 HEVC，所以靠不住）。兜底是附带 libheif + libde265 DLL（LGPL，需动态链接） | 0 或 +1.5–2.5 MB | 1–3 |
| DPAPI、设置、i18n、数据目录 ACL | pair_store.py、settings.py、i18n.py | windows-sys + serde_json | +0.05 MB | 2–3 |
| 更新检查 | updater.py | WinHTTP 请求 GitHub API | ≈0 | 0.5–1 |
| 安装器、签名、CI、测试、旧数据迁移 | build.ps1、setup.iss | 见下 | — | 2–4 |
| **合计** | 约 5,100 行 Python | | | **约 29–47 人日（6–9 周，1 人）** |

**最终体积预估**

- 主程序 exe：不带 HEIC 解码库 **3–4.5 MB**；附带 libheif/libde265 时 **5–7 MB**。
- 安装包：Inno Setup（LZMA 压缩后 exe 约剩一半）**3–5 MB**。
- 安装后：exe + 解码 DLL + 卸载程序。现在 Inno 的 `unins000.exe` 一个就 4.4 MB，所以合计 **8–12 MB**；换成 NSIS 或 MSI 可以降到 **5–8 MB**。
- 开发者镜像（约 18 MB）和 Python 版一样，运行时才下载到数据目录，不计入以上数字。

**运行时内存预估**：托盘空闲 4–10 MB，弹窗打开（D2D 设备 + 缩略图）时 10–20 MB。接收大图时，一张 12MP 照片解码成 RGBA 要占约 48 MB 的瞬时内存，所以要用 WIC 按目标尺寸缩放解码、用完立即释放，才能把峰值也压在 30 MB 以内。

## 动画演示（`anim-demo/`）

目的：验证推荐的 UI 技术栈能否在不突破体积和内存目标的前提下，做出流畅的反馈和状态切换动画。

**技术栈**：Win32 窗口（`WS_EX_NOREDIRECTIONBITMAP`）+ Windows.UI.Composition 桌面互操作（`ICompositorDesktopInterop`）。文字和图标用 Direct2D/DirectWrite 画进合成表面，图标字体是 Segoe Fluent Icons。所有动画由系统合成器（DWM）按显示器刷新率驱动，UI 线程繁忙时也不会掉帧。配色、圆角（24）、宽度（392）沿用 Python 版 `popup.py`。

**演示内容**（循环播放，悬停和点击随时可用）：

| 场景 | 动画 |
|---|---|
| 弹窗出现/隐藏 | 从下方弹簧上浮 + 缩放 + 淡入；隐藏时下沉淡出 |
| 状态切换（未连接 → 连接中 → 已连接 → 断开） | 状态胶囊的背景色和圆点颜色渐变；胶囊宽度跟着文字变化并保持右对齐；文字上下交叉淡化；连接中圆点呼吸；状态稳定时圆点弹一下 |
| 状态说明文字 | 旧文字上移淡出，新文字从下方淡入 |
| 进度条 | 宽度连续缓动，完成后停留片刻再淡出 |
| 收到新剪贴板 | 新条目从上方滑入并带弹簧缩放；已有条目通过隐式动画平滑下移；超过 4 条时最旧的淡出 |
| 悬停/按压 | 条目和按钮背景色渐变；按下时弹簧缩小，松开回弹 |
| 推送反馈 | 按钮挤压回弹；"已推送到 iPhone"提示弹出、停留、淡出 |

**实测**（AMD RX 7900 XT，任务管理器"内存"列 = 专用工作集）：

| 状态 | 任务管理器内存 | Private Bytes | 线程 |
|---|---|---|---|
| 弹窗显示（默认，D2D 用 WARP 光栅化） | 2.7–4.6 MB | 5.1–7.9 MB | 10 |
| 弹窗显示（`--hw`，硬件 D3D 设备） | 5.4–33.5 MB | 41–57 MB | 26 |
| 弹窗隐藏（释放全部 GPU 资源） | 2.6–2.8 MB | 3.9–4.3 MB | 5–10 |

- exe 体积 **194,560 字节**，合成、绘制、动画全靠系统组件，几乎不占体积。
- **关键发现**：创建硬件 D3D 设备会加载显卡厂商的用户态驱动，这块就要 15–50 MB 私有内存，线程也从 10 个涨到 26 个。改用 WARP 后，D2D 只在 CPU 上光栅化少量文字表面，合成和全部动画仍然在 DWM 里由 GPU 完成，动画流畅度不受影响，内存却降到原来的约十分之一。所以默认用 WARP，以后要画大图缩略图时，再按需评估硬件设备。
- 弹窗隐藏时会丢弃整个场景（视觉树、表面、D2D/D3D 设备），只保留托盘需要的部分。
- **踩到的坑**：①`LayerVisual.Shadow` 在本机会盖住这一层自身的内容，所以改成"阴影层只放卡片轮廓，内容层放在它上面"的结构。②windows-rs 没法直接构造 `IReference<Vector3>`，Vector3 弹簧动画改成按分量（`Offset.Y`、`Scale.X` 等）分别用标量弹簧，效果相同。

运行：`anim-demo\target\release\anim-demo.exe`。空格键隐藏/显示，Esc 退出；`--hw` 改用硬件 D3D 设备对比内存，`--hide-after N` 让弹窗 N 秒后自动隐藏，`--noshadow` 关闭阴影。

**对重写估算的影响**：之前估计"弹窗显示时 10–20 MB"偏保守，用 WARP 的话，显示时预计 5–10 MB（加上真实的历史缩略图后）。动画相关的额外工作量约 3–5 人日，结论不变。

## 风险与注意事项

1. **idevice 还在 0.x**：作者声明每个小版本都可能破坏 API，必须钉死版本（原型用 `=0.1.68`），升级要单独排期。它是单人维护的项目，但已被 StikDebug、CrossCode 等应用大量使用。
2. **配对记录的 identifier**：Python 记录里没有保存 identifier，只能按主机名重新推导。电脑改名后 Wi‑Fi pair‑verify 会失败。Rust 版迁移时应该把 identifier 写回记录（加上 `host_identifier` 字段）。
3. **原型的简化之处**：Wi‑Fi 隧道期间用 `mem::forget` 保持 RPPairing 控制连接，正式版要持有这条连接，并处理心跳、睡眠唤醒和断线重连。jktcp 用户态 TCP 在大文件（Python 版允许的最大文件）传输下的吞吐和稳定性还没测。
4. **许可证**：idevice（MIT）、ring（ISC 风格）、tokio（MIT）都没问题。不能再沿用 pymobiledevice3（GPL‑3.0）的代码；原型只参考了它的协议说明。HEIC 兜底库 libde265 是 LGPL，必须动态链接。
5. **测试副作用**：第一次 USB 写入测试时，手机剪贴板里是一个 URL，主类型 `public.url`，文本只是次要类型，所以没取到文本，测试后也没能恢复。之后改成写入前先 `allResolved` 全量拉取文本再恢复（恢复时只保留文本，URL 类型会丢失）。

## 复现

```powershell
$env:RUSTUP_HOME='E:\rust\rustup'; $env:CARGO_HOME='E:\rust\cargo'; $env:PATH="E:\rust\cargo\bin;$env:PATH"
cd poc-rust
cargo build --release
.\target\release\copiago-poc.exe info                                   # 第 1 步（需 USB）
.\target\release\copiago-poc.exe run --set "测试文本" --idle 130         # USB：隧道/RSD/读写/通知/空闲内存
.\target\release\copiago-poc.exe run --wifi --set "测试文本" --idle 130  # Wi‑Fi
```

配对记录目录默认是 `E:\iPhoneClipboard\data\<USERNAME>\device-data`，可以用 `--data` 或环境变量 `COPIAGO_DATA` 指定。`--set` 会临时改写手机剪贴板，测试完自动写回原来的文本。
