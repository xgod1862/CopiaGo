# CopiaGo Rust 实测指标

每个阶段记录提交、Windows 版本、设备与连接条件、样本数和实测值。尚未验证的项目填写「未测」。目标见 [需求第 1 节与 F-PKG-4](../docs/rust-rewrite/REQUIREMENTS.md)。

| 阶段 / 提交 | EXE ≤ 6 MB | 安装包 ≤ 6 MB | 安装后 ≤ 12 MB | 托盘空闲 ≤ 10 MB | 弹窗打开 ≤ 20 MB | Wi‑Fi 冷启动到连上 ≤ 3 s |
| --- | --- | --- | --- | --- | --- | --- |
| M0 · T-CORE-01 骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M0 · 工程骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M1 · 集成候选 `07d2e93` | 1,956,352 字节 | 未测 | 未测 | 未测 | 未测 | 1,371.15–2,249.55 ms，中位数 1,922.97 ms（5 次） |
| M2 · T-IMG-02 初版 `d039a6a` | 1,956,864 字节，比 `9be9333` 增加 1,024 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M2 · T-IMG-02 定向修订 | 2,465,280 字节，比 `a25cf2f` 的 2,453,504 字节增加 11,776 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
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

## M2 · T-DEV-07 大文件压测

### 流式接收与上限

- Python `device.py::_fetch` 先用 `allPromised` 校验 `(nonce, changeCount)`，再用 `PULL thresholdData((size or 512 MiB) + 1)`。Rust 沿用该路径，描述阶段仍用 256 KiB 阈值。探针另测 `RESOLVE`，用于比较同一文件在两种命令下的回复。[Python 文件接收](../source/iphone_clip/device.py)
- `idevice 0.1.68` 的 `RemoteXpcClient::recv` 聚合整个回复，解码 XPC Data 时分配数据，再复制进 plist。类型化 `resolve` 还会把 plist 字节复制成 `Vec<u8>`。直接使用上述接收 API 需要上游新增流式接口。[RemoteXPC 接收](https://raw.githubusercontent.com/jkcoxson/idevice/d32c8189c51c2789496b0768039419c3705498c3/idevice/src/xpc/mod.rs) · [XPC 格式](https://raw.githubusercontent.com/jkcoxson/idevice/d32c8189c51c2789496b0768039419c3705498c3/idevice/src/xpc/format.rs) · [RESOLVE](https://raw.githubusercontent.com/jkcoxson/idevice/d32c8189c51c2789496b0768039419c3705498c3/idevice/src/services/core_device/pasteboard_service.rs)
- CopiaGo 的文件接收器沿用 `idevice` 的隧道、RemoteXPC 握手和发送，按 HTTP/2 DATA 帧读取并写出目标 UTI，保持连接和回复流两级流控。当前实现可在锁定的 `idevice` 版本上流式落盘，无需打补丁。仅处理 `PULL_REPLY` 或 `DATA` 中的内联 XPC Data，独立文件流描述符单独报错。[流式接收实现](crates/copiago-device/src/pasteboard/file_transfer.rs) · [RFC 9113 流控](https://www.rfc-editor.org/rfc/rfc9113.html#section-6.9)
- 保持文件上限 512 MiB。元数据回复上限为 2 MiB，包含文件的整个 XPC 消息体上限为 514 MiB，有效文件字节另按 512 MiB 限制。模拟覆盖分帧、连接与回复流窗口耗尽后续发、目标端背压、错误长度、超限、新的 change ID、截断和超时。USB 和 Wi‑Fi 真机吞吐与稳定性仍待验证，现有结果仅支持保留上限进入 T-DEV-08。[测试](crates/copiago-device/src/pasteboard/file_transfer/simulation/tests.rs)

### 本地协议模拟

Windows 11 Pro 26H2 build 26300.9974 x64、Rust 1.100.0、输出盘 NTFS。Release 示例 `file_bench.exe` 为 1,903,616 字节，工作区 Release 的 `copiago.exe` 为 1,956,352 字节。每个组合使用 3 个独立进程，从模拟对端经内存中的双向流接收并写入受保护临时文件。全部 12 次落盘字节数和 SHA-256 一致，结束后无输出文件残留。下表给出范围，吞吐括号内为中位数。

| 方法 | 文件字节 | 传输 ms | 吞吐 MiB/s | 刷盘 ms | 峰值工作集字节 | 峰值私有提交字节 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| thresholdData · 100 MiB | 104,857,600 | 99.115–100.008 | 999.92–1,008.93（1,008.19） | 112.171–149.757 | 8,118,272–8,712,192 | 2,076,672–2,129,920 |
| thresholdData · 500 MiB | 524,288,000 | 495.513–505.250 | 989.61–1,009.06（1,003.21） | 189.711–562.880 | 8,122,368–8,151,040 | 2,080,768–2,113,536 |
| RESOLVE · 100 MiB | 104,857,600 | 96.730–99.014 | 1,009.96–1,033.81（1,016.37） | 39.772–49.174 | 8,097,792–8,110,080 | 2,064,384–2,097,152 |
| RESOLVE · 500 MiB | 524,288,000 | 489.301–502.702 | 994.63–1,021.87（1,020.67） | 212.987–569.136 | 8,126,464–8,138,752 | 2,105,344–2,154,496 |

`transfer_ms` 包括请求、解帧和缓冲写盘，模拟时也包含对端生成、散列及流控，真机时包含传输前复查。最后一次 flush 与 `sync_all` 计在 `disk_sync_ms`，读回散列计在 `verify_ms`。内存是完成同步、读回校验前的进程高水位，分别使用 `PeakWorkingSetSize` 和 `PeakPagefileUsage`。工作集包含共享页，模拟峰值适用于探针进程。[Windows 计数器定义](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex)

### 真机状态

2026-11-24 真机操作限于只读服务探针。Wi‑Fi 两次返回 `could not connect to the RemotePairing endpoint`（306、467 ms），USB 返回 `no USB device found`（2 ms）。受控合成样本在手机上的可用性未验证。USB 与 Wi‑Fi 的 100/500 MiB 文件吞吐、内存和重复成功率均未测。

### 复现

从 `copiago-rs` 运行以下命令。模拟只需临时目录，真机复验时使用同一 Windows 账户，将一台设备的 `remote_*.plist` 副本放入该目录的 `device-data`，有本地 lockdown 配对记录时一并复制。同一目录用 `icacls` 确认只有当前账户和 SYSTEM。探针从配对缓存读取，收到的样本使用另建的受保护目录和关闭即删除的文件。

```powershell
$ErrorActionPreference = 'Stop'
$env:RUSTUP_HOME = 'E:\rust\rustup'
$env:CARGO_HOME = 'E:\rust\cargo'
$env:PATH = "E:\rust\cargo\bin;$env:PATH"
$env:ICB_DATA_DIR = Join-Path ([IO.Path]::GetTempPath()) ('CopiaGo-file-bench-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $env:ICB_DATA_DIR
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
icacls $env:ICB_DATA_DIR /inheritance:r /grant:r "*${sid}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F'
if ($LASTEXITCODE) { throw 'Temporary ACL setup failed' }
$null = New-Item -ItemType Directory -Path (Join-Path $env:ICB_DATA_DIR 'device-data')
cargo build --release --locked -p copiago-device --example file_bench --example wifi_probe --example usb_probe
if ($LASTEXITCODE) { throw 'Build failed' }
$examples = '.\target\release\examples'
$bench = Join-Path $examples 'file_bench.exe'
foreach ($method in 'threshold', 'resolve') {
    foreach ($size in 100, 500) {
        foreach ($run in 1..3) {
            & $bench simulate $method $size
            if ($LASTEXITCODE) { throw 'Simulation failed' }
        }
    }
}
```

真机复验时生成 100 MiB、500 MiB 的合成文件，保留本地原件用于验证。

```powershell
$samples = Join-Path $env:ICB_DATA_DIR 'samples'
$null = New-Item -ItemType Directory -Path $samples
$buffer = [byte[]]::new(1MB)
foreach ($size in 100, 500) {
    $random = [Random]::new($size)
    $sample = Join-Path $samples "copiago-bench-${size}MiB.bin"
    $file = [IO.File]::Open($sample, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
    try {
        foreach ($block in 1..$size) {
            $random.NextBytes($buffer)
            $file.Write($buffer, 0, $buffer.Length)
        }
    } finally {
        $file.Dispose()
    }
}
```

- 在真机测试前将这两份合成文件置于测试手机的「文件」App。优先建立 Wi‑Fi 测试条件，运行 `& (Join-Path $examples 'wifi_probe.exe')`，期望 `pair_verify=true`、`pasteboard_advertised=true`。USB 复验连接同一已配对设备，运行 `& (Join-Path $examples 'usb_probe.exe')`，期望 `pasteboard_advertised=true`。
- 在「文件」App 复制相应样本后运行 `& $bench wifi threshold (Join-Path $samples 'copiago-bench-100MiB.bin')`。同样覆盖 `resolve`、`500MiB`、`usb` 的组合，每个组合独立执行 3 次。期望 `sha256_match=true`、`bytes=104857600` 或 `bytes=524288000`。按组合记录成功次数、传输/刷盘耗时、吞吐与内存峰值。若返回样本不符、数据未内联、超时或断线，记录失败和错误类别。
- T-DEV-08 可在串行剪贴板请求中调用 crate 内 `pasteboard::file_transfer::receive_file(adapter, port, &offer, Method::Threshold, &mut writer)`。目标须为尚未发布的临时文件。成功值为完整回复中的实际文件字节数。调用方在成功后同步并发布，失败或取消时丢弃临时文件。

## M2 · T-IMG-02 HEIC 方案评估

采用方案 (b)。沿用内联 PNG、JPEG 优先级。选中 HEIC 且该条目声明替代格式时，按原始条目索引和 UTI 执行 `RESOLVE`，优先 PNG，其次 JPEG。每次请求前和采纳数据前以有界的 `allPromised` 复核原剪贴板变更标识（nonce 和 changeCount）。可选阶段总限时 6 秒，单次 `RESOLVE` 限时 2 秒。超长、签名明显不符或不可取的候选会跳过，最终可回退至原 HEIC 和 WIC。[拉取实现](crates/copiago-device/src/pasteboard/content.rs)

复用 T-DEV-07 分帧接收器，有界元数据回复上限 2 MiB，`RESOLVE` 外层 XPC 正文上限 66 MiB，所选图片数据上限 64 MiB。图片长度校验通过后才按所需长度分配缓冲区。[分帧实现](crates/copiago-device/src/pasteboard/file_transfer.rs)

定向修订与基线 `a25cf2f` 均在下述 Windows/Rust 环境使用 `cargo build --workspace --release --locked`，按 `Get-Item` 的 `Length` 比较 EXE。

| 方案 | 在无系统 HEVC 解码器时的覆盖 | 体积和许可证 |
| --- | --- | --- |
| (a) 动态链接 libheif 和 libde265 | 本机既有 Pillow-HEIF 1.8.1 用 libheif 1.23.6 和 libde265 1.1.3 解出合成 HEIC。CopiaGo 独立 DLL 集成未测。 | 现有 Python wheel 中两个 DLL 合计 3,136,000 字节（2,176,000 和 960,000 字节）。这是参考构建大小，仅含解码器的发行构建体积未测。两库采用 LGPL，项目要求仅动态链接。发布依赖须覆盖对应源码与许可证，并允许替换兼容 DLL。<https://raw.githubusercontent.com/strukturag/libheif/v1.23.6/README.md> <https://raw.githubusercontent.com/strukturag/libheif/v1.23.6/COPYING> <https://raw.githubusercontent.com/strukturag/libde265/v1.1.3/COPYING> |
| (b) 优先取手机提供的替代格式 | 选到 PNG 或 JPEG 时用 WIC 解码。只有 HEIC 数据时仍依赖 WIC 的系统编解码器。 | 新增 DLL 体积为 0 字节。初版与定向修订的 Release EXE 体积见上表。[WIC 解码实现](crates/copiago-win/src/image.rs) <https://learn.microsoft.com/en-us/windows/win32/wic/native-wic-codecs> <https://learn.microsoft.com/en-us/windows/win32/wic/heif-codec> |

Windows 侧数据来自初版 `d039a6a`。环境为 Windows 11 Pro 26H2 build 26300.9974、x64、Rust 1.100.0。当前测试账户通过 `Get-AppxPackage` 查到 `Microsoft.HEIFImageExtension 1.2.65.0`，无 HEVC 扩展。使用 Pillow 13.0.0 和既有 Pillow-HEIF 生成 160 × 96 的红绿合成图，JPEG 925 字节、HEIC 660 字节。独立 Release 测试进程运行 5 次，计时仅含剪贴板数据准备。[合成样本](crates/copiago-win/src/image/fixtures/)

| 测项 | 实测 |
| --- | --- |
| 合成 JPEG 走 Windows 图片写入的准备路径 | 5/5 生成有效 PNG 和 DIBV5，尺寸及像素验证通过。耗时 7.64–8.28 ms，中位数 7.91 ms。 |
| 合成 HEIC 走相同路径 | 5/5 返回 `0xc00d5212`（`MF_E_TOPO_CODEC_NOT_FOUND`），耗时 63.06–67.94 ms，中位数 66.34 ms。解码失败发生在打开 Windows 剪贴板之前。<https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsourcereader-setcurrentmediatype> |
| Release 测试进程峰值，含上述两条路径 | 工作集 18,477,056–19,333,120 字节，提交量 3,842,048–3,883,008 字节。指标含测试进程开销。<https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters> |
| 真机拉取到粘贴的耗时、内存及安装体积 | 未测。 |

定向修订的协议模拟覆盖了原始条目索引、内联 JPEG 优先、PNG/JPEG 回退、变更标识前后复核、长度上限、错误签名和总限时。定向网络耗时、内存峰值未测。相册照片、截图、微信图片、Safari 图片各有 0 个真机样本，四类来源的实际 UTI 及无 HEVC 电脑端到端粘贴均待验。如相册只提供 HEIC，缺少系统解码器时需要本地解码方案。

#### 复验

在 `copiago-rs/` 运行以下命令，可复测本机编解码路径。检测到系统 HEIC 解码器时，同一个测试也会验证合成 HEIC 输出像素。

```powershell
Get-AppxPackage | Where-Object Name -Match 'HEIF|HEVC' | Select-Object Name, Version
cargo test --release --locked -p copiago-win clipboard::tests::wic_prepares_jpeg_and_uses_the_optional_heic_codec -- --exact --nocapture
cargo test --locked -p copiago-device --lib targeted_image_checks_payload_and_wrapper_limits_before_reading_data
```

真机验证使用独立测试设备、已配对的受保护临时 `ICB_DATA_DIR` 和隔离 Windows 剪贴板。每次从相册复制一张已知 HEIC 照片、复制一张截图、从微信复制图片、从 Safari 复制网页图片后，运行探针并按来源记录各条目的有序 UTI。探针通过已认证的 Wi-Fi 连接执行 `PULL allPromised`，输出 UTI 和计时。期望相册条目存在可兑现的 `public.png` 或 `public.jpeg`，各来源实际的 UTI 以探针为准。

```powershell
cargo run --release --locked -p copiago-device --example wifi_probe -- --pasteboard-types
```

在测试数据目录开启 `pull` 和图片类型，关闭 `push`。运行以下命令，看到 `event=connected transport=wifi` 后重新复制样本。成功时应见 `event=pull_complete kind=image`，并可把当次图片粘贴到测试应用中。记录相册原图格式、Windows HEVC 扩展状态、每种来源的通过数和总样本数。

```powershell
cargo run --release --locked -p copiago-app -- --headless
```

## M2 · T-DEV-08 文件接收

- Windows x64 build 26300、Rust 1.100.0。Release `copiago.exe` 为 2,030,592 字节。工作区 144 项测试通过，5 项默认忽略。
- 合成对端通过 `thresholdData` 发送恰好 512 MiB，使用正式临时文件、长度校验、刷盘和发布流程。3 次均为 536,870,912 字节，读回 SHA-256 一致。接收至发布耗时 938–991 ms，含读回校验为 1,472–1,540 ms。测试进程峰值工作集 8,171,520–11,698,176 字节，约每 15 ms 采样的 Private Bytes 峰值 1,683,456–1,724,416 字节。
- 非交互 headless 启动清除了 25 h 前的合成接收目录，保留近期目录，处理窗口关闭消息后正常退出。阶段目录和发布目录的 ACL 单测验证了当前用户与 SYSTEM。合成测试使用受保护的临时目录。
- 真机 USB/Wi-Fi 文件接收、资源管理器实际粘贴、真机耗时和内存、安装体积待测。

### 复验

沿用上节创建受保护的临时 `ICB_DATA_DIR`、配置工具链和准备测试配对记录的步骤。从 `copiago-rs` 运行。

```powershell
$env:TEMP = $env:ICB_DATA_DIR
$env:TMP = $env:ICB_DATA_DIR
cargo test --release --locked -p copiago-device --lib pasteboard::file_receive::tests::streamed_file_accepts_the_exact_file_size_limit -- --ignored --exact --nocapture
```

真机检查使用「文件」App 中的合成样本。本机另留样本原件，在专用 Windows 测试会话运行。

```powershell
[IO.File]::WriteAllText((Join-Path $env:ICB_DATA_DIR 'settings.json'), '{"pull":true,"pull_types":["file"],"push":false,"file_ask_mb":20}')
cargo build --release --locked -p copiago-app
.\target\release\copiago.exe --headless
```

- 等待 `event=connected transport=wifi`。分别复制 64 KiB 和 1 MiB 样本，预期自动出现 `event=file_complete`。在资源管理器粘贴，核对文件名和 `Get-FileHash` 的两份样本结果。
- 复制 25 MiB 或 100 MiB 样本，预期 `event=file_waiting`，暂时无下载。输入 `ignore <id>` 验证忽略。重新复制后输入新提示里的 `accept <id>`，预期 `event=file_requested` 和 `event=file_complete`。
- 复制 512 MiB 样本并确认，预期 `event=file_complete bytes=536870912`。复制大于 512 MiB 的样本，预期 `event=file_failed reason=too_large`。在已开始的大文件传输中断开连接或退出后，核对临时 `.incoming-*` 目录已清理。
- 退出 headless，运行 `cargo run --release --locked -p copiago-device --example connection_probe -- --check-expired-offer --seconds 60`。等待连接后复制 25 MiB 文件，看到 `first_promised_offer_saved` 后再复制另一个合成文件，预期探针提交过期的 `FetchOffer` 并报告 `offer_expired`。
- 接上同一台已配对 iPhone，在 Wi-Fi 无法建立时预期 `transport=usb`，重复文件接收检查。
