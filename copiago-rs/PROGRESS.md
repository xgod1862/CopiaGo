# CopiaGo Rust 重写进度

| 编号 | 状态 | PR 链接 | 备注 |
| --- | --- | --- | --- |
| T-CORE-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/8 | 建立 workspace 与 crate 骨架 |
| T-CI-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | Rust CI、6 MB 门禁及 GPL 拒绝验证 |
| T-CORE-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 四种数据目录、DPAPI、原子写和原生 ACL 继承通过。实际 Python device-info 只读解析通过 |
| T-CORE-03 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 10 项设置，非法值回落、未知键和原子更新测试通过。真实 Python 设置只读验证通过 |
| T-CORE-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 170 组翻译逐项核对，命名占位符、延迟渲染和系统语言测试通过 |
| T-DEV-01 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/10 | USB 隧道和设备公共接口已集成。本机 usbmuxd 可用，待接单台已信任 iPhone 验证 RSD |
| T-DEV-02 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/10 | 通知、2 秒轮询及剪贴板拉取的 20 项单测通过，待 USB 真机验证事件延迟与 Notes |
| T-DEV-03 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/10 | 模拟中文、emoji、PNG、120 秒回声和 SET 变更标识通过，真机往返待安全复验 |
| T-DEV-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 真机自动发现和显式 IPv4 均完成 pair-verify/RSD。临时 DPAPI 写回和 Python 互通通过。迁移拒绝覆盖检查时已变化的记录，目标已删除时不会重建。 |
| T-DEV-05 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/10 | Wi-Fi 优先、同设备 USB 回退及断线恢复已集成。Wi-Fi 与模拟唤醒通过，慢请求后的连续推送按序。USB、锁屏、实际休眠待测。 |
| T-DEV-06 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 多网卡原生 mDNS 真机匹配 8/8，正常间隔均 < 1 s。自动路径只连接与 Python 的 peer_alt_irk/TXT 匹配的地址；无匹配时由连接管理器回退。 |
| T-WIN-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/10 | 真实 Windows runner 剪贴板通过文本、透明 PNG、DIB、文件及监听与隐私标记验收 |
| T-APP-01 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/10 | 真机 Wi-Fi 基线 30 分钟，121 个样本，峰值专用工作集 2.18 MB。集成候选冷启中位 1.92 s，注入消息恢复通过；双向文本/PNG、USB 和系统睡眠待验。 |
| T-DEV-07 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/11 | 100/500 MiB 模拟 12/12，保留 512 MiB。文件描述与清单版本一致，缺少回复 ID 时复查；USB/Wi-Fi 真机传输待测。 |
| T-DEV-08 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/11 | 简报/确认、FetchOffer、校验刷盘后 CF_HDROP 与 24h/2GiB 清理已接线，512 MiB 合成 3/3；真机接收、过期与上限待验。 |
| T-DEV-09 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/11 | USB 信任、无线配对保存与 Forget 已接线，DPAPI 合成记录由 Rust/Python 读回。本机无 USB 设备，从零配对和拒绝信任待验。 |
| T-CORE-05 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/11 | 使用系统 winsqlite3、DPAPI 和原表。双来源上限、去重、旧列迁移及合成 Python 双向实测通过。关闭保存后保留原历史，独立清空。 |
| T-IMG-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/11 | 统一 WIC 解码、缩放、PNG 与 DIB。48 MP 基线 JPEG 缩略图 5 次峰值 ≤ 13.20 MB，透明像素保持。渐进式 48 MP 可解码但超 30 MB（155.39 MB）。 |
| T-IMG-02 | 待真机验证 | https://github.com/xgod1862/CopiaGo/pull/11 | 同一条目定向请求 PNG/JPEG，总限时 6 s、图片 ≤64 MiB；无替代时 WIC，四种来源及相册端到端待验。 |
| T-DDI-01 | 待办 | — | |
| T-DDI-02 | 待办 | — | |
| T-UI-01 | 待办 | — | |
| T-UI-02 | 待办 | — | |
| T-UI-03 | 待办 | — | |
| T-UI-04 | 待办 | — | |
| T-UI-05 | 待办 | — | |
| T-UI-06 | 待办 | — | |
| T-WIN-02 | 待办 | — | |
| T-WIN-03 | 待办 | — | |
| T-APP-02 | 待办 | — | |
| T-PKG-01 | 待办 | — | |
| T-PKG-02 | 待办 | — | |
| T-QA-01 | 待办 | — | |
