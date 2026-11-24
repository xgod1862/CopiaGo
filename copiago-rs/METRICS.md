# CopiaGo Rust 实测指标

每个阶段记录提交、Windows 版本、设备与连接条件、样本数和实测值。尚未验证的项目填写「未测」。目标见 [需求第 1 节与 F-PKG-4](../docs/rust-rewrite/REQUIREMENTS.md)。

| 阶段 / 提交 | EXE ≤ 6 MB | 安装包 ≤ 6 MB | 安装后 ≤ 12 MB | 托盘空闲 ≤ 10 MB | 弹窗打开 ≤ 20 MB | Wi‑Fi 冷启动到连上 ≤ 3 s |
| --- | --- | --- | --- | --- | --- | --- |
| M0 · T-CORE-01 骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M0 · 工程骨架 | 105,984 字节 | 未测 | 未测 | 未测 | 未测 | 未测 |
| M1 · 集成候选 `07d2e93` | 1,956,352 字节 | 未测 | 未测 | 未测 | 未测 | 1,371.15–2,249.55 ms，中位数 1,922.97 ms（5 次） |
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
