# CopiaGo Rust 重写进度

2026-11-28 的真机结果及证据来源见 [交付报告](RUN-REPORT.md) 和 [实测指标](METRICS.md)。

| 编号 | 状态 | PR 链接 | 备注 |
| --- | --- | --- | --- |
| T-CORE-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/8 | 建立 workspace 与 crate 骨架 |
| T-CI-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | Rust CI、6 MB 门禁及 GPL 拒绝验证 |
| T-CORE-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 四种数据目录、DPAPI、原子写和原生 ACL 继承通过。实际 Python device-info 只读解析通过 |
| T-CORE-03 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 10 项设置，非法值回落、未知键和原子更新测试通过。真实 Python 设置只读验证通过 |
| T-CORE-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 170 组翻译逐项核对，命名占位符、延迟渲染和系统语言测试通过 |
| T-DEV-01 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/10 | 已信任的单台 iPhone 在 USB 上完成 RSD、剪贴板服务发现及文件接收。 |
| T-DEV-02 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/10 | USB、Wi-Fi 均读回手机文字；复制至可粘贴及历史更新的时延、Notes 慢响应后的恢复由测试者复测通过。 |
| T-DEV-03 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/10 | 两条链路均双向核对文字和 PNG；推送回声、历史手动发送及空剪贴板恢复由测试者确认。 |
| T-DEV-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 真机自动发现和显式 IPv4 均完成 pair-verify/RSD。临时 DPAPI 写回和 Python 互通通过。迁移拒绝覆盖检查时已变化的记录，目标已删除时不会重建。 |
| T-DEV-05 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/10 | 留有 USB、Wi-Fi 连接及恢复无线记录。睡眠唤醒、同机回退与回切、离线后恢复由测试者确认。 |
| T-DEV-06 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 多网卡原生 mDNS 真机匹配 8/8，正常间隔均 < 1 s。自动路径只连接与 Python 的 peer_alt_irk/TXT 匹配的地址；无匹配时由连接管理器回退。 |
| T-WIN-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 真实 Windows runner 剪贴板通过文本、透明 PNG、DIB、文件及监听与隐私标记验收 |
| T-APP-01 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/10 | 双向文字及 PNG 有双链路回读；headless 传输期间再次复制和实际唤醒由测试者确认。 |
| T-DEV-07 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/11 | 100 MiB 在 USB、Wi-Fi 上落盘哈希一致；500 MiB 两种请求方式的对照由测试者确认，未提供逐次测值。 |
| T-DEV-08 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/11 | 正式版 USB 复制一次 512 MiB，仅一次内容请求和一份收件；513 MiB 拒收。重叠复制、确认、过期及清理由测试者确认。 |
| T-DEV-09 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/11 | 隔离实例忘记设备后保留历史并返回引导；首次配对及拒绝信任由测试者确认。 |
| T-CORE-05 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/11 | 使用系统 winsqlite3、DPAPI 和原表。双来源上限、去重、旧列迁移及合成 Python 双向实测通过。关闭保存后保留原历史，独立清空。 |
| T-IMG-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/11 | 统一 WIC 解码、缩放、PNG 与 DIB。48 MP 基线 JPEG 缩略图 5 次峰值 ≤ 13.20 MB，透明像素保持。渐进式 48 MP 可解码但超 30 MB（155.39 MB）。 |
| T-IMG-02 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/11 | USB、Wi-Fi 均逐像素核对测试图；照片、截图、Safari 和 Notes 来源由测试者确认。 |
| T-DDI-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/12 | WinHTTP 续传、换源、取消和原子发布。两套镜像真下载，8/8 Python 固定大小及 SHA-256 匹配，热缓存零下载。 |
| T-DDI-02 | 人工复测通过 | https://github.com/xgod1862/CopiaGo/pull/12 | 测试者确认开发者模式与重启后重新挂载，未提供新的挂载耗时。 |
| T-UI-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | Composition/WARP 宿主、跨进程透明区穿透和显隐资源释放通过。模拟 150%/200% 像素验收通过，隐藏专用工作集峰值 2,797,568 字节。真实混合 DPI 待复验。 |
| T-UI-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | 通用合成动画和全部组件已集成，普通/减少动画原生展示、滚动命中和视觉树清理通过。展示可见专用工作集峰值 5,308,416 字节。 |
| T-UI-03 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | Python 双标签、选择/复制/发送、清空确认已实现；四组 96 DPI 原尺寸对照，50 条图像只解码视口。150 ms 失焦和 Esc 原生通过。 |
| T-UI-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | 全部设置、即时语言和忘记二次确认通过隔离测试；设备卡、配对、镜像、错误和两种动画原生预览通过。真实驱动与设备由 T-APP-02 联验。 |
| T-UI-05 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | Tab/Shift+Tab、Enter/Space、焦点可见和跨进程 UIA 通过，普通/减少动画两轮验证。实际讲述人音频及正式页面待联验。 |
| T-UI-06 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | 真实 Shell 图标与双屏定位、无激活显示和保锚重建通过。四边、负坐标、溢出及 150%/200% 模拟通过，真实混合 DPI 待验。 |
| T-WIN-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | Windows Shell 删除测试图标后由 TaskbarCreated 恢复，双进程只激活原 PID，开机启动隔离验证精确旧 Run 值。Explorer 实际重启、主题和用户登录待联验。 |
| T-WIN-03 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/13 | 启动 15 s 后单次检查，重新开启立即检查，关闭取消并清提示。真实 GitHub 稳定版提示和发布页跳转通过。2.0.0-beta 沿用 Python 规则无提示，正式 UI 随 T-APP-02 验收。 |
| T-APP-02 | 真机通过 | https://github.com/xgod1862/CopiaGo/pull/13 | 正式界面接收、资源管理器粘贴及原位升级有现场记录；睡眠、VPN 和正式面板交互由测试者确认。 |
| T-PKG-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/14 | 最终安装包 3,688,692 B，ZIP 1,900,112 B，隔离实装 7,896,320 B。静态 CRT、安装卸载及重复安装启动项通过。 |
| T-PKG-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/14 | 真实 Python 1.2.0 副本覆盖及数据互通通过。最终包使用保留配对在 3,177.88 ms 连上 Wi-Fi，无需重新配对。 |
| T-QA-01 | 人工复测通过 | https://github.com/xgod1862/CopiaGo/pull/14 | 初测冷启及多网卡发现保留原始数值；测试者确认复测及 24 小时稳定性通过，未提供该轮采样序列。 |
