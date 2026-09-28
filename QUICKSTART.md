# 快速开始指南

## 1️⃣ 编译项目

```bash
cd /path/to/asrtranslate
cargo build --release
```

✅ 输出：`target/release/asrtranslate` (4.5 MB)

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
./target/release/asrtranslate --list-devices
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
./target/release/asrtranslate --log-level info
```

如果只想看控制台输出，可使用：

```bash
./target/release/asrtranslate --console --log-level info
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
2024-09-28T11:00:00.123Z INFO asrtranslate: Starting ASR Translate Application
2024-09-28T11:00:00.456Z INFO asrtranslate: Config loaded successfully
2024-09-28T11:00:01.789Z INFO asrtranslate::audio::capture: Opening audio device: Device(0)
2024-09-28T11:00:01.890Z INFO asrtranslate::audio::capture: Audio stream started
2024-09-28T11:00:05.234Z INFO asrtranslate::asr: ASR result: Hello, everyone
2024-09-28T11:00:06.567Z INFO asrtranslate::translate: Translated: Hello, everyone -> 大家好
2024-09-28T11:00:06.568Z INFO asrtranslate: Subtitle: Hello, everyone => 大家好
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
./target/release/asrtranslate --list-devices

# 编辑 config/default.toml
device_name = "microphone"  # 或 "loopback"
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

## 📚 详细文档

- **PLAN.md** - 完整技术方案
- **README_CN.md** - 详细用户指南
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
./target/release/asrtranslate --log-level info
```

祝你使用愉快！🎉
