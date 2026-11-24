# CopiaGo Rust 版

## 构建

在 Windows 上使用 Rust 1.100.0、MSVC Build Tools 和 Windows SDK。在仓库根目录运行以下 PowerShell 命令。

```powershell
$env:RUSTUP_HOME = 'E:\rust\rustup'
$env:CARGO_HOME = 'E:\rust\cargo'
$env:PATH = "$env:CARGO_HOME\bin;$env:PATH"
Set-Location copiago-rs
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release
```

产物位于 `target/release/copiago.exe`。

默认 MSVC 构建静态链接 C 运行库，安装版和便携版均无需另外安装 Visual C++ Redistributable。

## 安装包与便携版

在仓库根目录运行以下命令。使用 Inno Setup 6.5 或更新的 6.x 版本，按本机路径设置 `-IsccPath`。

```powershell
.\copiago-rs\scripts\build.ps1 -IsccPath 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

默认在 `copiago-rs\target\packages` 输出 x64 安装包和便携 ZIP。可用 `-OutputDir` 指定输出目录。安装包默认装到 `%LOCALAPPDATA%\Programs\CopiaGo`，便携版解压后在 `copiago.exe` 旁保留 `portable-data` 文件。两个包的版本取自同一个 Release EXE。`-SignToolCommand` 接收 Inno 签名工具命令模板，后续可由维护者配置证书。模板用 `$f` 指定待签文件。

本机复验时，在 `%TEMP%` 下选用独立路径，传入 `-TestId` 和 `-TestInstallDir` 生成测试安装器。复测同一次升级时，两次编译使用同一测试 ID 和路径。Inno 脚本直接编译时对应 `/DTestId=<32 位十六进制 GUID>` 和 `/DTestDir=<绝对路径>`。测试包使用独立应用 ID 和登录启动测试键，省略开始菜单快捷方式和安装后启动。

```powershell
$testId = [guid]::NewGuid().ToString('N')
$testDir = Join-Path $env:TEMP "CopiaGo-$testId\app"
.\copiago-rs\scripts\build.ps1 -IsccPath 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe' -TestId $testId -TestInstallDir $testDir
```

运行生成的 `CopiaGo-Test-*.exe` 后，按 `Get-ChildItem -LiteralPath $testDir -File -Recurse | Measure-Object Length -Sum` 的 `Sum` 核对安装占用，包含卸载器，预算 12,000,000 字节。测试应用在独立 `ICB_DATA_DIR` 和 `ICB_INSTANCE_NAMESPACE` 下启动。

### 从 Python 1.2.0 覆盖升级

在原 Windows 账户下运行 Rust 安装包。原来装在默认目录 `%LOCALAPPDATA%\Programs\iPhoneClipboard` 的，改装到 `%LOCALAPPDATA%\Programs\CopiaGo`：安装器关闭旧目录中的程序，把 `data` 整体移到新目录，清理 `iPhoneClipboard.exe`、`_internal` 和旧卸载器，旧目录清空后删除。用户自选的安装目录保持不变，就地升级并清理 Python 程序文件。登录启动选项默认采用该账户当前的开关状态，并把旧的 `iPhoneClipboardPrototype` 启动项换成 `CopiaGo`。

如果 Python 仍在使用 `%LOCALAPPDATA%\iPhoneClipboardPrototype`，且当前账户的安装目录数据为空，Rust 首次启动时会迁移设置、历史和配对记录，并尝试把源目录保留为 `iPhoneClipboardPrototype.before-rust-upgrade` 备份。迁移失败时继续使用原数据目录。卸载时默认保留数据，选择同时删除数据会清理当前账户的备份。

真机复验时，在测试电脑的同一账户用 Python 1.2.0 完成配对并保存设置和两侧历史，保持手机解锁、拔掉 USB，并确认电脑与手机处于同一局域网。从仓库根目录启动已构建的安装器。

```powershell
Start-Process -FilePath '.\copiago-rs\target\packages\CopiaGo-Setup-1.3.1-x64.exe' -ArgumentList ('/LOG="{0}"' -f (Join-Path $env:TEMP 'CopiaGo-upgrade.log')) -Wait
```

安装后首次打开 CopiaGo，确认直接显示「已连接（无线）」且未出现配对引导。核对 iPhone 和电脑两侧的历史、设置及登录启动开关，再确认旧程序和运行时已清理。

## USB 配对真机复验

在 `copiago-rs` 中选用专用测试 iPhone 和测试电脑，确认测试机运行 Apple Mobile Device Service，并为每个场景指定新的临时数据目录。CopiaGo 在初次写入时为目录设置仅当前用户和 SYSTEM 可访问的 ACL。`ICB_DATA_DIR` 隔离 CopiaGo 文件，首次信任和无线开关仍作用于测试设备。

```powershell
$env:ICB_DATA_DIR = Join-Path $env:TEMP ('copiago-pair-' + [guid]::NewGuid().ToString('N'))
cargo run --release -p copiago-device --example pairing_probe -- pair
```

- 接入两台测试 iPhone，再运行 `pair`，应提示「请暂时只连接一台 iPhone」。
- 在从未信任测试电脑的单台 iPhone 上点「不信任」，应提示重新插拔并选择「信任」。保持测试手机解锁并重新运行 `pair`，点「信任」并输入锁屏密码。
- 配对输出 `pairing_saved=true` 后拔线，保持在同一 Wi‑Fi 网络。运行 `cargo run --release -p copiago-device --example wifi_probe`，应输出 `pair_verify=true`、RSD 服务数量和 `identifier_migration=AlreadyPresent`。
- 在同一临时目录运行 `cargo run --release -p copiago-device --example pairing_probe -- forget`，应回到 `Setup`，目录里的 `remote_*.plist` 和 `device-info.bin` 应消失。再次运行 `wifi_probe` 应报告没有 RemotePairing 记录，再接 USB 运行 `pair` 可重新配对。

## 目录

| Crate | 职责 |
| --- | --- |
| `copiago-app` | 可执行程序、Windows 资源和各模块的组装 |
| `copiago-core` | 数据模型、设置、持久化和国际化 |
| `copiago-device` | iPhone 配对、连接和剪贴板同步 |
| `copiago-ddi` | 开发者镜像下载、校验、签名和挂载 |
| `copiago-win` | Windows 剪贴板、托盘、系统集成和 HTTP |
| `copiago-ui` | 弹窗、动画、历史和设置界面 |

`copiago-app` 负责组装其他模块。`copiago-device`、`copiago-ddi`、`copiago-ui` 和 `copiago-win` 共用 `copiago-core` 的类型和逻辑。内部依赖在首次使用时声明。

需求见 [REQUIREMENTS.md](../docs/rust-rewrite/REQUIREMENTS.md)，任务见 [TASKS.md](../docs/rust-rewrite/TASKS.md)，测试数据记录在 [METRICS.md](METRICS.md)。
