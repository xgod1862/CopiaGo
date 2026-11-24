# CopiaGo Rust 实测指标

每个阶段记录提交、Windows 版本、设备与连接条件、样本数和实测值。尚未验证的项目填写「未测」。目标见 [需求第 1 节与 F-PKG-4](../docs/rust-rewrite/REQUIREMENTS.md)。

| 阶段 / 提交 | EXE ≤ 6 MB | 安装包 ≤ 6 MB | 安装后 ≤ 12 MB | 托盘空闲 ≤ 10 MB | 弹窗打开 ≤ 20 MB | Wi‑Fi 冷启动到连上 ≤ 3 s |
| --- | --- | --- | --- | --- | --- | --- |
| M0 · T-CORE-01 骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M0 · 工程骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M1 · 集成候选 `07d2e93` | 1,956,352 字节 | 未测 | 未测 | 未测 | 未测 | 1,371.15–2,249.55 ms，中位数 1,922.97 ms（5 次） |
| M2 · T-IMG-02 | 1,956,864 字节，比 `9be9333` 增加 1,024 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| 阶段 / 提交 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |

## 测量方法

- 体积使用 `Get-Item` 的 `Length` 字段。安装后体积汇总安装目录内文件，并排除运行时下载的开发者镜像。
- 内存使用 `Get-Counter '\Process(copiago)\Working Set - Private'`，同时记录 Private Bytes 和线程数。24 小时稳定性测试每小时采样一次。
- Wi‑Fi 冷启动从进程启动计时到首次连接成功。手机保持解锁，与电脑位于同一局域网。

## 阶段记录

| 阶段 / 提交 | 运行环境 | 测量命令与样本 | 备注 |
| --- | --- | --- | --- |
| M0 · T-CORE-01 | Windows 11 Pro build 26300、x64、Rust 1.100.0 | `cargo build --release --locked`，全新构建目录 1 次。直接启动 EXE 10 次，退出后用 `GetProcessMemoryInfo` 读峰值 | Release 构建 1.61 s。启动至退出耗时 12.60–67.85 ms，中位数 13.11 ms；峰值工作集 4,517,888–4,526,080 字节，峰值私有提交量 753,664–765,952 字节。 |
| M0 · 工程骨架 | Windows 11 Pro build 26300、x64、Rust 1.100.0 | `cargo build --workspace --release --locked --target-dir target/m0-stage-release`，新目标目录 1 次 | 工作区构建 3.97 s。文件及产品版本 2.0.0-beta。 |
| M1 · T-APP-01 基线 `3f1ef14` | Windows 11 Pro 26H2 build 26300.9974、x64、Rust 1.100.0，同一局域网内的 iPhone 15 Pro Max（iOS 27.2.1） | Release 运行 `copiago.exe --headless`。首次观察到 Wi-Fi 连接后，按 15 s 调度采样 121 次 | 专用工作集 2,097,152–2,179,072 字节，峰值低于 M1 的 5 MB 目标。Private Bytes 2,629,632–2,797,568 字节，总工作集 12,529,664–12,701,696 字节，2–8 个线程、185–193 个句柄。 |
| M1 · 集成候选 `07d2e93` | 同上 | `cargo build --workspace --release --locked`。5 次独立启动、1 次应用内存采样、2 次模拟事件触发真实 Wi-Fi 重连 | 首次启动连接后第 16.409 s 的 headless 专用工作集为 2,183,168 字节，Private Bytes 为 2,887,680 字节，总工作集 12,591,104 字节，8 个线程、193 个句柄。模拟唤醒到观察到恢复为 2,158.95 ms，模拟网络变化为 2,442.79 ms（含网络事件合并延迟）。 |
| 阶段 / 提交 | 未测 | 未测 | |

### M1 测量范围

- 两份构建先后使用同一个受保护 DACL 的临时 `ICB_DATA_DIR`，其中保留 Python 版配对记录的副本，设置为 `pull=false`、`push=false`。手机与电脑保持同一局域网。
- 基线专用工作集通过 `Get-Counter '\Process(copiago*)\Working Set - Private'` 采集，并按 `ID Process` 匹配 PID 27004。末样本距连接就绪观测 1,801.076 s，采样 UTC 时间为 2026-11-24 09:50:20.521 至 10:20:19.878，实测采样间隔 14.399–15.146 s。期间只有一条 Wi-Fi 会话，控制台记录两次自然发生的手机文本事件，均暂存，最后约 24 min 无剪贴板事件。采样区间进程 CPU 时间为 1.484375 s，折合单个逻辑处理器 0.0825%，包含元数据轮询。
- 集成候选用 Windows 进程创建时间至外部观察到 `event=connected transport=wifi` 计时，5 次分别为 1,371.15、2,249.55、1,882.08、1,922.97、2,001.93 ms。程序内部的对应区间为 1,265–2,203 ms。向隐藏窗口分别注入 `WM_POWERBROADCAST(PBT_APMRESUMEAUTOMATIC)` 和模拟 IP 通知对应的窗口消息，应用内从收到信号到 Wi-Fi 重连分别为 2,136 ms 和 2,127 ms。
- 待真机复验的项目包括 USB 和 Wi-Fi 双向文本与 PNG、接线保留 Wi-Fi、无线失效回退同一手机的 USB、断开 USB 后恢复无线，以及实际锁屏、系统睡眠和网络切换后的恢复。30 分钟采样使用 T-APP-01 基线，集成候选的持续内存采样待测。

### M2 · T-IMG-02 HEIC 方案评估

采用方案 (b)。沿用内联 PNG、JPEG 优先级。选中 HEIC 且同一个剪贴板条目声明替代格式时，额外请求一次 `allResolved`，仅采纳变更标识（nonce 和 changeCount）与条目位置均匹配的 PNG 或 JPEG。额外请求最多等待 30 秒，也可能取回同次复制的其他表示。替代格式取用失败时保留原 HEIC，交给 WIC。方案依据是工程评估，真机可用性见下文。[拉取实现](crates/copiago-device/src/pasteboard/content.rs)

| 方案 | 在无系统 HEVC 解码器时的覆盖 | 体积和许可证 |
| --- | --- | --- |
| (a) 动态链接 libheif 和 libde265 | 本机既有 Pillow-HEIF 1.8.1 用 libheif 1.23.6 和 libde265 1.1.3 解出合成 HEIC。CopiaGo 独立 DLL 集成未测。 | 现有 Python wheel 中两个 DLL 合计 3,136,000 字节（2,176,000 和 960,000 字节）。这是参考构建大小，仅含解码器的发行构建体积未测。两库采用 LGPL，项目要求仅动态链接。发布依赖须覆盖对应源码与许可证，并允许替换兼容 DLL。<https://raw.githubusercontent.com/strukturag/libheif/v1.23.6/README.md> <https://raw.githubusercontent.com/strukturag/libheif/v1.23.6/COPYING> <https://raw.githubusercontent.com/strukturag/libde265/v1.1.3/COPYING> |
| (b) 优先取手机提供的替代格式 | 选到 PNG 或 JPEG 时用 WIC 解码。只有 HEIC 数据时仍依赖 WIC 的系统编解码器。 | Release EXE 为 1,956,864 字节，比同工具链和配置编译的基线多 1,024 字节。新增 DLL 体积为 0 字节。[WIC 解码实现](crates/copiago-win/src/image.rs) <https://learn.microsoft.com/en-us/windows/win32/wic/native-wic-codecs> <https://learn.microsoft.com/en-us/windows/win32/wic/heif-codec> |

在 Windows 11 Pro 26H2 build 26300.9974、x64、Rust 1.100.0 下，以 `Get-AppxPackage` 检查当前测试账户，列出 `Microsoft.HEIFImageExtension 1.2.65.0`，无 HEVC 扩展。使用 Pillow 13.0.0 和既有 Pillow-HEIF 生成 160 × 96 的红绿合成图，JPEG 925 字节、HEIC 660 字节。独立 Release 测试进程运行 5 次，计时仅含剪贴板数据准备。[合成样本](crates/copiago-win/src/image/fixtures/)

| 测项 | 实测 |
| --- | --- |
| 合成 JPEG 走 Windows 图片写入的准备路径 | 5/5 生成有效 PNG 和 DIBV5，尺寸及像素验证通过。耗时 7.64–8.28 ms，中位数 7.91 ms。 |
| 合成 HEIC 走相同路径 | 5/5 返回 `0xc00d5212`（`MF_E_TOPO_CODEC_NOT_FOUND`），耗时 63.06–67.94 ms，中位数 66.34 ms。解码失败发生在打开 Windows 剪贴板之前。<https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsourcereader-setcurrentmediatype> |
| Release 测试进程峰值，含上述两条路径 | 工作集 18,477,056–19,333,120 字节，提交量 3,842,048–3,883,008 字节。指标含测试进程开销。<https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters> |
| 真机拉取到粘贴的耗时、内存及安装体积 | 未测。 |

协议模拟覆盖了内联 JPEG 优先、PNG/JPEG 承诺数据、只有 HEIC、另一个条目声明替代格式、变更标识不同、替代未兑现和额外请求超时。模拟使用构造的 UTI。相册照片、截图、微信图片、Safari 图片各有 0 个真机样本，四类来源的实际 UTI 及无 HEVC 电脑端到端粘贴均待验。如相册只提供 HEIC，缺少系统解码器时需要本地解码方案。

#### 复验

在 `copiago-rs/` 运行以下命令，可复测本机编解码路径。检测到系统 HEIC 解码器时，同一个测试也会验证合成 HEIC 输出像素。

```powershell
Get-AppxPackage | Where-Object Name -Match 'HEIF|HEVC' | Select-Object Name, Version
cargo test --release --locked -p copiago-win clipboard::tests::wic_prepares_jpeg_and_uses_the_optional_heic_codec -- --exact --nocapture
```

真机验证使用独立测试设备、已配对的受保护临时 `ICB_DATA_DIR` 和隔离 Windows 剪贴板。每次从相册复制一张已知 HEIC 照片、复制一张截图、从微信复制图片、从 Safari 复制网页图片后，运行探针并按来源记录各条目的有序 UTI。探针通过已认证的 Wi-Fi 连接执行 `PULL allPromised`，输出 UTI 和计时。期望相册条目存在可兑现的 `public.png` 或 `public.jpeg`，各来源实际的 UTI 以探针为准。

```powershell
cargo run --release --locked -p copiago-device --example wifi_probe -- --pasteboard-types
```

在测试数据目录开启 `pull` 和图片类型，关闭 `push`。运行以下命令，看到 `event=connected transport=wifi` 后重新复制样本。成功时应见 `event=pull_complete kind=image`，并可把当次图片粘贴到测试应用中。记录相册原图格式、Windows HEVC 扩展状态、每种来源的通过数和总样本数。

```powershell
cargo run --release --locked -p copiago-app -- --headless
```
