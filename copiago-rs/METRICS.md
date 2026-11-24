# CopiaGo Rust 实测指标

每个阶段记录提交、Windows 版本、设备与连接条件、样本数和实测值。尚未验证的项目填写「未测」。目标见 [需求第 1 节与 F-PKG-4](../docs/rust-rewrite/REQUIREMENTS.md)。

| 阶段 / 提交 | EXE ≤ 6 MB | 安装包 ≤ 6 MB | 安装后 ≤ 12 MB | 托盘空闲 ≤ 10 MB | 弹窗打开 ≤ 20 MB | Wi‑Fi 冷启动到连上 ≤ 3 s |
| --- | --- | --- | --- | --- | --- | --- |
| M0 · T-CORE-01 骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M0 · 工程骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M1 · 集成候选 `07d2e93` | 1,956,352 字节 | 未测 | 未测 | 未测 | 未测 | 1,371.15–2,249.55 ms，中位数 1,922.97 ms（5 次） |
| M2 · T-IMG-02 初版 `d039a6a` | 1,956,864 字节，比 `9be9333` 增加 1,024 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M2 · T-IMG-02 定向修订 | 2,465,280 字节，比 `a25cf2f` 的 2,453,504 字节增加 11,776 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M2 · 配对、文件、历史和图片 | 2,465,280 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M3 · 镜像下载、签名及挂载 `76463e3` | 2,748,416 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M4 · 正式托盘 `0294ce2` | 3,227,648 字节 | 未测 | 未测 | 2,392,064 字节（断线夹具峰值） | 12,939,264 字节（历史／设置两页峰值） | 未测 |
| M5 · T-QA-01 最终构建 `79c16b3` | 3,308,032 字节 | 3,642,571 字节 | 7,785,968 字节（TestId 全新实装） | 2,334,720 字节（断线峰值） | 13,025,280 字节（历史／设置峰值） | 未测，探针未建立会话 |

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
| M3 · 集成候选 `76463e3` | Windows 11 Pro 26H2 build 26300.9974、x64、Rust 1.100.0 | `cargo build --workspace --release --locked --target-dir target/stage-release-m3`，新目标目录 1 次 | 工作区构建 37.05 s，EXE 2,748,416 字节。 |

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

## M3 · T-DDI-01 镜像下载

Windows 11 Pro 26H2 build 26300.9974、x64、Rust 1.100.0。Release 下载探针 296,960 字节，使用受保护的临时 `ICB_DATA_DIR`，两套镜像均经 WinHTTP 从 GitHub Raw 下载。8 个文件独立核对大小与 SHA-256，全部符合 Python 固定清单。热缓存两套各 10 ms，均无下载。

| 镜像 | 下载字节 | 耗时 | 采样 Private Bytes 峰值 | 采样总工作集峰值 |
| --- | ---: | ---: | ---: | ---: |
| Cryptex | 18,045,645 | 2.304 s | 2,670,592 | 12,607,488 |
| Personalized | 17,886,923 | 1.754 s | 2,621,440 | 12,566,528 |

从 `copiago-rs` 运行 `cargo run --release --locked -p copiago-ddi --example ddi_download -- cryptex` 和同一命令的 `personalized`。在空临时目录应下载成功，重跑返回缓存。使用 `Get-FileHash -Algorithm SHA256` 逐项核对。[固定清单](crates/copiago-ddi/src/download/pinned.rs)

## M3 · T-DDI-02 签名与挂载

- Windows x64 build 26300、Rust 1.100.0。Release `copiago.exe` 为 2,747,904 字节，较 `fd44c1e` 的 2,464,768 字节增加 283,136 字节。工作区格式检查、Clippy（含全部 target）和 192 项测试通过，6 项默认忽略。新增的 9 个包在 Cargo 元数据中均使用 MIT 或 MIT/Apache-2.0。
- 3 次 Release 合成 Cryptex 传输均完成 5 段载荷，按识别出的标识和版本替换未挂载的旧 DDI 并确认安装记录。17,239,040 字节镜像从准备到确认用时 31.89–34.72 ms，传输中采样的专用工作集为 18,374,656–18,411,520 字节，峰值私有提交量为 19,189,760–19,230,720 字节，总工作集峰值为 42,250,240–42,811,392 字节。采样使用 `K32GetProcessMemoryInfo` 的 `PROCESS_MEMORY_COUNTERS_EX2`。
- 两份 T-DDI-01 固定 27B52 镜像清单使用合成标识和 nonce 做离线请求对照。Personalized 的 140 组 board/chip 请求和 Cryptex 的 2 组生产标志请求均与 Python 11.61.1 一致，随机 UUID 单独验证为大写 v4。
- TSS 直接连接 `gs.apple.com:443`，用 rustls/ring 校验 Apple 根和服务器证书。专用根 SHA-256 为 `B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024`。本机 Release 发送空 plist 的冒烟用时 0.63 s，独立验证同一端点返回 HTTP 200 和 TSS `STATUS=94`。此响应只验证 HTTPS 和 Apple 业务接口可达。 [Apple PKI](https://www.apple.com/certificateauthority/) [Apple 根证书指纹表](https://support.apple.com/en-us/126047)
- CopiaGo 按 Python 的字段顺序和 256 KiB 轮转格式写入 `developer-setup.log`。测试覆盖状态码、统计字段、轮转覆盖以及含合成设备标识的异常文本过滤。
- 真机首次签名、重启后的实际挂载、USB/Wi-Fi 重连、开发者模式提示、端到端耗时和内存待验。

### 复验

从 `copiago-rs` 运行。使用当前用户和 SYSTEM 专用的临时 `ICB_DATA_DIR`，并准备授权测试手机的配对记录。

```powershell
$env:RUSTUP_HOME = 'E:\rust\rustup'
$env:CARGO_HOME = 'E:\rust\cargo'
$env:PATH = "E:\rust\cargo\bin;$env:PATH"
$env:TEMP = $env:ICB_DATA_DIR
$env:TMP = $env:ICB_DATA_DIR
cargo test --release --locked -p copiago-ddi --lib setup::tests::cryptex::replaces_only_unmounted_ddi_and_uploads_its_five_cryptex_payloads -- --exact --nocapture
cargo test --release --locked -p copiago-ddi --lib setup::tests::apple_tss_returns_a_business_response_without_device_identifiers -- --ignored --exact --nocapture
cargo build --release --locked -p copiago-device --example connection_probe
```

- 重启测试 iPhone 并解锁。运行 `.\target\release\examples\connection_probe.exe --seconds 15`，观察 `developer_services_missing` 和只读探针的安装类状态。
- 运行 `.\target\release\examples\connection_probe.exe --install-developer-image --seconds 1200`。预期缺服务后准备镜像，按需下载、签名和挂载，随后重连一次并输出 `connected wireless=true`。同一已配对手机在无线不可用时接线复验，预期 `connected wireless=false`。Personalized 使用设备缓存票据时从准备直接进入挂载。
- 在测试机关闭开发者模式并重启解锁，再运行带 `--install-developer-image` 的探针。预期 `state=Developer`，显示「设置 → 隐私与安全性 → 开发者模式」的开启路径。在测试机开启并按提示重启后复验。 [Apple Developer Mode 文档](https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device.md)
- 用 `Get-Content -LiteralPath (Join-Path $env:ICB_DATA_DIR 'developer-setup.log') -Tail 20` 检查阶段和结果。TSS 证书验证失败显示 `error=SSLError`，Apple 拒绝请求显示 `error=TSSError`。记录验证重连耗时、专用工作集和下载/签名/安装日志。

## M4 · 宿主和 Windows 组件

以下为独立 Release 探针，Windows 11 Pro 26H2 build 26300.9974 x64、Rust 1.100.0。正式 EXE 的托盘和弹窗采样见 M4 集成记录。

| 任务 | 探针体积 | 耗时 | 专用工作集 |
| --- | ---: | --- | --- |
| T-UI-01 空卡 | 155,136 B | 初始化至发出显示 21 ms | 隐藏后 9 次采样为 2,711,552–2,797,568 B |
| T-UI-02 正常动画 | 230,400 B | 初始化至发出显示 20 ms | 可见峰值采样 5,181,440 B，隐藏 2,629,632 B |
| T-UI-02 减少动画 | 230,400 B | 初始化至发出显示 20 ms | 可见峰值采样 5,308,416 B，隐藏 2,756,608 B |
| T-WIN-02 托盘 | 250,880 B | 启动到矩形可查 10.811–11.576 ms（3 次） | 空闲 9 次采样为 1,196,032–1,212,416 B |
| T-WIN-03 更新 | 217,088 B | 一次真实 API 检查 15.889 s，含启动延迟 | 13 次采样峰值 2,150,400 B |
| T-UI-03 历史页 | 352,256 B | 50 条图像构建 26 ms | 可见峰值 8,224,768 B，隐藏峰值 3,416,064 B |
| T-UI-04 设置页（普通／减少动画） | 328,704 B | 两轮首显 21 ms | 可见峰值 7,081,984／7,045,120 B，隐藏末次 2,772,992／2,695,168 B |
| T-UI-05 键盘及 UIA | 299,008 B | 首显 21 ms，跨进程整体验证 85–89 ms | 可见峰值 4,534,272 B，隐藏峰值 2,748,416 B |
| T-UI-06 托盘定位 | 1,135,104 B | 原生显示调用 16.312–17.487 ms（3 轮） | 70 个样本中，可见峰值 4,505,600 B，隐藏峰值 3,063,808 B |

两台显示器当前均为 96 DPI。T-UI-01 在实际屏幕验证模拟 150%/200% 的卡片为 588×678／784×904 像素，透明边缘和阴影落到另一进程底窗。T-UI-02 两个动画策略均按预期完成视觉树清理。T-WIN-02 保留实际 Explorer，仅删除自有测试图标再定向发送 `TaskbarCreated`，三次恢复为 2.520–4.344 ms，集成版再次实测为 2.394 ms。T-WIN-03 以稳定版 `1.1.0` 检测到 `1.2.0`，系统接受发布页打开。由 Python 三段数字版本规则决定，`2.0.0-beta` 查询后无提示。[1.2.0 发布页](https://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0)

历史页用合成数据做 Python 与 Rust 的四组原尺寸对照。合并后额外复验 50 条列表、原生失焦／恢复、Esc、跨进程 UIA，均通过。[历史页对照图](docs/history-comparison.png)

## M4 · T-APP-02 正式托盘宿主

Windows 11 Pro 26H2，build 26300.9974，x64，Rust 1.100.0。最终 Release `copiago.exe` 为 **3,227,648 字节**。SHA-256 为 `D1234D4AD67F627B4453D43313CFBC477DC8F16044EC32C3A4A6383E01F31E06`。

PID 28772，隔离数据目录仅允许当前 Windows 账户与 SYSTEM。夹具含虚构的断线配对文件和 Python 生成的 DPAPI 历史，两个来源各 50 条。关闭两方向自动同步、历史保存、通知和更新。每个状态用 `Get-Counter` 每秒采样一次，共 9 次。`ID Process`、`Working Set - Private`、`Private Bytes` 和 `Thread Count` 取同一实例。

| 状态 | 专用工作集（字节） | Private Bytes（字节） | 线程 |
| --- | ---: | ---: | ---: |
| 初始托盘空闲 | 2,387,968–2,392,064 | 3,112,960 | 8 |
| 历史弹窗打开 | 10,117,120–12,775,424 | 16,248,832–19,460,096 | 15 |
| 设置弹窗打开 | 12,877,824–12,939,264 | 19,492,864–19,554,304 | 15 |
| 隐藏动画完成后 | 4,665,344–5,046,272 | 6,668,288–7,630,848 | 14–15 |

本轮采样的托盘空闲与可见弹窗均低于 10 MB / 20 MB 对应验收线。冷进程启动到 `event=ready` 为 201.81 ms，普通第二实例启动到原 PID 的历史页可见为 113.32 ms，计时含测试轮询。手机会话和大图、文件传输的瞬时峰值未在本轮测量。验收前后核对了前台 HWND 和 Windows 剪贴板序列号，结果一致。

格式检查、Clippy 和 245 项测试通过，10 项默认忽略。

### 本机验收

| 项目 | 结果 |
| --- | --- |
| 默认 GUI、托盘、单实例、退出 | 带断线夹具的主进程 `--minimized` 启动后停在托盘。第二个 `--minimized` 进程静默退出，普通入口唤起同一 PID。定向托盘退出正常返回，`--headless` 经 stdout 重定向验证仍输出诊断。 |
| 引导、历史、键盘和无障碍 | 首次启动及忘记后恢复引导，引导收到失焦消息后保持打开，Esc 可关闭。历史收到失焦消息后收起。跨进程 UIA 在应用窗口验证双来源各 50 条、单条删除、清空过期、两次确认、仅清当前列表、停用保存仍可读已有历史。定向键盘消息验证 Tab、Enter 和 Space。 |
| 设置、忘记、数据互通 | 隔离 UIA 验证历史开关、拉取开关、文件阈值、中英切换即时生效、重启保留和未知键保留。确认后由正式设备线程处理隔离配对记录。持有禁止共享删除的句柄时，忘记返回失败并保留记录。释放句柄后再次确认成功。应用直接读取 Python 构造的 DPAPI 历史。运行 `cargo test` 验证全部设置字段、Run 值名及命令、实际状态下的开关和更新关闭/取消；Run 适配器使用独立临时注册表键。 |
| 传输状态与恢复 | 已运行合成设备及存储测试，覆盖 Wi-Fi 优先和 USB 回退、文件阈值、过期、发件请求 ID 和回执、忘记失败后释放在途请求、暂存及自写抑制、未知文件名跟随即时语言。正式窗口收到合成唤醒和网络消息，三次相邻的网络消息合并为一次重连信号。 |
| 更新 | 检查定时、关闭后取消、可用版本提示的数据和发布地址校验通过模拟。正式版本为 `2.0.0-beta`，本轮未在该 EXE 中打开新版发布页。 |

iPhone 仍需在已授权的独立设备上完成 USB 信任、拒绝信任、拔线后无线、无线失败回退 USB、文字和各类图片、文件实际确认与粘贴、512 MiB 边界、视频提示、电脑发送与回声、断线较旧内容、手机重启时镜像挂载和开发者模式提示。当前机器无可用 USB iPhone，已配对记录的最新只读 Wi-Fi 探针未建立会话。本轮只运行了合成电源/网络事件，物理睡眠、实际 Wi-Fi/VPN 切换、24 h 稳定性、真实混合 DPI、讲述人语音、Windows 登录启动和安装覆盖升级均待复验。

### GUI 与计数器复验

从 `copiago-rs/` 运行。以下命令创建只有当前用户和 SYSTEM 可访问的独立数据根，准备断线夹具并避免抢占前台焦点。删去该夹具的虚构配对文件后用 `--minimized` 启动，可检查无配对时自动打开引导。可在目录中预先放入当前 Windows 账户使用 Python 1.2.0 生成的合成 `history.sqlite3`，复验有记录时的工作集。

```powershell
$env:RUSTUP_HOME = 'E:\rust\rustup'
$env:CARGO_HOME = 'E:\rust\cargo'
$env:PATH = "E:\rust\cargo\bin;$env:PATH"
cargo build --release --locked -p copiago-app

$fixtureId = [guid]::NewGuid().ToString('N')
$fixture = Join-Path $env:TEMP ('CopiaGo-GUI-' + $fixtureId)
New-Item -ItemType Directory -Path $fixture | Out-Null
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetAccessRuleProtection($true, $false)
$currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().User
$systemUser = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
foreach ($sid in @($currentUser, $systemUser)) {
    $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $fixture -AclObject $acl
$env:ICB_DATA_DIR = $fixture
$env:ICB_INSTANCE_NAMESPACE = 'tapp02-' + $fixtureId
$env:ICB_NO_ACTIVATE = '1'
$env:ICB_DIAGNOSTIC_FILE = Join-Path $fixture 'diagnostic.log'
$deviceFixture = Join-Path $fixture 'device-data'
New-Item -ItemType Directory -Path $deviceFixture | Out-Null
[IO.File]::WriteAllText((Join-Path $deviceFixture 'remote_00008020-0000000000000000.plist'), 'invalid synthetic record')
[IO.File]::WriteAllText((Join-Path $fixture 'settings.json'), '{"pull":false,"push":false,"notifications":false,"update_check":false}')
$exe = (Resolve-Path .\target\release\copiago.exe).Path
$process = Start-Process -FilePath $exe -ArgumentList '--minimized' -PassThru -WindowStyle Hidden

function Measure-CopiaGo([int]$targetPid) {
    $paths = '\Process(copiago*)\ID Process', '\Process(copiago*)\Working Set - Private', '\Process(copiago*)\Private Bytes', '\Process(copiago*)\Thread Count'
    Get-Counter -Counter $paths -SampleInterval 1 -MaxSamples 9 | ForEach-Object {
        $instance = $_.CounterSamples | Where-Object {
            $_.Path -like '*\id process' -and [int]$_.CookedValue -eq $targetPid
        } | Select-Object -First 1
        if ($instance) {
            $_.CounterSamples | Where-Object InstanceName -eq $instance.InstanceName |
                Select-Object Timestamp, Path, CookedValue
        }
    }
}
Measure-CopiaGo $process.Id
Start-Process -FilePath $exe -WindowStyle Hidden
Measure-CopiaGo $process.Id
```

托盘菜单打开「连接与设置」后再次采样。关闭弹窗，等待收起动画再采样。通过托盘菜单的「退出」结束进程，诊断文件中应出现 `event=stopped`。真机步骤使用单独的受控测试设备和已知合成剪贴板数据。

## M5 · T-QA-01 稳定性与指标回归

2026-11-24，Windows 11 Pro 26H2 build 26300.9974 x64，20 个逻辑处理器，Rust 1.100.0，Inno Setup 6.7.4。被测构建为 `79c16b3` 的 x64 Release `2.0.0-beta`。MB 按 1,000,000 字节计算，文件传输上限按 `MAX_FILE_BYTES` 的 512 MiB（536,870,912 字节）计算。[验收指标](../docs/rust-rewrite/REQUIREMENTS.md) · [文件上限](crates/copiago-core/src/files.rs)

| 指标 | 验收线 | 最终构建实测 | 判定 |
| --- | --- | --- | --- |
| EXE 大小 | ≤ 6 MB | 3,308,032 字节 | 通过 |
| x64 正式安装包 | ≤ 6 MB | 3,642,571 字节 | 通过 |
| x64 便携 ZIP | 有 EXE 和 `portable-data` | 1,839,959 字节，已验目录和标记 | 通过 |
| 安装后大小 | ≤ 12 MB，排除运行时 DDI | 7,785,968 字节，TestId 全新实装，含卸载器及许可证 | 通过 |
| 托盘空闲专用工作集 | ≤ 10 MB | 峰值 2,334,720 字节，假配对断线 | 本机通过，连机未测 |
| 打开弹窗专用工作集 | ≤ 20 MB | 历史页峰值 12,808,192，设置页峰值 13,025,280 字节，断线 | 本机通过，连机未测 |
| 瞬时专用工作集 | ≤ 30 MB | 正式 EXE 常规四状态的 1 s 采样最大值 13,025,280 字节。当前源码的独立 Debug 渐进 JPEG 缩略图探针最高 146,251,776 字节 | 缩略图路径有超线复现，正式 EXE 端到端峰值未测 |
| Wi‑Fi 冷启动至连上 | ≤ 3 s，已解锁、同一局域网 | 最终 EXE 无成功会话样本。断线场景从发起启动到观察到 `event=ready` 为 86.53 ms（1 次） | 待真机 |
| N-3 已连接空闲 CPU | 平均 < 0.1%，排除 2 s 元数据轮询 | 断线短测 109.375 ms CPU / 122.813 s，折合单逻辑核 0.089058%，整机 0.004453% | 待真机 |
| N-4 24 h 内存 | 每小时采样，增长 < 2 MB 且无持续增长 | 断线收起后连续 61 点，首末样本相距 120.743 s。专用工作集变化 −139,264 字节，Private Bytes 变化 +77,824 字节 | 待 24 h 真机 |

正式包以 `scripts/build.ps1 -IsccPath` 重新构建。隔离 TestId 实装和卸载均以退出码 0 完成。Release EXE、便携 ZIP 内 EXE 和 TestId 安装的 EXE 三者的 SHA-256 一致，为 `0F14A4012610A726B9358F5B2126D29D907AA688952411E7CD1D4B99C0741792`。ZIP 包括零字节 `portable-data`、`LICENSE` 及三份附带许可证。正式安装包只编译。安装体积为七个文件的 `Length` 之和。[构建和安装说明](README.md#安装包与便携版) · [构建脚本](scripts/build.ps1)

### 最终 EXE 的本机断线采样

被测 PID 22616。隔离夹具含虚构配对记录、Python 写入的 DPAPI 合成历史（手机、电脑各 50 条）。关闭两侧自动同步、历史保存、系统通知和更新。以相同 `Get-Counter` 样本内的 `ID Process` 选中 PID，再按实例读取 `Working Set - Private`、`Private Bytes` 和 `Thread Count`。四个状态各 9 点、间隔 1 s，短测设为 2 s。CPU 取短测开始和结束的进程累计 CPU 时间差。一秒计数器采样只说明观测点的峰值。[Microsoft Get-Counter](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.diagnostics/get-counter?view=powershell-7.5)

| 正式 EXE 状态 | 专用工作集（字节） | Private Bytes（字节） | 线程 | 样本 |
| --- | ---: | ---: | ---: | ---: |
| 初始托盘 | 2,330,624–2,334,720 | 3,047,424–3,051,520 | 8 | 9 |
| 历史弹窗 | 10,022,912–12,808,192 | 16,130,048–19,562,496 | 16 | 9 |
| 设置弹窗 | 12,963,840–13,025,280 | 19,677,184–19,738,624 | 16 | 9 |
| 弹窗隐藏后 | 4,767,744–5,152,768 | 6,856,704–7,823,360 | 15–16 | 9 |
| 隐藏后的断线短测 | 4,628,480–4,825,088 | 6,787,072–7,213,056 | 5–15 | 61 |

第二次 `--minimized` 静默退出，普通实例启动至原 PID 的历史页可见为 124.25 ms。定向消息打开设置页、收起弹窗并退出，诊断写出 `event=stopped`，退出码 0。独立空数据根以 `--minimized` 启动，出现 `event=ready paired=false`、可见的配对引导和 `state=Setup`，正常退出。两次复验前后，前台窗口和 Windows 剪贴板序列号分别一致。另在本机运行自有托盘图标恢复探针（3.331 ms）、双屏定位（实际均 96 DPI，另模拟 144/192 DPI）和键盘焦点绘制探针。[宿主](crates/copiago-app/src/gui.rs) · [托盘探针](crates/copiago-win/src/tray/tests.rs) · [双屏探针](crates/copiago-ui/src/composition/position/tests/live.rs)

`cargo fmt --check`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过。默认 248 项通过、10 项按原标记忽略。另定向执行 512 MiB 合成文件、Python/Rust 双向历史和上述三个 Windows 实机探针，均通过。512 MiB 合成文件用现行 Debug 测试经分帧、验证、刷盘、发布并读回校验，接收至发布 6,748 ms，含校验 14,104 ms。[文件接收测试](crates/copiago-device/src/pasteboard/file_receive/tests.rs) · [历史互通测试](crates/copiago-core/src/history/tests.rs)

### 真机只读复验

仅将真实 Python 1.2.0 同账户的一个 `remote_*.plist` 和 `device-info.bin` 副本置于受保护的临时 `ICB_DATA_DIR`，ACL 只允许当前账户和 SYSTEM。Wi‑Fi 探针未使用 `--write-identifier`，连接探针也未传数据保存、自动挂载或文件请求选项。Apple Mobile Device Service 运行中。原目录的 DDI 缓存存在，手机上的挂载状态本轮未知。[Wi‑Fi 探针](crates/copiago-device/examples/wifi_probe.rs) · [USB 探针](crates/copiago-device/examples/usb_probe.rs) · [连接探针](crates/copiago-device/examples/connection_probe.rs)

| 本轮探针 | 结果 |
| --- | --- |
| `wifi_probe` | 486 ms 返回 `could not connect to the RemotePairing endpoint`，无 pair-verify/RSD 成功证据 |
| `usb_probe` | 2 ms 返回 `no USB device found` |
| `connection_probe --seconds 25` | 在 `Connecting` 和 `Waiting` 间重试，25 s 内没有 `Connected` 事件 |

从 `copiago-rs` 运行。先将 `ICB_DATA_DIR` 指向当前账户可解密的受保护配对副本。

```powershell
cargo run --release --locked -p copiago-device --example wifi_probe
cargo run --release --locked -p copiago-device --example usb_probe
cargo run --release --locked -p copiago-device --example connection_probe -- --seconds 25
```

### 需求第 8 节核对

复验用专用 Windows 测试会话和测试手机，剪贴板样本用合成数据。以 Wi‑Fi 为优先验收链路，USB 用于配对、信任、回退与对应协议步骤。未完成验收的使用场景继续用 Python 1.2.0。[需求第 8 节](../docs/rust-rewrite/REQUIREMENTS.md#8-验收清单与-python-120-对照) · [Python 行为](../source/iphone_clip/)

| 核对项 | 本轮合成或自动化 | 本轮本机 | 此前阶段实机 | 待验 |
| --- | --- | --- | --- | --- |
| 全新安装、托盘、USB 信任、拔线后 Wi‑Fi | 配对阶段、多台 USB 筛选、拒绝信任文案与配对记录保存 | TestId 实装、自有托盘和空目录配对引导 | M1 从已有记录完成 Wi‑Fi pair-verify | 新设备点信任并输入密码，配对后拔线连 Wi‑Fi，补测拒绝信任 |
| 接线保留 Wi‑Fi、回退同一设备 USB、恢复 Wi‑Fi | 状态机复验优先级、同一设备约束和重连 | 无会话 | M1 建立过 Wi‑Fi 会话 | 接线确认仍为 Wi‑Fi。无线失效时确认同一设备走 USB，恢复无线并拔线后回到 Wi‑Fi |
| 手机文字 ≤ 1 s 到电脑剪贴板和历史 | 通知、2 s 轮询、文本处理和 DPAPI 历史 | — | M1 Wi‑Fi 监听过自然发生的手机文字变化并暂存 | 开启拉取及手机历史，测复制到电脑可粘贴和历史出现的时间 |
| HEIC 照片、PNG 截图、Safari 和 Notes 图片 | PNG、同条目 JPEG/PNG 替代、webarchive 和 Notes 超时 | 独立 HEIC 样本返回 `0xc00d5212` | — | 逐项复制粘贴四类来源，核对尺寸和透明度，覆盖仅有 HEIC 的样本 |
| 「文件」App 小文件自动、大文件询问、超过 512 MiB 拒绝 | 阈值、过期、失败清理、超限拒绝。合成 512 MiB 发布后校验通过 | — | — | 复制 <256 KiB、1 MiB、100 MiB、512 MiB 和超限样本，核对阈值确认、资源管理器粘贴及内容 |
| 视频等不支持类型提示 | 类型识别、跳过正文和提示文案 | — | — | 复制受控视频，核对提示及两侧历史 |
| Windows 文字、PNG 推到手机及回声抑制 | SET、120 s 指纹、变更标识和 Windows 自写标记 | — | 原型验证实际中文和 emoji 的 SET/读回。T-WIN-01 在 Windows runner 验证剪贴板格式 | 开启推送，验证手机粘贴中文、emoji 和透明 PNG，确认两端无回推 |
| 断线期间复制、重连托盘菜单可用 | 拉取关闭和 Windows 已更新时的暂存，菜单命令与无条目时隐藏 | — | M1 验证实际 Wi‑Fi 恢复，另观察到文字暂存 | 关闭自动拉取，断线复制再重连，核对菜单和执行后的 Windows 剪贴板 |
| 手机重启后重新挂载镜像 | Cryptex 合成传输、签名请求对照、错误和进度 | 原目录有 DDI 缓存，手机挂载状态未知 | M3 下载并核对两个镜像，Apple TLS 冒烟通过 | 用测试手机重启，分别记录下载或热缓存、签名、挂载和首次重连 |
| Windows 睡眠、换 Wi‑Fi、开关 VPN 恢复 | 信号合并和连接状态重置 | — | M1 注入唤醒和网络变化后恢复真机 Wi‑Fi。M4 验证正式宿主的模拟唤醒和合并 | 用专用测试机逐项执行实际切换，记录各自恢复耗时 |
| 全部设置生效、重启保留、即时语言切换 | 十个 JSON 键、类型开关、即时语言和重读 | 最终 EXE 设置页可见 | M4 隔离 UIA 验证重启、即时语言、历史和拉取开关、文件阈值 | 最终 EXE 在连机时逐项验证设置实际效果并重启 |
| 忘记后需要重新 USB 配对 | 确认、删除记录、失败保留和退回 Setup | — | M4 隔离宿主从 UI 忘记合成配对，锁文件失败及释放后成功 | 用测试手机验证忘记后需重新信任和配对，再转 Wi‑Fi |
| 两历史列表、上限、删除、清空及停用保存 | 两来源 50 条／128 MiB、单条 32 MiB、去重、Python/Rust 双向 DPAPI 互通 | 正式 EXE 用双侧各 50 条的合成库打开历史页 | M4 隔离 UIA 验证删除、独立清空、过期确认和停用保存 | 最终连机复验两边记录及停用后的实际表现 |
| 开机启动与注册表一致 | Windows 隔离注册表键的值名、格式和状态 | — | T-PKG-02 真实旧安装副本的隔离启动键覆盖规则通过 | 专用账户核对实际 Run 值并重新登录 |
| 更新提示与跳转 | 定时、取消、Python 纯数字版本解析和发布 URL 白名单 | — | M4 稳定版检查探针发出更新并打开发布页。当前 `2.0.0-beta` 版本字符串无法按 Python 格式比较 | 用低版本稳定测试构建和受控目标复验最终 UI 提示与打开发布页 |
| 原样接管 Python 1.2.0 | 路径选择、未知键、ACL、DPAPI 与历史迁移 | 只读探针使用真实配对副本，未连上 | T-PKG-02 真实安装副本覆盖保留两账户 7 个文件的哈希和 ACL。10 项设置、14 条历史与配对记录可读 | 专用测试安装在同账户升级并沿用原目录，无需重配即连 Wi‑Fi，核对历史、设置和启动项 |
| 安装包、安装后、托盘、弹窗及 24 h | 体积门禁与合成图像、文件负载测试 | 最终包、隔离实装、断线内存和短测已记录 | M1 headless 版本真机 Wi‑Fi 30 min 和冷启有实测 | 最终 EXE 连机专用工作集、瞬时峰值、冷启、CPU 及 24 h |

历史证据见本文件各章节、[原型报告](../poc-rust/REPORT.md)、[阶段进度](PROGRESS.md) 与 T-PKG-02 集成提交 `79c16b3`。Python 文件、历史、更新和登录启动口径分别来自 [device.py](../source/iphone_clip/device.py)、[history.py](../source/iphone_clip/history.py)、[updater.py](../source/iphone_clip/updater.py) 和 [ui.py](../source/iphone_clip/ui.py)。

### 问题清单

- **渐进 JPEG 峰值越线。** 同一 Pillow 13.0.0 双色合成图（8000 × 6000，quality 85），用当前源码的独立 Debug 子进程走 `decode_scaled` 并编码为 96 × 72 PNG。基线 JPEG 三次通过，子进程峰值总工作集为 13,058,048–13,066,240 字节。渐进 JPEG 三次均解码成功后触发峰值断言，`K32GetProcessMemoryInfo` 的采样专用工作集为 146,247,680–146,251,776 字节，峰值私有提交为 147,091,456–147,116,032 字节。此场景超 30 MB，最终 GUI 收到真机大图时的峰值待测。[缩略图测试](crates/copiago-win/src/image/tests.rs) · [Microsoft PROCESS_MEMORY_COUNTERS_EX2](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex2)
- **仅 HEIC 样本在本机无法准备 Windows 剪贴板数据。** 当前测试读取一个 660 字节合成 HEIC 时返回 `0xc00d5212`。若同一手机条目没有可用 PNG/JPEG 替代，还需评估目标电脑的编解码器及实际粘贴路径。[HEIC 样本测试](crates/copiago-win/src/clipboard/tests.rs) · [替代格式请求](crates/copiago-device/src/pasteboard/content.rs)
- **真机证据缺口。** 本轮没有建立 iPhone 会话。Wi‑Fi 冷启、连机 CPU、连机内存、24 h 和第 8 节所列端到端动作仍待验，包括升级后无须重配的首连。短测中专用工作集比收起后的首样本略降，Private Bytes 略升，持续性待长测。

### Wi‑Fi 冷启动、N-3 与 N-4 复验命令

在专用测试电脑、同一 Windows 账户下，从 `copiago-rs` 开始。测试 iPhone 已配对、解锁、与电脑同一局域网，开发者镜像已挂载。先用前文「GUI 与计数器复验」的步骤创建受保护的 `$fixture` 和对应 `ICB_*` 环境变量。跳过生成虚构配对记录、写断线设置和启动，改为执行下方命令。`$fixture` 应是新建的空测试目录。源路径选择该账户在专用测试机的 Python 1.2.0 测试数据根。采样时保持测试设备在线、托盘模式、无复制或其他交互。每次冷启动测试用一个新隔离命名空间、诊断文件和进程。

```powershell
$ErrorActionPreference = 'Stop'
cargo build --release --locked --target x86_64-pc-windows-msvc -p copiago-app
if ($LASTEXITCODE) { throw 'Release build failed.' }
$known = (Get-Item -LiteralPath (Read-Host '输入已有配对的 Python 测试数据目录')).FullName
$device = New-Item -ItemType Directory -Path (Join-Path $fixture 'device-data')
$records = @(Get-ChildItem -LiteralPath (Join-Path $known 'device-data') -File -Filter 'remote_*.plist')
if ($records.Count -ne 1) { throw 'Use a fixture for one already-trusted iPhone.' }
Copy-Item -LiteralPath $records[0].FullName, (Join-Path $known 'device-data\device-info.bin') -Destination $device.FullName
[IO.File]::WriteAllText((Join-Path $fixture 'settings.json'), '{"pull":false,"push":false,"history":false,"pc_history":false,"notifications":false,"update_check":false}')
$exe = (Resolve-Path '.\target\x86_64-pc-windows-msvc\release\copiago.exe').Path
$log = $env:ICB_DIAGNOSTIC_FILE
$launch = [Diagnostics.Stopwatch]::StartNew()
$app = Start-Process -FilePath $exe -ArgumentList '--minimized' -PassThru -WindowStyle Hidden
do {
    if ($app.HasExited) { throw 'Test process exited before connection.' }
    $wifi = (Test-Path -LiteralPath $log) -and
        (Select-String -LiteralPath $log -Quiet -Pattern 'event=connected transport=wifi$')
    if (!$wifi) { Start-Sleep -Milliseconds 50 }
} until ($wifi -or $launch.Elapsed.TotalSeconds -ge 30)
$launch.Stop()
if (!$wifi) { throw 'No Wi-Fi connection. Exit the isolated test copy.' }
[pscustomobject]@{ WiFiColdStartMs = $launch.Elapsed.TotalMilliseconds }

$paths = '\Process(copiago*)\ID Process', '\Process(copiago*)\Working Set - Private',
         '\Process(copiago*)\Private Bytes', '\Process(copiago*)\Thread Count'
$csv = Join-Path $fixture 'wifi-idle-24h.csv'
$clock = [Diagnostics.Stopwatch]::new()
for ($hour = 0; $hour -le 24; $hour++) {
    $waitMs = 3600000L * $hour - $clock.ElapsedMilliseconds
    if ($waitMs -gt 0) { Start-Sleep -Milliseconds $waitMs }
    if ($app.HasExited) { throw 'Original test PID exited.' }
    $sample = Get-Counter -Counter $paths -MaxSamples 1 -ErrorAction SilentlyContinue
    $instance = $sample.CounterSamples | Where-Object {
        $_.Path -like '*\id process' -and $_.Status -in 0,1 -and [int]$_.CookedValue -eq $app.Id
    } | Select-Object -First 1
    if (!$instance) { throw 'Missing test PID sample.' }
    $counters = @($sample.CounterSamples | Where-Object InstanceName -eq $instance.InstanceName)
    if ($counters.Count -ne 4 -or @($counters | Where-Object Status -notin 0,1).Count) {
        throw 'Invalid test PID counters.'
    }
    if ($hour -eq 0) { $clock.Start() }
    $app.Refresh()
    [pscustomobject]@{
        Hour = $hour
        UTC = $sample.Timestamp.ToUniversalTime().ToString('o')
        PID = $app.Id
        WSPrivate = [long]($counters | Where-Object Path -like '*\working set - private').CookedValue
        PrivateBytes = [long]($counters | Where-Object Path -like '*\private bytes').CookedValue
        Threads = [int]($counters | Where-Object Path -like '*\thread count').CookedValue
        CpuSeconds = $app.TotalProcessorTime.TotalSeconds
    } | Export-Csv -LiteralPath $csv -NoTypeInformation -Append
}
$rows = @(Import-Csv -LiteralPath $csv)
$seconds = $clock.Elapsed.TotalSeconds
$events = @(Get-Content -LiteralPath $log)
$firstWiFi = 0..($events.Count - 1) | Where-Object { $events[$_] -match 'event=connected transport=wifi$' } | Select-Object -First 1
$lateEvents = @($events | Select-Object -Skip ($firstWiFi + 1) | Where-Object {
    $_ -match 'event=(connected|status|stopped|popup_shown|pull_held|pull_complete|push_queued|setup_progress)\b'
})
[pscustomobject]@{
    Samples = $rows.Count
    ElapsedSeconds = $seconds
    ConnectionOrActivityEventsAfterFirstWiFi = $lateEvents.Count
    WorkingSetGrowthBytes = [long]$rows[-1].WSPrivate - [long]$rows[0].WSPrivate
    PrivateBytesGrowth = [long]$rows[-1].PrivateBytes - [long]$rows[0].PrivateBytes
    CpuOneLogicalProcessorPercent = 100 * ([double]$rows[-1].CpuSeconds - [double]$rows[0].CpuSeconds) / $seconds
}
```

冷启动阈值为 3,000 ms，计时包含进程启动和 50 ms 间隔的轮询探测。N-4 核对同一 PID 的 25 个点、采样时段 ≥ 86,400 s、专用工作集末值减初值严格小于 2,000,000 字节，并检查小时序列是否持续上涨。Private Bytes 记录供交叉检查。诊断若显示断线、重连、弹窗或复制活动，按实际条件记录并另取稳定空闲样本。N-3 可先从同一 Wi‑Fi 空闲进程取一段累计 CPU 差值除以实际墙钟时间。命令报告的 CPU 包含 2 s 轮询。若含轮询的平均值已低于 0.1%，可用作上界。若达到或超过阈值，需另测并剔除轮询开销。采样完成后从托盘退出隔离实例，核对诊断中的 `event=stopped`。[需求 N-1、N-3、N-4](../docs/rust-rewrite/REQUIREMENTS.md#5-非功能需求) · [Microsoft Get-Counter](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.diagnostics/get-counter?view=powershell-7.5)
