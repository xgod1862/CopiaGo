# CopiaGo Rust 版任务卡

用法：每张卡都能单独交给一个编码代理（Codex、Claude Code 等）。把"通用约束"和该卡片的全文一起作为任务说明即可。需求细节以 `docs/rust-rewrite/REQUIREMENTS.md` 为准，卡片里的 `F-xxx` 编号指向该文档。

---

## 通用约束（每个任务都必须遵守）

1. **先读资料**：`docs/rust-rewrite/REQUIREMENTS.md`（全文）、`poc-rust/REPORT.md`、卡片"参考"一栏列出的文件。现有功能的行为以 Python 源码 `source/iphone_clip/` 为准，相关差异直接按源码调整实现和验收。
2. **位置**：代码写在 `copiago-rs/` workspace 内。不要修改 `source/`（Python 版）和 `poc-rust/`（原型）。
3. **工具链**：`RUSTUP_HOME=E:\rust\rustup`、`CARGO_HOME=E:\rust\cargo`，`PATH` 加 `E:\rust\cargo\bin`；MSVC Build Tools 已安装。Rust 1.100、edition 2024。
4. **依赖**：`idevice = "=0.1.68"`（`default-features = false`，按需开 feature，TLS 用 `ring`）；Windows API 用 `windows` 0.61 / `windows-sys` 0.59。新依赖必须是 MIT/Apache/BSD/ISC/Zlib 许可证。**禁止**复制或逐行翻译 pymobiledevice3（GPL‑3.0）代码，只能参考它的协议说明。
5. **完成标准**：`cargo fmt --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace` 全部通过；卡片里的验收项逐条满足；在任务合并提交里写明验收结果、实测数据（体积/内存/耗时）、人工复验步骤和真机状态，汇总到阶段 PR。
6. **真机**：标"需要真机"的任务，代理在没有 iPhone 时只完成代码和单测，并在 PR 里列出需要人工验证的步骤。不要伪造真机结果。
7. **Git**：从 `rust/dev` 新建分支 `rust/<任务编号>-<简述>`，本地验收后用 `git merge --no-ff` 集成并推送 `rust/dev`。每阶段开一个从 `rust/dev` 到 `main` 的非草稿 PR，合并后同步 `rust/dev`。卡片提到的 PR 为阶段 PR。提交信息和 PR 使用简体中文（文件名、标识符保留原文）。
8. **安全与隐私**：日志不得包含剪贴板内容、密钥；`unsafe` 块必须有 `// SAFETY:` 注释。
9. **范围**：只做本卡的事。发现别的问题，在 PR 描述的"后续事项"里列出来，不要顺手改。

---

## 依赖关系与并行分组

```
M0:  T-CORE-01 ─┬─ T-CORE-02 ─┬─ T-CORE-03
                │             └─ T-CORE-04
                └─ T-CI-01
M1:  T-DEV-01 → T-DEV-02 → T-DEV-03 → T-DEV-04 → T-DEV-05
     T-DEV-06（Wi-Fi 发现，可与 T-DEV-02~04 并行）
     T-WIN-01（剪贴板，可与 T-DEV-* 并行） → T-APP-01（headless 宿主，需 T-DEV-05 + T-WIN-01）
M2:  T-DEV-07（大文件压测，最先做） → T-DEV-08（文件）
     T-DEV-09（USB 配对/忘记）    T-CORE-05（历史库）    T-IMG-01 → T-IMG-02
M3:  T-DDI-01 → T-DDI-02
M4:  T-UI-01 → T-UI-02 → { T-UI-03, T-UI-04, T-UI-05 } → T-UI-06
     T-WIN-02（托盘/单实例/开机启动，可与 T-UI-* 并行）   T-WIN-03（更新检查）
     T-APP-02（UI 宿主集成，最后做）
M5:  T-PKG-01 → T-PKG-02 → T-QA-01
```

同一时刻可以并行的任务，修改的 crate 互不重叠：`core`、`device`、`win`、`ui`、`ddi` 各自独立。`T-APP-*` 只能串行。

---

## M0 · 工程骨架

### T-CORE-01 创建 workspace 与 crate 骨架
- **目标**：按 REQUIREMENTS 3.1 建立 `copiago-rs/` workspace 和六个 crate（空实现能编译），统一 release profile（`opt-level="z"`、`lto=true`、`codegen-units=1`、`strip=true`、`panic="abort"`）。
- **产出**：`copiago-rs/Cargo.toml`、`crates/*/Cargo.toml` + `src/lib.rs`/`main.rs`、`copiago-rs/README.md`（构建命令、目录说明）、`copiago-rs/METRICS.md`（指标记录模板）、`rust-toolchain.toml`、`.gitignore`。
- **验收**：`cargo build --release` 通过；`copiago-app` 产出 `copiago.exe`，带版本资源和清单（Per‑Monitor V2、`supportedOS`），可用 `embed-resource` 或 `winres`。
- **依赖**：无。

### T-CI-01 CI 与质量门禁
- **目标**：F-PKG-4、N-6。
- **产出**：`.github/workflows/rust.yml`（只在 `copiago-rs/**` 变化时触发）：fmt、clippy、test、release 构建、体积检查（exe > 6 MB 失败）、`cargo-deny`（许可证白名单 + 禁止 GPL）。`copiago-rs/deny.toml`。
- **验收**：PR 上 CI 绿；故意加入 GPL 依赖时 CI 失败（在 PR 描述中说明验证过）。
- **依赖**：T-CORE-01。

### T-CORE-02 数据目录、DPAPI、原子写
- **目标**：F-DATA-1、F-DATA-2、F-DATA-5、F-SEC-1~3。
- **参考**：`source/iphone_clip/settings.py`（`default_data_dir`、`data_dir`、`account_folder_name`、`restrict_to_current_user`）、`pair_store.py`、`device_info.py`、`poc-rust/src/dpapi.rs`。
- **产出**：`copiago-core::{paths, dpapi, atomic}`；`DeviceInfo` 的读写（`device-info.bin`）。
- **验收**：单测覆盖四种数据目录来源（环境变量 / portable / 安装版 / 开发）；DPAPI 往返测试；用 Python 版写出的 `device-info.bin` 样本（测试时现场用 PowerShell + DPAPI 生成）能读出；ACL 设置后用 `icacls` 校验只有当前用户和 SYSTEM。
- **依赖**：T-CORE-01。

### T-CORE-03 设置
- **目标**：4.3 节全部键、F-SET-1。开机启动的注册表读写放在 T-WIN-02，这里只做 JSON。
- **参考**：`source/iphone_clip/settings.py`。
- **验收**：默认值、非法值回落、保留未知键、写入原子性都有单测；能读本机真实的 `E:\iPhoneClipboard\data\<用户>\settings.json`（只读测试，不写）。
- **依赖**：T-CORE-02。

### T-CORE-04 i18n
- **目标**：F-I18N-1~3。
- **参考**：`source/iphone_clip/i18n.py`。
- **产出**：`copiago-core::i18n`，`tr!("…", kind = …)` 风格的宏或函数；英文表用 `build.rs` 从一个数据文件生成，或者手写静态 `phf` 表（体积小优先）。
- **验收**：条目数 ≥ Python 版（测试里写死 Python 版条目数并注明来源）；占位符替换正确；`system` 语言判断有单测（注入语言 ID）。
- **依赖**：T-CORE-01。

---

## M1 · 无界面同步内核

### T-DEV-01 设备层骨架与 USB 隧道
- **目标**：`copiago-device` 的公共接口（REQUIREMENTS 3.3 的 `DeviceEvent`/`DeviceCommand`/`ConnState`），以及 USB 路径：usbmux 列设备 → lockdown 配对记录（usbmuxd 优先，回落本地文件，F-DEV-3）→ CoreDeviceProxy → jktcp → RSD。
- **参考**：`poc-rust/src/main.rs`（`Provider`、`open_usb_tunnel`）。
- **验收**：对 idevice 的调用封装在 crate 内部，公共 API 不出现 idevice 类型；提供 `examples/usb_probe.rs` 打印服务数和剪贴板服务是否存在。**需要真机**。
- **依赖**：T-CORE-02。

### T-DEV-02 剪贴板拉取
- **目标**：F-CLIP-1（先只做 notification + 2 s 轮询）、F-CLIP-2、F-CLIP-3、F-CLIP-9。
- **参考**：`source/iphone_clip/device.py`（`read_clip`、`_watch`、`snapshot_types`、`change_id`、`unsupported_kind`）。
- **产出**：`Clip`（文本/图片字节+UTI/文件 offer）模型放在 `copiago-core`；拉取逻辑基于一个 `PasteboardBackend` trait，方便用假数据单测。
- **验收**：用假 backend 单测覆盖：纯文本、URL（文本为次要类型）、PNG、HEIC+JPEG、webarchive 里的图片、Notes 卡 64 s（模拟超时只跳过本次）、不支持类型；真机上复制文字 1 s 内收到事件。**需要真机**。
- **依赖**：T-DEV-01。

### T-DEV-03 推送与回声抑制
- **目标**：F-CLIP-5（设备侧 SET）、F-CLIP-6。
- **参考**：`device.py::_push`、`is_echo`；`clipboard.py::fingerprint`。
- **验收**：单测覆盖 120 s 回声窗口和 changeCount 记录；真机推送中文 + emoji 文本、PNG 后回读一致。**需要真机**。
- **依赖**：T-DEV-02。

### T-DEV-04 Wi‑Fi 隧道与配对记录写回
- **目标**：F-DEV-4（Wi‑Fi 部分）、F-DEV-7、F-DATA-4。发现部分先用参数传入地址，T-DEV-06 完成后接上。
- **参考**：`poc-rust/src/main.rs`（`open_wifi_tunnel`、`load_rp_record`）。
- **验收**：读取 Python 版记录 → 推导 identifier → pair‑verify 成功 → 写回 `identifier` 字段（DPAPI，原子），写回后的文件 Python 版仍能读（用 Python 版 `pair_store.read_record` 验证并记录命令）；RP 控制连接与隧道同生命周期。**需要真机**。
- **依赖**：T-DEV-01。

### T-DEV-05 连接管理器
- **目标**：F-DEV-4（策略）、F-DEV-5、F-DEV-6、F-DEV-8（仅检测缺服务并发事件，挂载在 M3 实现）、F-DEV-9、F-CLIP-4。
- **产出**：一个状态机，驱动 Wi‑Fi 优先与同设备 USB 回退、普通错误固定间隔重试、等待用户继续的失败状态及设备与网络变化通知（Win32 通知由 `copiago-win` 提供 trait 实现，这里依赖 trait）。
- **验收**：状态机有表驱动单测（假时钟、假设备），覆盖 Wi‑Fi 优先、同设备 USB 回退、失败后等待和用户重试。真机验证接线时保持可用 Wi‑Fi、无线失败后的 USB 回退、回退会话断开后恢复无线，以及手机锁屏解锁、电脑睡眠唤醒。PR 里记录恢复耗时。**需要真机**。
- **依赖**：T-DEV-02、T-DEV-03、T-DEV-04。

### T-DEV-06 Windows 原生 mDNS 发现
- **目标**：F-DEV-11。
- **参考**：`source/iphone_clip/windows_discovery.py`（多网卡问题的背景）、idevice `mdns.rs`（`txt_record_matches`）。
- **产出**：`copiago-device::discovery`，基于 `DnsServiceBrowse`/`DnsServiceResolve`（dnsapi.dll），不引入 `mdns-sd`。
- **验收**：本机能发现手机，耗时 < 1 s；有 VPN 或虚拟网卡时仍能发现（记录测试环境）；与 `mdns-sd` 方案相比 exe 体积减少量写进 PR。**需要真机**。
- **依赖**：T-CORE-01（可与 T-DEV-02~04 并行）。

### T-WIN-01 Windows 剪贴板
- **目标**：F-CLIP-7、F-CLIP-5（监听部分）、F-CLIP-8、写入时打标记避免自我回推。
- **参考**：`source/iphone_clip/clipboard.py`。
- **产出**：`copiago-win::clipboard`：读取文本/图片（DIB/DIBV5/PNG 格式）、写入文本/图片/文件列表（`CF_HDROP`）、`AddClipboardFormatListener` 监听，打开剪贴板按 Python `_open` 重试。
- **验收**：单测（在 CI 的 Windows runner 上实际操作剪贴板）：文本往返、带透明的 PNG 往返、写入文件列表后能读回路径；大图限制。
- **依赖**：T-CORE-01。

### T-APP-01 headless 宿主
- **目标**：REQUIREMENTS 3.2 线程模型的最小实现：`copiago.exe --headless` 启动设备线程 + 主线程消息循环，双向同步文本和图片，日志输出到控制台。
- **验收**：M1 退出条件全部满足；`METRICS.md` 记录空闲 30 min 内存、冷启动到连上的耗时。**需要真机**。
- **依赖**：T-DEV-05、T-DEV-06、T-WIN-01、T-CORE-03。

---

## M2 · 配对、文件、历史

### T-DEV-07 大文件传输压测（先做）
- **目标**：验证 F-FILE-5 的可行性，确定 RESOLVE / thresholdData 传 100 MB、500 MB 文件的速度、内存峰值和稳定性（USB 与 Wi‑Fi）。
- **产出**：`copiago-device/examples/file_bench.rs` + 结果写进 `METRICS.md`；结论：能否流式写盘、需不需要给 idevice 打补丁、上限是否要调整。
- **验收**：数据真实可复现。**需要真机**（手机"文件"App 中准备测试文件）。
- **依赖**：T-DEV-02。

### T-DEV-08 文件 offer 与接收
- **目标**：F-FILE-1~5。
- **参考**：`device.py`（`describe_file`、`_fetch`、`FileOffer`）、`files.py`。
- **验收**：单测覆盖文件名解析、安全文件名、24 h 清理；真机：小文件自动接收、大文件询问、`OfferExpired`、512 MB 上限。**需要真机**。
- **依赖**：T-DEV-07、T-WIN-01。

### T-DEV-09 USB 配对与忘记设备
- **目标**：F-DEV-1、F-DEV-2、F-DEV-10。
- **参考**：`device.py::pair_over_usb`、`forget_paired_devices`；idevice `RemotePairingLockdownService`、`RemotePairingClient::pair`。
- **验收**：在测试用数据目录（`ICB_DATA_DIR` 指向临时目录）中从零完成配对并连上 Wi‑Fi；多设备提示；手机拒绝信任时的错误文案。**需要真机**。
- **依赖**：T-DEV-05。

### T-CORE-05 历史库
- **目标**：F-HIST-1~4、F-SEC-2。
- **参考**：`source/iphone_clip/history.py`。
- **产出**：`copiago-core::history`。先实现 `winsqlite3.dll` 动态加载版本；如果不可行，在 PR 中说明原因并改用 `rusqlite`（bundled），同时报告体积增量。
- **验收**：单测覆盖上限、去重、两个列表、`source` 列迁移；与 Python 版互通（测试里用 Python 写一个库，Rust 读；Rust 写，Python 读，命令写进 PR）。
- **依赖**：T-CORE-02。

### T-IMG-01 WIC 图片管线
- **目标**：F-IMG-1、F-IMG-3。
- **产出**：`copiago-win::image`：解码、按尺寸缩放解码、PNG 编码、DIB ↔ PNG。
- **验收**：48MP JPEG 生成 96 px 缩略图时峰值内存 < 30 MB（测试里测）；透明 PNG 保真。
- **依赖**：T-CORE-01。

### T-IMG-02 HEIC 方案评估与实现
- **目标**：F-IMG-2。比较方案 (a) libheif DLL、(b) 请求替代格式：在真机上统计常见来源（相册照片、截图、微信图片、Safari 图片）实际提供哪些 UTI。
- **产出**：评估结论写进 `METRICS.md`，并实现选定方案。
- **验收**：没有 HEVC 扩展的电脑上，相册照片能正常粘贴。**需要真机**。
- **依赖**：T-IMG-01、T-DEV-02。

---

## M3 · 开发者镜像

### T-DDI-01 镜像下载
- **目标**：F-DDI-1、F-DDI-2。
- **参考**：`source/iphone_clip/developer_setup.py`（`DDI_REPO`、`DDI_REF`、`PINNED_FILES`、镜像源列表、`_fetch_file`、断点续传逻辑）。
- **产出**：`copiago-ddi::download`（WinHTTP，WinHTTP 封装放在 `copiago-win::http`，供更新检查复用）。
- **验收**：单测（本地 HTTP 测试服务器）覆盖续传、停滞换源、哈希不符、取消；真实下载一次并校验。
- **依赖**：T-CORE-02。

### T-DDI-02 签名与挂载
- **目标**：F-DDI-3~5，并接入 T-DEV-05 的"缺服务"分支。
- **参考**：`developer_setup.py::_install_image`、`prepare_developer_services`；idevice `cryptexd`、`mobile_image_mounter`、`tss`。
- **验收**：手机重启后（镜像会被卸载）首次连接自动完成挂载并连上；`developer-setup.log` 格式与 Python 版一致；未开开发者模式时提示正确。**需要真机**。
- **依赖**：T-DDI-01、T-DEV-05。

---

## M4 · 托盘与弹窗 UI

### T-UI-01 Composition 宿主与绘制基础
- **目标**：把 `poc-rust/anim-demo` 的基础设施整理成 `copiago-ui` 的模块：窗口（`WS_EX_NOREDIRECTIONBITMAP`、透明区域点击穿透）、DispatcherQueue、Compositor、WARP D2D 设备、文字/图标表面、圆角形状、阴影（使用"阴影层 + 内容层"结构）、DPI 变化处理、隐藏时释放 GPU 资源（F-UI-7）。
- **参考**：`poc-rust/anim-demo/src/*.rs`、REPORT 的"踩到的坑"。
- **验收**：示例程序显示一个空卡片；150%/200% DPI 正确；隐藏后内存回到 ≤ 4 MB。
- **依赖**：T-CORE-01。

### T-UI-02 动画工具库与组件
- **目标**：F-ANI 的公共部分：`animate_scalar/color/v2/v3`、按分量的弹簧、隐式 Offset、减少动画开关（F-ANI-13）；组件：按钮、开关（F-ANI-11）、状态胶囊（F-ANI-3/4）、进度条（F-ANI-5）、提示条（F-ANI-9）、列表项（F-ANI-6/7/8/12）、滚动容器（F-UI-5）。
- **验收**：一个组件展示页（`examples/gallery.rs`）能演示全部组件和动画；组件有简单的布局/命中测试单测。
- **依赖**：T-UI-01。

### T-UI-03 历史页
- **目标**：F-UI-1、F-UI-2、F-UI-4（与历史相关的部分）。
- **参考**：`source/iphone_clip/popup.py`（布局常量、`short_time`、文本截断与换行规则 `fit`/`wrap`）。
- **验收**：用假数据驱动；与 Python 版截图并排对比（PR 里附截图）。
- **依赖**：T-UI-02、T-CORE-05。

### T-UI-04 连接与设置页
- **目标**：F-UI-3、F-UI-4（配对/镜像/错误）、F-ANI-10。
- **参考**：`popup.py` 设置页部分、`ui.py::settings_state/toggle_setting/toggle_type`。
- **验收**：全部设置项可切换并持久化；忘记设备二次确认；语言即时切换。
- **依赖**：T-UI-02、T-CORE-03、T-CORE-04。

### T-UI-05 键盘与无障碍
- **目标**：F-UI-6、F-ANI-13，以及基础 UI Automation（至少让讲述人能读出按钮和列表项）。
- **依赖**：T-UI-02。

### T-UI-06 弹窗定位
- **目标**：F-UI-1 的定位部分：任务栏四个方向、多显示器、托盘图标位置（`Shell_NotifyIconGetRect`），出现方向随任务栏方向调整（F-ANI-1）。
- **依赖**：T-UI-03。

### T-WIN-02 托盘、单实例、开机启动、通知
- **目标**：F-TRAY-1~6、F-SET-2。
- **参考**：`source/iphone_clip/ui.py`（菜单、`promote_tray`、`startup_command`、`RUN_KEY`/`RUN_VALUE`）、`assets/`。
- **验收**：Explorer 重启后图标恢复；第二个实例唤起第一个；开机启动与 Python 版写同一注册表值。
- **依赖**：T-CORE-03。

### T-WIN-03 更新检查
- **目标**：F-UPD-1、F-UPD-2。
- **参考**：`source/iphone_clip/updater.py`。
- **依赖**：T-DDI-01（复用 WinHTTP 封装）。

### T-APP-02 UI 宿主集成
- **目标**：把设备事件接到 UI，替换 headless 宿主为正式托盘应用（headless 保留为 `--headless` 调试模式）。
- **验收**：第 8 节验收清单中除打包外的所有项；`METRICS.md` 记录托盘空闲、弹窗打开的内存。**需要真机**。
- **依赖**：M4 其余全部任务、M1–M3 全部任务。

---

## M5 · 打包与发布

### T-PKG-01 安装器与便携版
- **目标**：F-PKG-1、F-PKG-2、F-PKG-5。
- **参考**：`source/setup.iss`、`source/build.ps1`、`source/ChineseSimplified.isl`。
- **产出**：`copiago-rs/installer/setup.iss`、`scripts/build.ps1`（构建 + 打包 + 体积报告）。
- **验收**：安装包 ≤ 6 MB、安装后 ≤ 12 MB。
- **依赖**：T-APP-02。

### T-PKG-02 从 Python 版覆盖升级
- **目标**：F-PKG-3。
- **验收**：在装有 Python 1.2.0（已配对）的机器上覆盖安装，不重新配对就能连上，历史和设置保留。**需要真机**。
- **依赖**：T-PKG-01。

### T-QA-01 稳定性与指标回归
- **目标**：N-1、N-3、N-4，第 8 节清单全部过一遍。
- **产出**：`METRICS.md` 完整表格；问题清单。**需要真机**。
- **依赖**：T-PKG-02。
