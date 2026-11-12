# LiveTranslate 构建风味：标准版 / Full 版

LiveTranslate 从 v0.2 开始提供两种构建风味。两者**版本号一致、配置互通**，区别只在是否编译了 SteamVR 与 OSC 输出。

## 一句话选择

- **标准版**：桌面浮窗字幕 + 多输入源 + 独立设置页。不含 SteamVR、不含 OSC。
- **Full 版**：标准版全部功能 + SteamVR 头显字幕 + VRChat OSC Chatbox。**仅 Windows x64**。

## 下载对照表

| 平台 | 文件名 | 风味 | SteamVR | OSC |
| --- | --- | --- | --- | --- |
| Windows x64 | `livetranslate-x86_64-pc-windows-msvc.zip` | 标准版 | — | — |
| Windows x64 | `livetranslate-x86_64-pc-windows-msvc-full.zip` | **Full 版** | ✅ | ✅ |
| Linux x64 | `livetranslate-x86_64-unknown-linux-gnu.tar.gz` | 标准版 | — | — |
| macOS（Universal） | `LiveTranslate-universal-apple-darwin.zip` | 标准版 | — | — |

> `-full` 后缀是唯一的区分标识。两个 Windows 包解压后的目录名也不同，不会互相覆盖。

## 怎么选

| 你的场景 | 选择 |
| --- | --- |
| 戴 VR 头显，想在 SteamVR 里看字幕 | Full 版 |
| 用 VRChat，想把字幕发到 Chatbox | Full 版 |
| 只用桌面浮窗 / 多麦克风 / 系统回环 | 标准版 |
| 不确定 | 先下标准版；需要时换 Full 版即可，配置不会丢 |

## 功能差异

| 功能 | 标准版 | Full 版 |
| --- | --- | --- |
| 桌面透明浮窗字幕 | ✅ | ✅ |
| 多输入源并行 + 逐源翻译开关 | ✅ | ✅ |
| 独立设置窗口 / 配置档切换 | ✅ | ✅ |
| VRChat OSC Chatbox 输出 | — | ✅ |
| SteamVR Overlay 字幕 | — | ✅（仅 Windows） |
| 原有托盘 / 快捷键 / 点击穿透 | ✅ | ✅ |

标准版的设置页里，SteamVR 与 OSC 分组会置灰并提示「当前构建未包含，请下载 Full 版」；手动在 `config.toml` 里把未启用的输出改成 `enabled = true` 也不会生效（代码未编译），保存时会被拒绝；若原本已在 Full 版启用，标准版允许正常修改其它配置并保留该开关。

## 配置与数据兼容

- 两个包使用**同一份**用户配置（`config.toml`）与**同一套**配置档目录（`profiles/`）。
- 标准版会**保留但忽略** `[output.steamvr]` / `[output.osc]` 段，不会删除；换回 Full 版后原设置继续生效。
- 配置档（profiles）、日志、ONNX Runtime 两个包一致。
- 从标准版换到 Full 版：解压新包即可，无需迁移配置；发布包内仅附带 `config/config.example.toml` 参考模板，默认读写用户配置目录（若手工重命名为 `config/default.toml` 则切换为单目录便携模式，此时不共用配置且禁用多配置档）。

## 从源码构建

- 标准版：`cargo build --release --features custom-protocol`
- Full 版（仅 Windows）：
  ```powershell
  cargo build --release --features custom-protocol,extras
  ```
  Full 版需要 CMake + MSVC（`openvr-sys` 静态链接 OpenVR），因此**只能在本机 Windows 或 CI 的 Windows runner 上构建**；macOS 上的交叉编译脚本只产出标准版。

## 已知限制

- SteamVR 仅支持 Windows；Linux / macOS 不提供 Full 版。
- OSC 需要 VRChat 开启 OSC：`Action Menu → Options → OSC → Enabled`。
- VRChat Chatbox 的 144 字符、最多 9 行、发送限流由 VRChat 客户端决定，程序已做合并与保守限流。
- 标准版无法通过改配置启用 OSC / SteamVR。

## 反馈问题时请注明

- 版本号（如 `v0.2.0`）
- 包名（标准版 / Full 版）
- 操作系统版本
- SteamVR / VRChat 版本（如相关）
- 日志文件：`<用户数据目录>/logs/livetranslate.log.<日期>`（macOS 为 `~/Library/Application Support/com.chinuno.LiveTranslate/logs`，Windows 为 `%LOCALAPPDATA%\chinuno\LiveTranslate\data\logs`）
