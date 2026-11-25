# CopiaGo

[English](README.md) · **简体中文**

在 iPhone 上复制，在 Windows 上直接粘贴。首次用 USB 配对，之后在同一局域网内无线同步。iPhone 无需安装 App。

**[下载安装包（Windows x64）](https://github.com/xgod1862/CopiaGo/releases/latest)**

## 系统要求

| | 要求 |
| --- | --- |
| iPhone | **iOS 17 或更高版本**（即 iPhone XS / XR 及之后的机型）。需开启开发者模式，CopiaGo 会引导开启；「设置」里还没有这个开关时，用数据线连接后 CopiaGo 会让它出现，开启和重启仍需在手机上操作。**iOS 16 及更早版本不支持**，配对时会直接提示升级。 |
| Windows | **Windows 10 21H2（版本 19044）或更高版本，含 Windows 11**，x64。需要 Apple 的设备连接组件（Apple Mobile Device Support）。缺少时可在 CopiaGo 中点「安装连接组件」，通过 Windows 包管理器（winget）安装；没有 winget 或 Microsoft Store 的电脑（如 LTSC、精简版系统）可点「手动下载安装」，从 Microsoft Store 获取 [Apple Devices](https://support.apple.com/zh-cn/118290)，或从 apple.com 下载 iTunes。 |
| 网络 | 首次配对需要数据线。之后无线同步要求电脑和 iPhone 在可互相发现的同一局域网内；电脑上的 VPN 或代理「TUN 模式」可能拦截局域网连接。首次准备开发者服务时需联网下载约 18 MB 的组件并请求 Apple 签名。 |

## 体积与占用

1.3.0 用 Rust 重写，与 Python 版 1.2.0 对比：

| 项目 | 1.3.0 | 1.2.0（Python） |
| --- | ---: | ---: |
| 安装包 | 3.7 MB | 25 MB |
| 便携版 ZIP | 1.9 MB | — |
| 安装后占用 | 7.9 MB（程序本身 3.4 MB，其余为卸载程序） | 74 MB |
| 托盘待机内存 | 约 3.5 MB | 约 71 MB |
| 打开面板时内存 | 约 10–12 MB | — |
| 待机 CPU | 约 0.1%（单个逻辑核心） | — |
| 启动到无线连上 | 约 2 秒 | — |

内存为进程专用内存（私有提交），在 Windows 11 x64、真实 iPhone 无线连接下测得；面板关闭后会归还内存，任务管理器中约 3 MB。开发者组件约 18 MB，首次连接时下载到数据目录，不计入安装后占用。

## 快速开始

1. 在 Windows 运行安装包。
2. 用数据线连接并解锁 iPhone。按手机提示点「信任」，按 CopiaGo 提示开启 [开发者模式](https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device)。
3. 在 CopiaGo 中点击「连接 iPhone」。配对完成后拔线，等托盘面板显示「已连接（无线）」。
4. 在 iPhone 上复制一段文字，在 Windows 应用中按 `Ctrl+V`。

安装器和 CopiaGo 默认按 Windows 显示语言选择简体中文或英文。在应用的「设置 → 语言」可改为手动选择，也可恢复「跟随系统」。

### 连接遇到问题

每个步骤失败时，CopiaGo 都会显示原因和下一步该做什么。下面这些问题可以在应用内直接处理：

- **Apple 设备连接服务没有运行、被禁用或没有响应**：点「修复连接服务」。CopiaGo 会请求管理员授权，启动或重启 Apple Mobile Device Service，并把它恢复为随 Windows 自动启动（Apple 的默认设置）。如果端口 27015 被其他程序占用，会提示是哪个程序。
- **电脑检测到了 iPhone，但 Apple USB 驱动没有工作**：点「修复 Apple 驱动」。CopiaGo 会请求管理员授权，用系统自带的 `pnputil` 重新安装 Apple 组件自带的 USB 驱动。
- **电脑没有检测到 iPhone**：多半是只能充电的数据线或接口问题，请换线或直接插在电脑机身的 USB 口。

仍然连不上时，点引导页或设置页里的「复制诊断信息」，把内容发给开发者；或点「反馈问题」，CopiaGo 会复制诊断信息并打开预填模板的 GitHub Issue（需要 GitHub 账号），在问题描述中粘贴即可。

### 从旧版升级

直接运行新安装包即可，配对记录、历史和设置都会保留，无需重新配对。原来装在默认目录 `%LOCALAPPDATA%\Programs\iPhoneClipboard` 的，会自动移到 `%LOCALAPPDATA%\Programs\CopiaGo`；自选的安装目录保持不变。

## 界面截图

| 首次配对 | 托盘弹窗 | 剪贴板历史 |
| --- | --- | --- |
| [![CopiaGo 首次配对引导](docs/images/first-pairing.png)](docs/images/first-pairing.png) | [![CopiaGo 的托盘弹窗，历史记录处于关闭状态](docs/images/tray-popup.png)](docs/images/tray-popup.png) | [![CopiaGo 剪贴板中的文字、图片和文件记录](docs/images/clipboard-history.png)](docs/images/clipboard-history.png) |

截图来自 CopiaGo 1.3.0，连接状态和历史记录使用示例数据。

## 同步范围

| 方向 | 内容 | 默认行为 |
| --- | --- | --- |
| iPhone → Windows | 文字、图片、「文件」App 中复制的文件 | 自动同步，大文件先询问 |
| Windows → iPhone | 文字、图片 | 在设置里开启自动同步，或从电脑历史手动发送 |

- 双端历史默认关闭，开启后可在 Windows 托盘面板查看、复制、发送和清除。
- 能否粘贴图片取决于来源应用提供的剪贴板内容。若应用只提供文字或文件名，可先保存到「照片」再复制。照片 App 中的视频复制暂不支持。
- 手机离开网络后，CopiaGo 会在它重新上线时自动重连，无需打开面板。

## 隐私

- 无线传输走已配对设备的加密隧道。CopiaGo 不使用云端中转剪贴板。
- 配对记录及启用后的历史正文与预览由 [Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata) 保护。接收的文件会暂存在数据目录的 `received` 文件夹。
- 仅在可信的 Windows 电脑上配对。停用时可在托盘设置中选「忘记此 iPhone」。
- 每次启动约 15 秒后，CopiaGo 会向 GitHub 查询一次最新版本，有新版时在托盘面板底部提示，不会自动下载或安装。GitHub 会看到这台电脑的 IP 地址和 CopiaGo 版本号。可在设置中关闭「启动时检查新版本」。
- 首次准备开发者服务时，CopiaGo 依次尝试 raw.githubusercontent.com、ghfast.top（第三方 GitHub 加速镜像）和 fastly.jsdelivr.net 下载开发者组件，每个文件都按固定的 SHA-256 校验；随后向 Apple 的签名服务器请求签名。这些服务会看到这台电脑的 IP 地址。
- CopiaGo 会在数据目录写入诊断日志 `diagnostic.log`（超过 512 KB 时轮换，最多保留约 1 MB），只记录事件类型、阶段、错误类别和数值，不记录剪贴板内容、文件名、iPhone 名称、序列号或文件路径。日志只保存在本机，只有你点「复制诊断信息」或「反馈问题」时才会复制到剪贴板，由你决定是否发送。复制的内容另含 Windows 和 CopiaGo 版本、iPhone 机型和 iOS 版本，发送前可以先检查。
- 需要管理员授权的操作只在你点击对应按钮、或点「连接 iPhone」而只差 Apple 服务（被禁用、未运行或无响应）这一步时发生，并且都由 Windows 弹窗确认：安装 Apple 组件（通过 winget，会代你接受 Apple 的许可协议）、启动 Apple 设备连接服务并恢复自动启动、重新安装 Apple USB 驱动。
- 开发者模式会放宽 iPhone 的部分安全限制，使用 CopiaGo 期间需要保持开启；不再使用时，可以在 iPhone「设置 → 隐私与安全性 → 开发者模式」中关闭。
- 卸载时会询问是否删除数据目录，包括配对记录、历史、设置和诊断日志。

## 从源码构建

CopiaGo 使用 Rust 编写，源码在 [`copiago-rs`](copiago-rs)。安装 [rustup](https://rustup.rs/) 后，工具链版本由 `rust-toolchain.toml` 自动选择。

```powershell
Set-Location copiago-rs
cargo test --workspace --locked
cargo build --release --locked
```

打包安装程序和便携版另需 [Inno Setup 6](https://jrsoftware.org/isinfo.php)，在仓库根目录运行：

```powershell
.\copiago-rs\scripts\build.ps1 -IsccPath 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

产物位于 `copiago-rs\target\packages`。更多构建、测试和升级验收说明见 [copiago-rs/README.md](copiago-rs/README.md)。1.2.0 及更早的 Python 版本代码保留在 git 历史和 [`v1.2.0`](https://github.com/xgod1862/CopiaGo/tree/v1.2.0) 标签中。

## 反馈与许可

在应用内点「反馈问题」，或直接通过 [GitHub Issues](https://github.com/xgod1862/CopiaGo/issues/new) 反馈。请附上「复制诊断信息」得到的内容和复现步骤；不要附上配对记录文件、剪贴板内容或其他个人信息。

项目采用 [GPL-3.0-or-later](LICENSE)。随安装包分发的第三方许可声明会安装到程序目录的 `licenses` 文件夹。感谢 [idevice](https://github.com/jkcoxson/idevice)、[pymobiledevice3](https://github.com/doronz88/pymobiledevice3)、[Relais](https://github.com/williamcachamwri/Relais) 和 [Inno Setup 简体中文翻译](https://github.com/kira-96/Inno-Setup-Chinese-Simplified-Translation)。
