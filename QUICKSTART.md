# 快速开始指南

## 1️⃣ 编译项目

```bash
cd /path/to/livetranslate
cargo build --release
```

✅ 输出：`target/release/livetranslate`（约 9.5 MB）

从 macOS/Linux 交叉编译到 Windows：

```bash
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

> 发布构建需启用 `custom-protocol` feature（已设为默认，无需手动指定）。

## 2️⃣ 准备环境

### a) 本地 Whisper 服务

使用 whisper.cpp 启动 HTTP 服务器：

```bash
# 下载 whisper.cpp
git clone https://github.com/ggerganov/whisper.cpp
cd whisper.cpp

# 下载模型（例如 base 模型）
bash ./models/download-ggml-model.sh base

# 启动 HTTP 服务器
./server -m models/ggml-base.bin -p 8765
```

检查服务是否正常：
```bash
curl http://127.0.0.1:8765/v1/models
```

### b) 翻译 API（OpenAI 或兼容服务）

编辑 `config/default.toml` 的 `[translate]` 部分：

```toml
[translate]
base_url = "https://api.openai.com/v1"      # OpenAI 官方或其他兼容服务
api_key = "sk-your-api-key-here"            # 替换为实际 API Key
model = "gpt-4o-mini"                       # 或其他可用模型
target_language = "zh-CN"                   # 翻译目标语言
system_prompt = "You are a real-time subtitle translator. Translate naturally and concisely."
timeout_secs = 20
```

## 3️⃣ 配置

编辑 `config/default.toml` 其他部分（可选）：

```toml
[audio]
# 设备选择：只要存在 [[audio.sources]]，device_name 就会被忽略（仅为兼容旧版本）。
# 请改 [[audio.sources]].device，或用浮窗「源」面板切换。
device_name = "default"        # "default" / "microphone" / "loopback"
chunk_seconds = 2.0            # 分片时长
sample_rate = 16000            # 采样率
channels = 1                   # 声道数
silence_threshold = 0.01       # 静音阈值

[asr]
base_url = "http://127.0.0.1:8765"
model = "whisper-1"
language = "auto"

[subtitle]
show_source = false            # true = 显示原文+译文，false = 仅显示译文
max_lines = 3
font_size = 28
```

## 4️⃣ 列出可用设备

在 Windows 上查看可用的麦克风和回环设备：

```bash
./target/release/livetranslate --list-devices
```

输出示例：
```
Available audio devices:
  0. Device(0) [Microphone]
  1. Device(1) [Loopback]
```

## 5️⃣ 运行程序

默认会启动 **Tauri 透明字幕浮窗**。

### 方式 A: 直接运行

```bash
./target/release/livetranslate --log-level info
```

如果只想看控制台输出，可使用：

```bash
./target/release/livetranslate --console --log-level info
```

### 方式 B: 使用启动脚本

**Windows:**
```bash
run.bat
```

**Linux/macOS:**
```bash
bash run.sh
```

## 📝 示例输出

运行成功时，会弹出透明字幕浮窗，同时控制台输出日志：

```
2024-09-28T11:00:00.123Z INFO livetranslate: Starting LiveTranslate Application
2024-09-28T11:00:00.456Z INFO livetranslate: Config loaded successfully
2024-09-28T11:00:01.789Z INFO livetranslate::audio::capture: Opening audio device: Device(0)
2024-09-28T11:00:01.890Z INFO livetranslate::audio::capture: Audio stream started
2024-09-28T11:00:05.234Z INFO livetranslate::asr: ASR result: Hello, everyone
2024-09-28T11:00:06.567Z INFO livetranslate::translate: Translated: Hello, everyone -> 大家好
2024-09-28T11:00:06.568Z INFO livetranslate: Subtitle: Hello, everyone => 大家好
```

## 🎤 测试

1. 打开麦克风或说话
2. 等待 ASR 识别 (1-3 秒)
3. 等待翻译 (1-3 秒)
4. 查看浮窗字幕与控制台日志

## 🔧 故障排除

### 问题 1: "No default input device found"

**解决**：
```bash
# 列出设备
./target/release/livetranslate --list-devices

# 编辑 config/default.toml 里的 [[audio.sources]].device（不是 device_name）
device = "microphone"  # 或 "loopback"
```

### 问题 2: "ASR request failed with status 503"

**解决**：
- 检查 Whisper 服务是否运行
- 检查 base_url 是否正确（默认 `http://127.0.0.1:8765`）
- 确认网络连接

```bash
curl http://127.0.0.1:8765/v1/models
```

### 问题 3: "Translation request failed with status 401"

**解决**：
- 检查 API Key 是否正确
- 检查翻译服务 base_url
- 确认 API 账户有效且有可用配额

### 问题 4: 内存持续增长

**解决**：
- 检查音频设备是否正常
- 尝试减少 `max_lines` 的值
- 重启程序

## 🆕 v0.2：设置页 / 多输入源 / SteamVR / VRChat

### 打开设置页

浮窗工具栏 **⚙ 设置**、托盘菜单 **设置…**，或快捷键 `Ctrl/⌘+Alt+,`。
设置页是独立窗口，覆盖配置文件全部字段，保存时保留注释并自动校验。

### 多输入源

1. 浮窗点 **源 (N)** → 每行切换设备 / 翻译开关；
2. 或在设置页 **输入源** 表格里添加、排序、设置语言覆盖；
3. 同时开启 ≥2 个源时，字幕自动带 `[麦克风]` 前缀（`subtitle.label_sources` 可调）。

### VRChat OSC（Full 版）

1. VRChat：`Action Menu → Options → OSC → Enabled`
2. 设置页 → **OSC / VRChat** → 启用（目标默认 `127.0.0.1:9000`）
3. 默认把最近 4 条字幕合并成滚动字幕；超过 144 字符丢弃最早一条
4. 若 Chatbox 不换行，把 `separator` 改成 `" | "`

### SteamVR Overlay（Windows Full 版）

设置页 → **SteamVR Overlay** → 启用。可调跟随头部/世界坐标、距离、高度、宽度、
曲率、透明度、字号与颜色。SteamVR 未启动时会自动重试。

### 标准版 vs Full 版

- 标准版：桌面浮窗 + 多源 + 设置页/配置档（无 SteamVR / OSC）
- Full 版：额外包含 SteamVR + OSC，仅 Windows x64
- 两包配置互通；详见 `docs/BUILD_FLAVORS.md`

## 📚 详细文档

- **PLAN.md** - 完整技术方案
- **README.md** - 详细用户指南
- **docs/ARCHITECTURE.md** - 系统架构
- **IMPLEMENTATION_SUMMARY.md** - 实现总结

## ⚡ 性能参考

| 指标 | 值 |
|-----|-----|
| 编译时间 | 19.21s |
| 二进制大小 | 4.5 MB |
| 内存占用 | < 2 MB |
| 端到端延迟 | 4-8 秒 |
| 支持设备 | Windows 10/11 |

## 🚀 下一步

1. 字幕导出功能
2. 实时统计面板
3. 更完整的设备/回环选择
4. Windows 打包发布

---

**准备好开始了？ 立即运行：**

```bash
./target/release/livetranslate --log-level info
```

祝你使用愉快！🎉
