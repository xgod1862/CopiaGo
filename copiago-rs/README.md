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
