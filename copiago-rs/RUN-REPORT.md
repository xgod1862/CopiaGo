# CopiaGo Rust 重写交付报告

本轮交付 M0–M5 共 33 张任务卡。20 张已合并、13 张待真机验证。状态按 [进度台账](PROGRESS.md) 记录，待真机验证的任务已集成代码并通过本地质量门禁，仍需完成下列设备操作。连接沿用 Python 1.2.0 的行为，Wi-Fi 优先，无线失败后回退到同一台已配对手机的 USB。 [连接验收](https://github.com/xgod1862/CopiaGo/pull/10)

## 任务结果

| 任务 | 状态 | PR | 结果 |
| --- | --- | --- | --- |
| T-CORE-01 | 已合并 | [PR #8](https://github.com/xgod1862/CopiaGo/pull/8) | 建立六 crate 工作区、Release 配置和 Windows 版本资源。 |
| T-CI-01 | 已合并 | [PR #9](https://github.com/xgod1862/CopiaGo/pull/9) | 添加 Windows Rust 质量、6 MB 体积和依赖许可门禁。 |
| T-CORE-02 | 已合并 | [PR #9](https://github.com/xgod1862/CopiaGo/pull/9) | 落地四种数据路径、DPAPI、原子保存和 Windows ACL。 |
| T-CORE-03 | 已合并 | [PR #9](https://github.com/xgod1862/CopiaGo/pull/9) | 持久化十项设置并兼容旧文件、异常值与未知键。 |
| T-CORE-04 | 已合并 | [PR #9](https://github.com/xgod1862/CopiaGo/pull/9) | 核对 170 组中英翻译和运行时语言切换。 |
| T-DEV-01 | 待真机验证 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 实现 USB 开发者隧道与 RSD 服务枚举。 |
| T-DEV-02 | 待真机验证 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 接通 Darwin 变化通知、两秒补偿轮询和拉取。 |
| T-DEV-03 | 待真机验证 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 实现文字和 PNG 推送、回声抑制与 SET 变更标识适配。 |
| T-DEV-04 | 已合并 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 真实 Wi-Fi 配对验证通过，Rust 和 Python 可复读隔离 DPAPI 记录。 |
| T-DEV-05 | 待真机验证 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 沿用 Python 的无线优先与同设备 USB 回退，模拟唤醒后真机重连通过。 |
| T-DEV-06 | 已合并 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 用原生 mDNS 实测多网卡下按 Python 配对标识匹配设备 8/8。 |
| T-WIN-01 | 已合并 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 接入 Windows 文本、PNG、DIB、文件剪贴板和监听。 |
| T-APP-01 | 待真机验证 | [PR #10](https://github.com/xgod1862/CopiaGo/pull/10) | 组装 headless 宿主并完成真实 Wi-Fi 30 分钟基线。 |
| T-DEV-07 | 待真机验证 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 流式落盘保留 512 MiB 上限，合成协议传输 12/12。 |
| T-DEV-08 | 待真机验证 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 接入文件确认、校验落盘、Windows 粘贴和过期清理。 |
| T-DEV-09 | 待真机验证 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 接入 USB 信任与无线配对保存、忘记和重新配对。 |
| T-CORE-05 | 已合并 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 系统 SQLite 加 DPAPI 历史与 Python 双向互通。 |
| T-IMG-01 | 已合并 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 透明 PNG 和基线 48 MP JPEG 缩略图通过，渐进式样本峰值超线。 |
| T-IMG-02 | 待真机验证 | [PR #11](https://github.com/xgod1862/CopiaGo/pull/11) | 优先向同一条目请求 PNG 或 JPEG，保留系统 HEIC 解码。 |
| T-DDI-01 | 已合并 | [PR #12](https://github.com/xgod1862/CopiaGo/pull/12) | 实现镜像下载、续传和原子发布，两套实测清单全匹配。 |
| T-DDI-02 | 待真机验证 | [PR #12](https://github.com/xgod1862/CopiaGo/pull/12) | 接入按 iOS 选择镜像、Apple 签名、挂载及一次重拨。 |
| T-UI-01 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 建立 Composition/WARP 宿主并验证原生透明区穿透。 |
| T-UI-02 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 接入合成动画、组件和系统减少动画设置。 |
| T-UI-03 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 交付与 Python 对照的双来源历史、复制、发送和清空。 |
| T-UI-04 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 交付连接与设置页、设备状态和忘记二次确认。 |
| T-UI-05 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 通过原生键盘交互、焦点可见和跨进程 UIA。 |
| T-UI-06 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 实现并实测真实托盘锚定、双屏定位及保锚重建。 |
| T-WIN-02 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 接入托盘、单实例和开机启动，验证图标重建和注册表命令。 |
| T-WIN-03 | 已合并 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 接入在线更新和发布跳转，保留 Python 的稳定版本比较。 |
| T-APP-02 | 待真机验证 | [PR #13](https://github.com/xgod1862/CopiaGo/pull/13) | 发布入口默认进入正式托盘，联通窗口、设置、历史和设备状态。 |
| T-PKG-01 | 已合并 | [M5]({{M5_PR_URL}}) | 交付默认 x64 安装包和便携 ZIP，隔离安装卸载通过。 |
| T-PKG-02 | 待真机验证 | [M5]({{M5_PR_URL}}) | 真实 Python 1.2.0 副本覆盖后，设置、双来源历史和配对记录兼容。 |
| T-QA-01 | 待真机验证 | [M5]({{M5_PR_URL}}) | 回归最终二进制、安装占用及离线内存，登记真机和 24 小时欠测项。 |

## 最终构建与目标

最终 x64 Release 为 `2.0.0-beta`。体积按十进制 MB，内存按进程专用工作集。安装后体积来自隔离 TestId 实装，含卸载器及许可证。托盘和历史／设置弹窗为断线夹具采样。目标和复现实验见 [需求第 1 节、F-PKG-4](../docs/rust-rewrite/REQUIREMENTS.md) 与 [M5 实测](METRICS.md)。

| 指标 | 目标 | 最终实测 | 判定 |
| --- | ---: | ---: | --- |
| EXE | ≤ 6 MB | 3,308,032 B | 通过 |
| 安装包 | ≤ 6 MB | 3,642,571 B | 通过 |
| 安装后（排除运行时 DDI） | ≤ 12 MB | 7,785,968 B | 通过 |
| 托盘空闲专用工作集 | ≤ 10 MB | 2,334,720 B | 断线夹具通过，连机未测 |
| 打开弹窗专用工作集 | ≤ 20 MB | 13,025,280 B | 断线夹具通过，连机未测 |
| Wi-Fi 冷启动到连上 | ≤ 3 s | 未测 | 最终版无连接样本 |

便携 ZIP 为 1,839,959 B。构建、ZIP 内和实装 EXE 的 SHA-256 一致。隔离实装、卸载、覆盖升级及数据读取通过。真实 Python 1.2.0 副本升级后仍可读取 10 项设置、两侧共 14 条历史与实际 DPAPI 配对记录。 [M5 实测](METRICS.md) · [升级步骤](README.md#从-python-120-覆盖升级)

M1 的 headless 集成候选在同一局域网上实际连上 5 次，中位 1.923 s。M5 从启动到托盘就绪为 86.53 ms，Wi-Fi 和 USB 只读探针均未建立会话。M5 断线短测 61 点，跨度 120.743 s，专用工作集首末相差 −139,264 B，CPU 平均约占一个逻辑处理器的 0.0891%。最终版已连接空闲 CPU、连机内存和 24 小时稳定性均为未测。 [阶段实测](METRICS.md)

本地 `cargo fmt --check`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 和依赖许可证检查通过。248 项默认通过，10 项按原标记忽略，另定向通过 512 MiB 合成接收、Python 双向历史和原生 Windows 探针。 [最终质量检查]({{M5_PR_URL}}/checks) · [复验结果](METRICS.md)

## 待真机与人工复验

### 建立隔离环境

在仓库的 `copiago-rs` 目录打开 PowerShell。下面的命令构建 Release 并准备只有当前用户和 SYSTEM 可访问的临时目录。配对、历史、设置、下载和文件接收均使用该目录。使用专用 iPhone 和 Windows 测试会话，写入前备份两端原剪贴板的全部格式，测试后恢复。自动往返用原剪贴板可验证为空的专用设备。

```powershell
$env:RUSTUP_HOME = 'E:\rust\rustup'
$env:CARGO_HOME = 'E:\rust\cargo'
$env:PATH = "E:\rust\cargo\bin;$env:PATH"
cargo build --workspace --release --locked
cargo build -p copiago-device --release --locked --examples

$fixtureId = [guid]::NewGuid().ToString('N')
$testRoot = Join-Path $env:TEMP ('CopiaGo-Run-QA-' + $fixtureId)
New-Item -ItemType Directory -Path $testRoot | Out-Null
$accountSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
icacls.exe $testRoot /inheritance:r /grant:r "*${accountSid}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F'
if ($LASTEXITCODE -ne 0) { throw '隔离目录权限配置失败' }
$env:ICB_DATA_DIR = $testRoot
$env:ICB_INSTANCE_NAMESPACE = 'run-qa-' + $fixtureId
$env:ICB_DIAGNOSTIC_FILE = Join-Path $testRoot 'diagnostic.log'
```

预期生成 `target/release/copiago.exe` 和探针，`icacls` 处理一个目录且零失败。首次配对使用空目录。复验旧配对时，在同一 Windows 账户把已授权测试手机的 DPAPI 配对记录复制到临时 `device-data`，保留原目录用于读前对比。GUI 与 `--headless` 的双向同步使用同一组临时数据时依次运行。

### 安装和覆盖升级

- [ ] 在 `copiago-rs` 下运行 `./scripts/build.ps1 -IsccPath $isccExe`，将 `$isccExe` 设为本机 ISCC.exe 的完整路径。预期 `target/packages` 生成 x64 安装包和便携 ZIP。用 `Get-Item` 核对安装包不超过 6,000,000 字节，用 `Expand-Archive` 解压到临时目录，确认 EXE 旁有零字节 `portable-data`，运行便携 EXE 的 `--help` 应正常退出。
- [ ] 运行 `$testApp = Join-Path $testRoot 'app'`，再用 `./scripts/build.ps1 -IsccPath $isccExe -TestId $fixtureId -TestInstallDir $testApp` 构建测试安装器。运行 `target/packages/CopiaGo-Test-*.exe` 并选择同一目录。预期错误 `/DIR` 被拒绝，安装后的 `Get-ChildItem -LiteralPath $testApp -Recurse -File | Measure-Object Length -Sum` 不超过 12,000,000 字节，包含卸载器。用同目录的卸载器复核静默卸载后保留测试数据。
- [ ] 在专用测试 Windows 同一账户中，用 Python 1.2.0 完成一次配对并保存两个来源的已知历史及设置。把旧安装副本放进隔离测试安装目录，按上一步用同一 `TestId` 和目录执行覆盖。预期只关闭该目录旧进程，清理旧 EXE 和 `_internal`，保留账户数据。用 `Get-FileHash -Algorithm SHA256` 对比升级前后的隔离文件。已安装的 Rust 使用临时账户数据副本启动，预期无需重新配对即显示「已连接（无线）」并可读两侧历史。正式覆盖与 Run 开关的实际复核在专用测试账户按 [README](README.md#从-python-120-覆盖升级) 执行。
- [ ] 在专用 Windows 测试账户与临时安装目录复验自动迁移，将测试进程的 `LOCALAPPDATA` 指向受保护的临时目录，在其 `iPhoneClipboardPrototype` 放入配对与历史副本。保持安装副本的账户数据为空，仅在专用测试会话设 `$env:LOCALAPPDATA = $testRoot`，执行 `Remove-Item Env:ICB_DATA_DIR,Env:ICB_INSTANCE_NAMESPACE`，再用 `Start-Process -FilePath (Join-Path $testApp 'copiago.exe')` 启动隔离安装版。预期既有设置、历史及配对可直接使用，源目录以 `iPhoneClipboardPrototype.before-rust-upgrade` 保留备份。故意用测试权限使复制失败后，预期继续读取原目录而且无半成品发布。完成后退出测试应用并关闭这个 PowerShell 会话，普通复验另开会话使用隔离环境。

### 初次配对、Wi-Fi 优先与恢复

- [ ] 运行 `./target/release/copiago.exe --minimized` 或 `cargo run --release --locked -p copiago-device --example pairing_probe -- pair`。用测试手机尝试一机拒绝信任、接入两台、再接单台解锁并点「信任」输入锁屏密码。预期分别出现重试引导、只保留一台设备提示、配对完成。拔线后运行 `./target/release/examples/wifi_probe.exe`，预期 `pair_verify=true`。
- [ ] 运行 `./target/release/examples/usb_probe.exe $env:ICB_DATA_DIR`。单台已信任且开发者服务可用时，预期 `services` 大于零且 `pasteboard_advertised=true`。运行 `./target/release/examples/clipboard_probe.exe $env:ICB_DATA_DIR 60`，待 `ready` 后在测试手机复制文字。预期 1 秒内出现 `text_event`，Notes 较慢回复后再复制应恢复。
- [ ] 在授权旧配对副本运行 `./target/release/examples/wifi_probe.exe --write-identifier` 两次。可迁移旧记录预期先 `Written` 再 `AlreadyPresent`。可按 [M1 的人工复验](https://github.com/xgod1862/CopiaGo/pull/10) 用 Python 复读。运行 `./target/release/examples/discovery_probe.exe (Join-Path $env:ICB_DATA_DIR 'device-data')`，重复 8 次，每轮间隔 4 秒。预期同一手机 `matched=true`，正常侦测每轮小于 1 秒。
- [ ] 运行 `./target/release/examples/connection_probe.exe --seconds 180`。Wi-Fi 正常时接线，预期仍为 `connected wireless=true`。断开无线且同一手机仍接线时预期回退 `wireless=false`，恢复无线后断开回退会话应恢复 `wireless=true`。分别实际锁屏解锁、Windows 睡眠唤醒、切换 Wi-Fi 和开关 VPN，再运行 `Get-Content -LiteralPath $env:ICB_DIAGNOSTIC_FILE -Tail 30`，预期恢复 `connected` 并继续同步。
- [ ] 在隔离目录运行 `./target/release/examples/pairing_probe.exe forget`，或在 GUI 「忘记此 iPhone」三秒内确认两次。预期临时配对记录被移除，旧历史保留，回到引导，下一次需要 USB 配对。模拟设备记录被打开且禁共享删除时，预期失败后可重试，实际释放占用后再确认可完成。

### 文字、图片、历史和离线内容

- [ ] 在剪贴板可验证为空的专用手机上，接入 USB 后运行 `cargo test --release --locked -p copiago-device --lib usb::tests::usb_push_round_trip -- --ignored --exact --nocapture`。预期 `text_match`、`png_match`、`restored_empty`。独立 Windows runner 可运行 `cargo test -p copiago-win --locked clipboard::tests::native_clipboard_round_trip -- --ignored --exact --nocapture` 验证系统剪贴板格式。
- [ ] 打开临时设置的双向同步和文字／图片类型，再运行 `./target/release/copiago.exe --headless` 或默认托盘入口。Windows 用 `Set-Clipboard -Value 'CopiaGo QA 中文 😀'`，预期 `push_complete`，手机粘贴一致且电脑无回声。手机复制另一段，Windows 用 `Get-Clipboard -Raw` 核对，预期 1 秒内写入并进入 iPhone 历史。两端各用支持透明 PNG 的应用复制、粘贴一次，核对透明度，结束恢复两端原剪贴板。
- [ ] 依次使用已配对测试机的相册 HEIC、截图 PNG、Safari 图片、Notes 图片和可用时的微信图片。每个样本运行 `./target/release/examples/wifi_probe.exe --pasteboard-types` 记录 UTI，并在无 HEVC 的测试会话实际粘贴。预期手机可提供 PNG/JPEG 时按同一条目定向接收。只有 HEIC 时记录 WIC 解码是否可用。复制视频，运行 `Get-Content -LiteralPath $env:ICB_DIAGNOSTIC_FILE -Tail 15`，预期不支持事件和只同步文字、图片及「文件」App 文件的提示。
- [ ] 运行 `./target/release/copiago.exe`。停用自动推送并启用电脑历史，复制专用样本，预期进入电脑历史。启用推送后在手机断线期间分别测试电脑未复制和电脑随后复制，后者应使较旧手机文字或图片留在「使用 iPhone 上最新复制的内容」。点击此项后用 `Get-Clipboard -Raw` 或相应应用验证。断线时复制小文件的自动接收沿用 Python 的开关和阈值，重连时验证是否覆盖随后复制的 Windows 样本。
- [ ] 运行 `./target/release/copiago.exe`，使用 GUI 的双来源标签、单击展开、双击或复制按钮、电脑项的独立发送按钮、删除与三秒内二次清空。预期仅操作选中条目及来源，关闭保存后仍可读取旧记录，实际历史按来源限额保留。可用 `cargo run --release --locked -p copiago-ui --example history_page -- phone` 先复验合成 UI。设置 `COPIAGO_HISTORY_PYTHON` 为本机 CopiaGo Python、`PYTHONPATH` 指向仓库 `source` 后运行 `cargo test -p copiago-core --locked history::tests::python_history_interoperability -- --ignored --exact`，预期文本、图片和文件的 Python／Rust 合成互通。

### 文件确认、校验与大小边界

- [ ] 按 [METRICS.md](METRICS.md) 的文件压测命令生成 100 和 500 MiB 样本，再把对应样本传到测试手机。手机复制目标后运行 `./target/release/examples/file_bench.exe wifi threshold (Join-Path $samples 'copiago-bench-100MiB.bin')`，预期 `sha256_match=true` 与精确字节。分别更换 `usb`、`resolve` 和两种大小，记录端到端耗时、刷盘和专用工作集。
- [ ] 在隔离设置开启接收与 file 类型，阈值 20 MiB，运行正式 EXE 或 `./target/release/copiago.exe --headless`。在「文件」App 依次复制 64 KiB、1 MiB、25 MiB、512 MiB 及大于 512 MiB 的合成样本。预期前两者自动，25 MiB 与 512 MiB 等待确认，大于上限报错。GUI 点击同步或 headless 输入 `accept <id>`，将接收文件粘贴到测试目录并运行 `Get-FileHash -Algorithm SHA256` 比较。
- [ ] 运行 `./target/release/examples/connection_probe.exe --check-expired-offer --seconds 60`。看到 `first_promised_offer_saved` 后复制第二份，预期旧 offer 为 `offer_expired` 且无旧文件发布。另在确认和忽略的边界快速复制两次，在一个大文件传输中退出应用。预期结果绑定原 offer，退出取消传输，查看临时 `Received` 下没有未完成发布。在该临时目录用合成文件复验 24 h／2 GiB 清理，将测试传输目录的 `LastWriteTimeUtc` 设为 `[DateTime]::UtcNow.AddHours(-25)` 构造过期记录，预期重启后过期文件清掉，近 24 小时超出总额时从旧到新清理。

### 镜像下载、签名和挂载

- [ ] 用一个空临时数据根运行 `cargo run --release --locked -p copiago-ddi --example ddi_download -- cryptex`，再更换为 `personalized`。用 `Get-FileHash -Algorithm SHA256` 对照 Python 的固定清单，预期 8 个文件大小与哈希匹配，重跑热缓存零下载。
- [ ] 重启和解锁专用 iPhone。运行 `./target/release/examples/connection_probe.exe --seconds 15`，预期在未挂载时出现 `developer_services_missing`。运行 `./target/release/examples/connection_probe.exe --install-developer-image --seconds 1500`，预期按需下载、请求 Apple 签名、挂载、单次重拨且可用剪贴板。先按同一 Wi-Fi 验证，之后单独验证 Wi-Fi 不可用时同设备接线。保留两套镜像选择、实际耗时与内存。
- [ ] 在专用手机关闭开发者模式并重启，复跑带安装标志的探针。预期 `state=Developer` 和「设置 → 隐私与安全性 → 开发者模式」。重新开启并按提示重启后复验，通过 `Get-Content -LiteralPath (Join-Path $env:ICB_DATA_DIR 'developer-setup.log') -Tail 20` 核对 download/sign/install、结果及超时。受设备签名／镜像缓存影响的阶段可跳过。

### Windows 托盘、设置与无障碍

- [ ] 运行 `./target/release/copiago.exe --minimized`，再普通启动一次。预期普通重复启动唤起同一 PID，重复 `--minimized` 静默；未配对时首次仍显示引导。在专用 Windows 测试会话，通过任务管理器重启 Explorer，再把任务栏移到可用的另一屏或打开溢出托盘。预期图标恢复，弹窗保持在图标所在工作区。用 `Start-Process 'ms-settings:display'` 和 `Start-Process 'ms-settings:colors'` 切换显示缩放与主题，预期重新定位、字体和主题正常。检查右键菜单、气泡及 Windows 10 图标字体。可用 `cargo test --release --locked -p copiago-ui --lib live_shell_anchor_and_secondary_window -- --ignored --nocapture` 先复验已有原生定位探针。使用支持相应位置的环境测试任务栏四边。
- [ ] 运行 `cargo run --release --locked -p copiago-ui --example gallery -- --probe --position 4634 840`，将位置替换为测试副屏坐标，追加 `--reduced-motion` 对照。预期滚动和重排命中正确、动画结束清理视觉树。实际触控板和 Windows 10 图标字体可用同一示例复验。
- [ ] 运行 `cargo run --release --locked -p copiago-ui --example keyboard_accessibility` 或打开正式历史／设置页。使用 Tab / Shift+Tab、Enter / Space，预期焦点可见、执行同一动作。在测试会话用 `Win+Ctrl+Enter` 启用讲述人，预期能朗读按钮、开关和列表。
- [ ] 在临时目录运行默认 EXE，切换所有设置并重启。用 `Get-Content -LiteralPath (Join-Path $env:ICB_DATA_DIR 'settings.json')` 核对，预期立即切换中英，持久保存字段且未知键保留。实际登录启动用专用测试账户，执行 `Get-ItemPropertyValue -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name iPhoneClipboardPrototype`，预期带引号的路径及 `--minimized`，禁用后对应值移除。
- [ ] 运行 `cargo run --release --locked -p copiago-win --example update_probe -- --github 1.1.0 --open`，预期正式 GitHub 发布端点报告较新的稳定版并打开本项目发布页；用 `2.0.0-beta` 复跑预期无提示。关闭更新开关预期无请求，重新开启立即检查。

### 正式版冷启、内存和 24 小时稳定性

- [ ] 在隔离目录准备已配对且开发者剪贴板服务可用的专用 iPhone，保持解锁且同一局域网。先退出现有测试实例。执行下方探针，预期可见 `event=connected transport=wifi`，并输出从进程启动到观察到连接的毫秒数。每次通过托盘退出后重新执行，共 5 次，目标单次 ≤ 3,000 ms。另记 `event=ready` 时间作为托盘准备时间。

```powershell
$exe = (Resolve-Path './target/release/copiago.exe').Path
$env:ICB_DIAGNOSTIC_FILE = Join-Path $testRoot ('cold-' + [guid]::NewGuid().ToString('N') + '.log')
$process = Start-Process -FilePath $exe -ArgumentList '--minimized' -PassThru
$startedUtc = $process.StartTime.ToUniversalTime()
do {
    $found = (Test-Path -LiteralPath $env:ICB_DIAGNOSTIC_FILE) -and
        (Select-String -Quiet -LiteralPath $env:ICB_DIAGNOSTIC_FILE -Pattern 'event=connected transport=wifi')
    if ($found) { break }
    Start-Sleep -Milliseconds 25
} while (!$process.HasExited -and ([DateTime]::UtcNow - $startedUtc).TotalSeconds -lt 30)
if ($found) { ([DateTime]::UtcNow - $startedUtc).TotalMilliseconds } else { '未连上' }
```

- [ ] 按 [METRICS.md](METRICS.md) 的 GUI 与计数器命令，在托盘、历史、设置和再次收起状态对同 PID 采样。预期托盘空闲不超过 10,000,000 B，打开弹窗不超过 20,000,000 B，记录 Private Bytes 和线程数。
- [ ] 测 24 小时时用同一已连接实例，开启接收和元数据侦测，关闭自动推送、通知和更新，保持测试手机不复制。确认 `event=connected transport=wifi` 后，使用同一 PowerShell 会话和上一步的进程 PID，运行下方采样。预期有 25 个相隔约一小时的样本，测试进程全天持续运行，专用工作集增长小于 2,000,000 B。空闲 CPU 以单个逻辑处理器计低于 0.1%，可以先报告含元数据轮询的上界。

```powershell
$testPid = $process.Id
$paths = '\Process(copiago*)\ID Process', '\Process(copiago*)\Working Set - Private', '\Process(copiago*)\Private Bytes', '\Process(copiago*)\Thread Count'
Get-Counter -Counter $paths -SampleInterval 3600 -MaxSamples 25 | ForEach-Object {
    $sample = $_
    $instance = $sample.CounterSamples | Where-Object {
        $_.Path -like '*\id process' -and [int]$_.CookedValue -eq $testPid
    } | Select-Object -First 1
    if (!$instance) { throw '测试进程已退出或采样丢失' }
    $values = $sample.CounterSamples | Where-Object InstanceName -eq $instance.InstanceName
    [pscustomobject]@{
        UTC = $sample.Timestamp.ToUniversalTime().ToString('o')
        PID = $testPid
        WorkingSetPrivate = [long]($values | Where-Object Path -like '*\working set - private').CookedValue
        PrivateBytes = [long]($values | Where-Object Path -like '*\private bytes').CookedValue
        Threads = [int]($values | Where-Object Path -like '*\thread count').CookedValue
        CpuSeconds = (Get-Process -Id $testPid).TotalProcessorTime.TotalSeconds
    }
} | Export-Csv (Join-Path $env:ICB_DATA_DIR 'stability-24h.csv') -NoTypeInformation
```

用首尾两个样本的 `CpuSeconds` 差值除以实际采样间隔秒数再乘 100 得到包含轮询的平均 CPU。将绝对内存变化、每小时趋势和诊断中的重连记录一并检查。完整样本中断时另记原因。


### 构建与旧版数据的附加复核

- [ ] 在 `copiago-rs` 执行 `cargo fmt --check`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 和 `cargo deny --locked check licenses`。预期全绿。GPL 拒绝门禁按 M0 PR 在临时 workspace 副本引入 GPL 测试依赖复验，预期 `error[rejected]`。 [M0 工程验收](https://github.com/xgod1862/CopiaGo/pull/9)
- [ ] 执行 `(Get-Item ./target/release/copiago.exe).VersionInfo`，预期产品为 `CopiaGo`，版本 `2.0.0-beta`。在可运行 `mt.exe` 的 Windows SDK 终端用 `mt.exe -nologo '-inputresource:target\release\copiago.exe;#1' '-out:target\copiago-extracted.manifest'`，再用 `mt.exe -nologo -manifest target\copiago-extracted.manifest -validate_manifest`，预期解析成功。 [资源验收](https://github.com/xgod1862/CopiaGo/pull/8)
- [ ] 对当前 Windows 账户的只读旧设置或其副本，执行 `cargo test -p copiago-core --lib settings::tests::reads_installed_python_settings -- --ignored --exact`，预期 1 项通过。执行 `cargo test -p copiago-core icacls_confirms_only_user_and_system_and_inheritance` 与 `cargo test -p copiago-core reads_python_compatible_powershell_fixture`，预期 ACL 和 DPAPI 的示例复验通过。 [数据路径验收](https://github.com/xgod1862/CopiaGo/pull/9)

## 已知差距与后续工作

- 台账无「阻塞」任务。先让测试手机在同一局域网完成 Wi-Fi 冷启和覆盖升级后的无重配连接，再按清单验证双向内容、真实 USB 回退、镜像、睡眠恢复和 24 小时运行。 [M5 真机探针](METRICS.md)
- 48 MP 基线 JPEG 缩略图和透明度通过。渐进 JPEG 的独立 Debug 探针三次超过 30 MB，采样专用工作集最高 146,251,776 B。应复现正式 EXE 端到端峰值并控制缩略图开销。仅有 HEIC 的样本在本机返回 `0xc00d5212`，先核对真实来源能否提供同条目的 PNG/JPEG，剩余样本再评估解码支持。 [图片问题清单](METRICS.md) · [M2](https://github.com/xgod1862/CopiaGo/pull/11)
- 断线期间复制的小文件在重连时仍按 Python 的开关与大小阈值自动接收，可能覆盖随后在 Windows 复制的内容。更新检查同样沿用 Python 稳定版本规则，`2.0.0-beta` 无更新提示。 [文件验收](https://github.com/xgod1862/CopiaGo/pull/11) · [更新验收](https://github.com/xgod1862/CopiaGo/pull/13)
- 旧开发布局迁移后保留当前账户的来源备份，失败时回到旧目录。x64 候选包为未签名本地构建，分发前可接入预留签名参数并补齐 Rust 传递依赖的许可证声明。 [安装与签名](README.md) · [构建脚本](scripts/build.ps1)
