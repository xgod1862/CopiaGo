# PR #23 Windows 真机测试手册（供 Codex 执行）

适用代码：`main` 上合并 PR #23 及其后续修复（headless 模式下载不覆盖更新内容）之后的版本。

你是在一台 Windows 电脑上执行测试的 Codex。现场只有**一台 Windows 电脑和一台已经和它配对过的 iPhone**，没有第二台设备。目标是验证 PR #23 的改动在真机上的行为：

- 各类失败提示：`Unreachable`、读取超时、发送失败、写入剪贴板失败、类型过滤
- 剪贴板读、写、文件下载三条通道并行，互不阻塞
- 被并行写入取代的旧读取结果会被丢弃
- 文件下载完成时，如果剪贴板里已有更新的内容，不再覆盖
- 相册 HEIC 照片的读取路径

## 0. 原则（必须遵守）

1. **尽量自动完成，减少找用户的次数。** 能用 PowerShell、诊断日志或剪贴板读写完成的判定，就不要问用户。全程计划只需要用户协助 **3 次**（见第 2 节），确实无法继续时才额外求助。每次求助都要一次说清全部操作，不要零碎地问。
2. **判定以证据为准。** 每个用例都记录 `diagnostic.log` 中的相关行和剪贴板读取结果。结论分成"通过 / 失败 / 无法判定"三种，不确定的不要写成通过。
3. **不碰用户数据。** 使用隔离的数据目录（第 1.3 节）。不要删除、修改用户原有的 CopiaGo 数据，不要卸载或安装任何系统组件（包括 HEIF/HEVC 扩展）。测试中照片只在本机内存里流转，报告里只写类型、尺寸和字节数，不写剪贴板的文字内容，也不附照片。
4. **网络改动必须恢复。** 第 1.5 节的隔离模拟会修改 ARP 表和网卡绑定，**无论测试成功还是失败，结束前都要执行恢复步骤**，并在报告里贴出恢复后的检查结果。
5. 需要管理员权限的命令（第 1.5 节）请在管理员 PowerShell 中执行。如果当前会话没有管理员权限，就在第一次协助时一并请用户提供。

## 1. Codex 自动准备

### 1.1 构建与单元测试

```powershell
cd <仓库根目录>\copiago-rs
git switch main; git pull --ff-only
git log --oneline -3          # 应包含合并 PR #23（69ab27b）及 headless 下载修复的提交
cargo test --workspace --locked
cargo build --release --locked -p copiago-app
```

`cargo test` 失败时停止，报告失败的用例名和断言输出，不要继续真机测试。

### 1.2 环境信息（只读，记录到报告）

```powershell
Get-ComputerInfo | Select-Object WindowsProductName, OsBuildNumber
Get-AppxPackage *HEIFImageExtension*, *HEVCVideoExtension* | Select-Object Name, Version
Get-NetAdapter | Where-Object Status -eq Up | Select-Object Name, InterfaceDescription
Get-Process copiago -ErrorAction SilentlyContinue | Select-Object Id, Path
```

记下两个 HEIC 扩展是否已安装，第 3 节的预期结果取决于此。

### 1.3 隔离的数据目录（沿用现有配对）

正式安装版的数据通常在 `%LOCALAPPDATA%\Programs\CopiaGo\data\<账户名>`。配对记录是这个目录下 `device-data` 子目录里的 `remote_*.plist` 文件，用当前 Windows 账户的 DPAPI 加密，同一账户下复制后仍可使用。程序只在 `<数据根目录>\device-data` 里找配对记录，所以复制时必须保留这一层。

```powershell
# $src 是 ...\<账户名>\device-data
$src = (Get-ChildItem "$env:LOCALAPPDATA\Programs\CopiaGo\data" -Recurse -Filter "remote_*.plist" -ErrorAction SilentlyContinue | Select-Object -First 1).DirectoryName
$qa = "$env:TEMP\copiago-qa"
Remove-Item $qa -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $qa\data | Out-Null
Copy-Item $src "$qa\data\device-data" -Recurse
Get-ChildItem "$qa\data\device-data" -Filter "remote_*.plist"   # 必须能列出至少一个文件
```

如果找不到 `remote_*.plist`，说明这台电脑还没有可用的配对。这时在第一次协助时请用户用数据线连接 iPhone，然后用 GUI 完成配对：点「连接 iPhone」，在手机上点「信任」并输入密码。

写入测试用的设置（程序启动时读取；修改设置后要重启程序才生效）：

```powershell
function Set-QaSettings([string[]]$Pull = @('text','image','file'), [string[]]$Push = @('text','image')) {
  @{ pull = $true; push = $true; history = $true; pc_history = $true; file_ask_mb = 0;
     notifications = $false; update_check = $false;
     pull_types = $Pull; push_types = $Push } | ConvertTo-Json |
    ForEach-Object { [IO.File]::WriteAllText("$qa\data\settings.json", $_, (New-Object Text.UTF8Encoding $false)) }
  # 不要用 Set-Content -Encoding UTF8：PowerShell 5.1 会写入 BOM
}
Set-QaSettings
```

说明：`file_ask_mb = 0` 表示文件直接自动接收，不弹确认；`notifications = $false` 用来避免系统通知干扰截图。

### 1.4 启动与停止被测程序

**主流程使用 GUI 模式**，因为提示文字和历史相关逻辑只在 GUI 模式里有。headless 模式只在第 3.5 节单独补测下载不覆盖的行为。

先退出用户正在运行的正式版 CopiaGo，以免两个程序同时连接同一台手机：

```powershell
$installed = (Get-Process copiago -ErrorAction SilentlyContinue).Path | Select-Object -First 1
Get-Process copiago -ErrorAction SilentlyContinue | Stop-Process -Force

function Start-Qa { $env:ICB_DATA_DIR = "$qa\data"; Start-Process "<仓库根目录>\copiago-rs\target\release\copiago.exe" -ArgumentList '--minimized' }
function Stop-Qa  { Get-Process copiago -ErrorAction SilentlyContinue | Stop-Process -Force }
function Show-Qa  { $env:ICB_DATA_DIR = "$qa\data"; Start-Process "<仓库根目录>\copiago-rs\target\release\copiago.exe" }  # 程序已在运行时，再启动一次会打开剪贴板面板，用于截图
```

测试结束后，用 `$installed` 记下的路径重新启动用户的正式版。

### 1.5 网络隔离模拟（强制走 USB，同时验证 `Unreachable`）

插着数据线时，程序会先尝试 Wi-Fi，失败后才回退到 USB。为了在不拔线的前提下分别测试两种传输方式，用下面的方法模拟校园网的 AP 隔离：手机的 mDNS 广播仍能被电脑收到，但电脑发往手机的单播包被静默丢弃。

需要两项准备：手机的 Wi-Fi IPv4 地址（第一次协助时向用户索取），以及管理员 PowerShell。

```powershell
$wifi  = '<Wi-Fi 网卡名，例如 WLAN>'
$phone = '<iPhone IPv4，例如 10.12.34.56>'

function Enable-Isolation {
  # 静态 ARP 指向一个不存在的 MAC：发往手机的 IPv4 包被静默丢弃（不会收到 RST，表现为超时）
  New-NetNeighbor -InterfaceAlias $wifi -IPAddress $phone -LinkLayerAddress '02-00-5e-00-00-01' -State Permanent | Out-Null
  # 手机还会通过 IPv6 link-local 地址提供服务；临时关闭 IPv6，避免绕过隔离
  Disable-NetAdapterBinding -Name $wifi -ComponentID ms_tcpip6
}
function Disable-Isolation {
  Remove-NetNeighbor -InterfaceAlias $wifi -IPAddress $phone -Confirm:$false -ErrorAction SilentlyContinue
  Enable-NetAdapterBinding -Name $wifi -ComponentID ms_tcpip6
}
function Show-Isolation {
  Get-NetNeighbor -InterfaceAlias $wifi -IPAddress $phone -ErrorAction SilentlyContinue
  Get-NetAdapterBinding -Name $wifi -ComponentID ms_tcpip6 | Select-Object Enabled
}
```

**不要**用防火墙出站规则来模拟。被防火墙拦截的连接会立刻返回 `WSAEACCES`，程序会把它归类为 `Connect`（手机拒绝连接），不是 AP 隔离对应的 `Unreachable`，测出来的结论会是错的。

### 1.6 辅助函数

所有剪贴板操作都要在 **STA** 线程里执行：Windows PowerShell 5.1 默认就是 STA；如果用 `pwsh`，请改用 `powershell.exe -STA` 执行。

```powershell
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$log = "$qa\data\diagnostic.log"

function Get-ClipText { try { [System.Windows.Forms.Clipboard]::GetText() } catch { $null } }
function Get-ClipImageSize { $i = [System.Windows.Forms.Clipboard]::GetImage(); if ($i) { "$($i.Width)x$($i.Height)" } }
function Set-ClipText([string]$t) { [System.Windows.Forms.Clipboard]::SetText($t) }

# 生成测试 PNG。-Noise 生成随机噪点（几乎无法压缩），用来控制文件大小；默认生成带半透明区域的图
function New-TestPng([string]$Path, [int]$W, [int]$H, [switch]$Noise) {
  $b = New-Object System.Drawing.Bitmap $W, $H, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  if ($Noise) {
    $rect = New-Object System.Drawing.Rectangle 0, 0, $W, $H
    $data = $b.LockBits($rect, 'WriteOnly', $b.PixelFormat)
    $bytes = New-Object byte[] ($data.Stride * $H); (New-Object Random 42).NextBytes($bytes)
    [Runtime.InteropServices.Marshal]::Copy($bytes, 0, $data.Scan0, $bytes.Length); $b.UnlockBits($data)
  } else {
    $g = [System.Drawing.Graphics]::FromImage($b); $g.Clear([System.Drawing.Color]::FromArgb(128, 30, 144, 255)); $g.Dispose()
  }
  $b.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose(); (Get-Item $Path).Length
}

# 同时放入 "PNG" 格式和位图格式，和常见的截图工具一致
function Set-ClipPng([string]$Path) {
  $o = New-Object System.Windows.Forms.DataObject
  $o.SetData('PNG', $false, (New-Object IO.MemoryStream (,[IO.File]::ReadAllBytes($Path))))
  $o.SetImage([System.Drawing.Image]::FromFile($Path))
  [System.Windows.Forms.Clipboard]::SetDataObject($o, $true)
}

# 等待诊断日志在 $Since 行之后出现匹配的行，返回匹配行
function Wait-Log([string]$Pattern, [int]$Since, [int]$TimeoutSec = 30) {
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    $hit = Get-Content $log -ErrorAction SilentlyContinue | Select-Object -Skip $Since | Select-String -Pattern $Pattern | Select-Object -First 1
    if ($hit) { return $hit.Line }; Start-Sleep -Milliseconds 300
  }
}
function Log-Mark { (Get-Content $log -ErrorAction SilentlyContinue | Measure-Object -Line).Lines }

# 等待 Windows 剪贴板出现指定文字（用于识别手机快捷指令发来的标记）
function Wait-ClipText([string]$Prefix, [int]$TimeoutSec = 180) {
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) { $t = Get-ClipText; if ($t -and $t.StartsWith($Prefix)) { return $t }; Start-Sleep -Milliseconds 300 }
}
```

诊断日志每行的格式是 `<UTC 时间> elapsed_ms=<毫秒> event=<名称> ...`，其中 `elapsed_ms` 可以用来计算耗时。这份日志不记录剪贴板内容；要核对内容，读剪贴板。

## 2. 需要用户协助的 3 个时间点

开始第 3 节之前，**一次性**把第 2.1 节发给用户，等用户确认全部完成。

### 2.1 协助 1：一次性准备（约 10 分钟）

请用户依次完成：

1. iPhone 用数据线连接这台电脑，并和电脑连在同一个 Wi-Fi；保持解锁。
2. 在「设置 → 显示与亮度 → 自动锁定」中选「永不」（测试结束后改回去），避免中途锁屏。
3. 在「设置 → 无线局域网 → 当前网络 ⓘ」中查看「IP 地址」，告诉 Codex。
4. 在「设置 → 快捷指令」中，把「允许从其他 App 粘贴」设为「允许」（不同 iOS 版本的入口可能在「设置 → App → 快捷指令 → 从其他 App 粘贴」）。
5. 先截一张图（随便截一张），保证"最新截图"存在。确认相册里最新的一张是用相机拍的照片（HEIC 格式；如果没有，用相机随手拍一张）。
6. 在「快捷指令」App 里新建**两个**快捷指令，内容见第 2.3 节。
7. 先运行一次「QA 生成压缩包」，确认「文件 → 我的 iPhone → CopiaGo-QA」里生成了 `qa.zip`。
8. 如果 Codex 没有管理员权限：请用户以管理员身份重新打开 Codex 所在的终端，或者在 Codex 需要时批准 UAC 提示。

### 2.2 协助 2、3：各点一次快捷指令

- **协助 2（USB 阶段）**：Codex 完成第 3.1 节的准备后，请用户在手机上运行「CopiaGo QA」一次，然后不要再动手机，直到 Codex 说这一轮结束（约 6 分钟）。
- **协助 3（Wi-Fi 阶段）**：Codex 完成第 3.2 节的准备后，再请用户运行一次「CopiaGo QA」。

### 2.3 快捷指令内容（让用户照着搭）

**快捷指令「QA 生成压缩包」**：
1. 「获取最新的照片」：数量 40，不包括截屏
2. 「制作归档」：格式 `.zip`，名称 `qa`
3. 「存储文件」：存到「我的 iPhone/CopiaGo-QA」，关闭「询问存储位置」，打开「覆盖文件」

**快捷指令「CopiaGo QA」**（按顺序添加，每个「等待」都是等待动作）：

| 步骤 | 动作 | 用途 |
|---|---|---|
| 1 | 文本 `QA-P0` → 拷贝到剪贴板；等待 6 秒 | 起始标记 |
| 2 | 获取最新的截屏（1 张）→ 拷贝到剪贴板；等待 15 秒 | 截图（PNG）→ 电脑 |
| 3 | 文本 `QA-P1` → 拷贝到剪贴板；等待 6 秒 | 标记 |
| 4 | 获取最新的照片（1 张，不包括截屏）→ 拷贝到剪贴板；等待 40 秒 | 相册照片单独传输 |
| 5 | 文本 `QA-P2` → 拷贝到剪贴板；等待 6 秒 | 标记，Codex 准备开始并发推送 |
| 6 | 获取最新的照片（1 张，不包括截屏）→ 拷贝到剪贴板；等待 40 秒 | 相册照片与电脑推送同时进行 |
| 7 | 文本 `QA-P3` → 拷贝到剪贴板；等待 6 秒 | 标记 |
| 8 | 获取文件：`CopiaGo-QA/qa.zip`（关闭「显示文档选择器」）→ 拷贝到剪贴板；等待 90 秒 | 大文件下载，以及下载期间的读写 |
| 9 | 文本 `QA-F0` → 拷贝到剪贴板；等待 40 秒 | Codex 在这段时间关闭"图片同步到电脑"并重启程序 |
| 10 | 获取最新的截屏 → 拷贝到剪贴板；等待 10 秒 | 类型过滤 |
| 11 | 文本 `QA-E0` → 拷贝到剪贴板 | 进入回显阶段 |
| 12 | 「重复」25 次：等待 5 秒 → 获取剪贴板 → 获取「类型」→ 如果类型是「图像」：获取图像的宽度、高度，拷贝文本 `ECHO-IMG:<宽>x<高>`；否则，如果剪贴板文字不以 `QA-` 开头、也不以 `ECHO` 开头，拷贝文本 `ECHO:` 加上原文字 | 把电脑发来的内容回传给电脑，用来核对 |
| 13 | 文本 `QA-END` → 拷贝到剪贴板 | 结束标记 |

所有步骤都以 Windows 剪贴板上出现的标记文字为准，**不要依赖等待的秒数**。如果某个标记 180 秒内没有出现，就把它记为"无法判定"，继续等下一个标记。

## 3. 测试流程

每个用例都要记录：开始时的 `Log-Mark`、相关日志行、剪贴板读取结果和结论。

### 3.1 第一轮：USB（开启隔离模拟）

准备：

```powershell
Enable-Isolation; Show-Isolation
Stop-Qa; Set-QaSettings; $m0 = Log-Mark; Start-Qa
Wait-Log 'event=wifi_endpoint_failed .*error=Unreachable' $m0 60
Wait-Log 'tunnel_open transport=' $m0 90      # 应显示 USB
Wait-Log 'event=status state=Connected' $m0 90
```

**U1 识别 AP 隔离**
- 通过条件：出现 `wifi_endpoint_failed family=v4 error=Unreachable`，之后出现 `attempt_failed transport=Wifi ... failure=Wifi(Unreachable)`，最终通过 USB 连上。
- 如果出现的是 `error=Connect` 或 `error=Timeout`，记为失败，并贴出相关日志行。

然后请用户执行**协助 2**，按顺序执行下面的用例：

**U2 截图（PNG）手机→电脑**：等到 `QA-P0` 出现后，再等剪贴板变成图片。
- 通过条件：出现 `event=pull_complete kind=image`，`Get-ClipImageSize` 和手机截图的尺寸一致。记录从标记出现到完成的耗时。

**U3 相册照片单独传输**：`QA-P1` 出现后 60 秒内，记录出现了下面哪一种：
- `pull_complete kind=image`：读取成功，记录尺寸。
- `pull_unreadable timed_out=true`：读取超时（截图核对第 3.4 节的提示文字）。
- `pull_failed kind=image ... error=Clipboard image codec failed (0xc00d5212)`，或其他编码 `0x88982f50`、`0x88982f8b`：说明这台电脑缺少 HEIC 解码组件。如果第 1.2 节显示这两个扩展**已安装**还出现这个错误，记为失败。
- 结合 `pull timeout types=` 中 image 的数量，判断手机提供了哪些图片格式。

**U4 并发：相册照片读取中，电脑推送不受阻塞**：`QA-P2` 出现后立刻执行：

```powershell
$m = Log-Mark
1..15 | ForEach-Object { Set-ClipText ("QA-PC-{0}-{1}" -f $_, [guid]::NewGuid()); Start-Sleep -Milliseconds 1500 }
$last = Get-ClipText
```

- 用日志里每一对 `push_queued` 和 `push_complete` 的 `elapsed_ms` 计算推送耗时。**通过条件**：在照片读取进行期间（从 `QA-P2` 之后照片开始传输，到 `pull_complete`、`pull_unreadable` 或 `pull_superseded` 出现为止），每次推送都在 3 秒内完成。
- **被取代的读取**：这一轮推送结束、照片读取也结束后，Windows 剪贴板应仍是 `$last`（电脑最后复制的文字），不能被那张照片覆盖。日志里出现 `pull_superseded` 属于正常。如果剪贴板变成了照片，记为失败。

**U5 大文件下载期间，读写照常，且下载结果不覆盖更新内容**：`QA-P3` 出现后：

```powershell
$m = Log-Mark
Wait-Log 'event=file_requested' $m 30
Start-Sleep 2; $t0 = Get-Date
Set-ClipText ("QA-PC-during-download-{0}" -f [guid]::NewGuid()); $mine = Get-ClipText
Wait-Log 'event=push_complete' $m 10        # 下载期间推送仍应很快完成
Wait-Log 'event=file_(superseded|complete|failed)' $m 120
```

- 通过条件：
  - 推送在 3 秒内完成。
  - 下载过程中定时检查照常进行：日志里没有 `MetadataUnresponsive`，连接没有断开。
  - 下载结束后出现 `file_superseded`，而且 Windows 剪贴板仍是 `$mine`。
- 如果出现的是 `file_complete`，并且剪贴板变成了文件，记为失败。
- 如果手机端没有发起文件下载（没有 `file_requested`），记为"无法判定"，并记录 `unsupported` 等相关行。可能是快捷指令没有把文件以文件形式放进剪贴板；这时在最后一次协助里请用户从「文件」App 里长按 `qa.zip` →「拷贝」，重测这一项。

**U6 类型过滤**：`QA-F0` 出现后立刻执行：

```powershell
Stop-Qa; Set-QaSettings -Pull @('text','file'); $m = Log-Mark; Start-Qa
Wait-Log 'event=status state=Connected' $m 60
Wait-Log 'event=pull_filtered kind=image' $m 60
```

- 通过条件：出现 `pull_filtered kind=image`，Windows 剪贴板没有变成截图。
- 用 `Show-Qa` 打开面板截图，核对状态栏文字（第 3.4 节）。
- 然后恢复设置：`Stop-Qa; Set-QaSettings; Start-Qa`，等到再次 `Connected`（`QA-E0` 出现前大约有 40 秒的余量）。

**U7 电脑→手机推送与回显**：`QA-E0` 出现后依次执行，每一项都等手机把内容回显回来：

| 项 | 操作 | 通过条件 |
|---|---|---|
| E1 文字 | `Set-ClipText "PCTXT-文字😀-$(New-Guid)"`（不能以 `QA-` 开头，否则快捷指令不会回显） | `push_complete`；30 秒内剪贴板变成 `ECHO:` 加上原文字，内容完全一致 |
| E2 小图 | `New-TestPng $qa\a.png 1200 800; Set-ClipPng $qa\a.png` | `push_queued kind=image` 之后很快出现 `push_complete kind=image`；回显 `ECHO-IMG:1200x800` |
| E3 大图 | `New-TestPng $qa\b.png 3800 3000 -Noise`（约 45 MB）后 `Set-ClipPng` | 记录推送耗时；`push_complete`，或 `push_failed reason=pasteboard request timed out`（这时截图核对提示文字）；能回显时尺寸为 `3800x3000` |
| E4 超限 | `New-TestPng $qa\c.png 5000 4000 -Noise`（大于 64 MB）后 `Set-ClipPng` | 不应出现 `push_queued kind=image`；应出现 `pc_read_failed`，截图核对"电脑上复制的图片过大"提示 |
| E5 回声 | E1 到 E3 期间 | 推送出去的内容不应作为"手机新复制"再写回电脑：日志里不应出现与之对应、而剪贴板内容又是原文字的 `pull_complete` |
| E6 推送过滤 | `Stop-Qa; Set-QaSettings -Push @('text'); Start-Qa`，连上后执行 `Set-ClipPng $qa\a.png` | 出现 `push_filtered kind=image`，没有 `push_queued kind=image`；截图核对提示；然后恢复设置并重启 |

如果 25 次回显循环提前结束，没做完的项记为"无法判定"，在最后一次协助时请用户再运行一次快捷指令补测。

### 3.2 第二轮：Wi-Fi（关闭隔离模拟）

```powershell
Disable-Isolation; Show-Isolation       # 应查不到静态 ARP 记录，IPv6 Enabled=True
Stop-Qa; Set-QaSettings; $m0 = Log-Mark; Start-Qa
Wait-Log 'tunnel_open transport=' $m0 90   # 应显示 Wi-Fi（插着线时也优先走 Wi-Fi）
```

- **W0**：确认这一轮的传输方式是 Wi-Fi。如果仍是 USB，记录 `wifi_endpoint_failed` 的错误类型。如果是 `Unreachable`，说明当前网络本身就禁止设备互访，这时 Wi-Fi 轮整体记为"无法判定（网络限制）"，跳过本轮。
- 请用户执行**协助 3**，再跑一遍 U2 到 U7（U6 可以跳过）。重点比较 U3、U4、U5 和 USB 轮的差异，尤其是推送耗时和照片读取耗时。

### 3.3 恢复（必须执行）

```powershell
Stop-Qa
Disable-Isolation; Show-Isolation
Remove-Item Env:ICB_DATA_DIR -ErrorAction SilentlyContinue
if ($installed) { Start-Process $installed }
```

提醒用户把「自动锁定」改回原来的设置。`$qa` 目录保留，便于事后复查；它在 `%TEMP%` 下，可以随时删除。

### 3.4 提示文字截图核对（GUI）

用 `Show-Qa` 打开面板，用下面的方法截屏，核对状态栏文字是否与预期一致。每张截图对应一个用例编号。

```powershell
$bmp = New-Object System.Drawing.Bitmap ([System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Width), ([System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Height)
[System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen(0, 0, 0, 0, $bmp.Size); $bmp.Save("$qa\shot-<用例>.png")
```

| 场景 | 应出现的文字（开头部分） |
|---|---|
| U3 读取超时 | "iPhone 上的图片读取超时，未同步到电脑。请保持手机解锁后重新复制；相册照片若存储在 iCloud…" |
| U3 缺少 HEIC 解码组件 | "这台电脑缺少 HEIC 照片解码组件，未能写入。请在 Microsoft Store 安装…" |
| U5 下载结果未覆盖 | "「qa.zip」已下载，但电脑剪贴板里已有更新的内容，为避免覆盖未放入剪贴板。需要时可在历史中复制它" |
| U6 | "iPhone 上复制了图片，但设置中未开启「图片」同步到电脑。如需同步，请在设置的「自动同步到电脑」中勾选「图片」" |
| E3 超时 | "发送图片超时：iPhone 在 20 秒内没有确认。请保持手机解锁后重试" |
| E4 | "电脑上复制的图片过大，未发送到 iPhone。请缩小或裁剪图片后重新复制" |
| E6 | "电脑上复制了图片，但设置中未开启「图片」同步到 iPhone。如需同步…" |
| 第二轮如果是 `Unreachable` | "已在 Wi-Fi 中发现 iPhone，但电脑连不上它。当前网络可能禁止设备互相访问…"（插着线时会回退到 USB，所以这句不会出现，属于正常） |

如果 `Show-Qa` 没有打开面板，或截图里看不到状态栏，把这些项留到最后，**合并成一次**请用户看屏幕确认。不要逐项询问。

### 3.5 headless 模式补测（可选，不需要额外协助）

**这一节要在第 3.3 节恢复之前执行。**

如果第二轮 Wi-Fi 结束后快捷指令还在回显阶段，或者用户愿意再点一次快捷指令，可以用 headless 模式重测 U5：

```powershell
Stop-Qa; Set-QaSettings; $env:ICB_DATA_DIR = "$qa\data"
$p = Start-Process "<仓库根目录>\copiago-rs\target\release\copiago.exe" -ArgumentList '--headless' -RedirectStandardOutput "$qa\headless.out" -PassThru
```

headless 模式会把事件同时写到标准输出（`$qa\headless.out`）和 `diagnostic.log`。通过条件和 U5 相同：下载期间在电脑上复制文字后，应出现 `event=file_superseded`，Windows 剪贴板仍是电脑上复制的文字。headless 模式没有历史，文件会被丢弃，这属于预期行为。结束后执行 `Stop-Process $p`。

## 4. 报告格式

输出一份 Markdown 报告，包含：

1. 环境：Windows 版本、HEIF/HEVC 扩展是否安装、iOS 版本（从诊断日志或问用户）、提交 hash、`cargo test` 结果。
2. 结果表：用例编号 | 传输方式 | 结论（通过 / 失败 / 无法判定）| 证据（日志行原文、剪贴板尺寸或文字是否一致、耗时）| 备注。
3. 推送耗时统计：USB 轮和 Wi-Fi 轮各自 U4、U5、E2、E3 的最小、中位、最大值。
4. 照片读取统计：U3、U4 从标记出现到结果的耗时，以及结果类型。
5. 失败项分析：失败的用例要给出最可能的原因，并指向相关代码位置（`crates/copiago-device/src/pasteboard.rs` 的 `watch_updates`、`crates/copiago-app/src/gui/sync.rs` 等）。
6. 恢复检查：贴出 `Show-Isolation` 的输出，确认网络已恢复。
7. 附件：`$qa\data\diagnostic.log` 和截图的路径。

## 5. 已知限制（不算失败，但要在报告里注明）

- HEIC 替代格式的等待时间（单项 8 秒、总共 12 秒）是估计值，请记录实际耗时，供后续调整。
- 历史面板里单条"发送中 / 发送失败"状态需要手动点击，本手册没有覆盖，只由单元测试 `a_row_cannot_be_sent_again_until_the_phone_answers` 保证。
- 第二轮如果当前网络本身就禁止设备互访，Wi-Fi 相关用例无法在这个网络里验证。
