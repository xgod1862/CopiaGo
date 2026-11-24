# CopiaGo Rust 重写进度

| 编号 | 状态 | PR 链接 | 备注 |
| --- | --- | --- | --- |
| T-CORE-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/8 | 建立 workspace 与 crate 骨架 |
| T-CI-01 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | Rust CI、6 MB 门禁及 GPL 拒绝验证 |
| T-CORE-02 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 四种数据目录、DPAPI、原子写和原生 ACL 继承通过。实际 Python device-info 只读解析通过 |
| T-CORE-03 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 10 项设置，非法值回落、未知键和原子更新测试通过。真实 Python 设置只读验证通过 |
| T-CORE-04 | 已合并 | https://github.com/xgod1862/CopiaGo/pull/9 | 170 组翻译逐项核对，命名占位符、延迟渲染和系统语言测试通过 |
| T-DEV-01 | 待真机验证 | — | USB 隧道和设备公共接口已集成。本机 usbmuxd 可用，待接单台已信任 iPhone 验证 RSD |
| T-DEV-02 | 待真机验证 | — | 通知、2 秒轮询及剪贴板拉取的 20 项单测通过，待 USB 真机验证事件延迟与 Notes |
| T-DEV-03 | 待真机验证 | — | 模拟中文、emoji、PNG、120 秒回声和 SET 变更标识通过，真机往返待安全复验 |
| T-DEV-04 | 已合并 | — | 真机自动发现和显式 IPv4 均完成 pair-verify/RSD。临时 DPAPI 写回和 Python 互通通过。迁移拒绝覆盖检查时已变化的记录，目标已删除时不会重建。 |
| T-DEV-05 | 待真机验证 | — | Wi-Fi 优先、同设备 USB 回退及断线恢复已集成。Wi-Fi 与模拟唤醒通过，实际 USB、锁屏、休眠待测 |
| T-DEV-06 | 已合并 | — | 原生 mDNS 在物理 Wi-Fi 与虚拟网卡并存时实机匹配 8/8，正常间隔均 < 1 s |
| T-WIN-01 | 已合并 | — | 真实 Windows runner 剪贴板通过文本、透明 PNG、DIB、文件及监听与隐私标记验收 |
| T-APP-01 | 待办 | — | |
| T-DEV-07 | 待办 | — | |
| T-DEV-08 | 待办 | — | |
| T-DEV-09 | 待办 | — | |
| T-CORE-05 | 待办 | — | |
| T-IMG-01 | 待办 | — | |
| T-IMG-02 | 待办 | — | |
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
