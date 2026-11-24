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
