# ASR Translate - 实时语音识别翻译字幕程序

一个用 Rust 编写的实时语音识别与翻译系统，支持从麦克风或系统回环采集音频，调用本地 Whisper API 进行识别，并通过 OpenAI 兼容 API 进行翻译。

## 特性

✅ **音频采集**
- 支持 Windows 麦克风输入
- 支持 Windows 系统回环采集（WASAPI Loopback）
- 自动设备枚举和选择

✅ **实时语音识别**
- 调用本地 Whisper 兼容 HTTP API（默认 `http://127.0.0.1:8765`）
- 可配置 API base_url
- 支持自定义识别语言

✅ **实时翻译**
- 对接 OpenAI 兼容 API（如 OpenAI、Azure OpenAI、本地 LLM 服务等）
- 可配置的翻译目标语言（默认中文）
- 可选原文显示

✅ **音频处理**
- 固定分片时长（可配置，建议 2 秒）
- 片段重叠处理（25% overlap）减少截断问题
- 静音检测和过滤
- 自动 WAV 编码

## 系统要求

- **Windows 10/11** (macOS/Linux 支持需要调整)
- Rust 1.70+
- 本地 Whisper 兼容 HTTP 服务（如 whisper.cpp HTTP server）
- OpenAI API 或兼容的翻译服务

## 安装

### 从源码编译

```bash
git clone <repo>
cd asrtranslate
cargo build --release
```

编译后的二进制在 `target/release/asrtranslate`

### 快速测试

```bash
# 列出可用的音频设备
./target/release/asrtranslate --list-devices

# 使用默认设备运行（需要配置文件）
./target/release/asrtranslate

# 指定日志级别
./target/release/asrtranslate --log-level debug
```

## 配置

编辑 `config/default.toml` 配置各个模块：

```toml
[audio]
# 设备选择（用 --list-devices 查看可用值）
#   "default" / "microphone" / "loopback" / "mic:0" / "loopback:0" / "0" / 设备名模糊匹配
# 回环采集仅 Windows 支持，用于捕获系统声音
# 也可在浮窗顶部下拉框直接切换
device_name = "default"
# 音频分片时长（秒）
chunk_seconds = 2.0
# 目标采样率（Whisper 推荐 16000，程序会自动重采样）
sample_rate = 16000
# 声道数（内部固定下混为单声道）
channels = 1
# 音量阈值 (0.0-1.0)
silence_threshold = 0.01

[asr]
# Whisper 兼容 API 地址
base_url = "http://127.0.0.1:8765"
# 模型名称
model = "whisper-1"
# 识别语言 (auto / zh / en 等)
language = "auto"
# 请求超时时间（秒）
timeout_secs = 20
# 请求路径
request_path = "/v1/audio/transcriptions"

[translate]
# OpenAI 兼容 API 地址
base_url = "https://api.openai.com/v1"
# API Key
api_key = "sk-your-api-key"
# 模型
model = "gpt-4o-mini"
# 目标语言
target_language = "zh-CN"
# 翻译系统提示词
system_prompt = "You are a real-time subtitle translator. Translate naturally and concisely."
# 超时时间（秒）
timeout_secs = 20

[subtitle]
# 最多显示行数
max_lines = 3
# 字体大小
font_size = 28
# 文字颜色
text_color = "#FFFFFF"
# 描边颜色
stroke_color = "#000000"
# 背景 (transparent)
background = "transparent"
# 是否显示源文本
show_source = false
# 窗口尺寸和位置
window_width = 1200
window_height = 220
position_x = 200
position_y = 760
# 始终置顶
always_on_top = true
# 点击穿透（透明窗口）
click_through = false
```

## 使用

### 基本流程

1. **启动本地 Whisper 服务**
   ```bash
   # 使用 whisper.cpp 的 HTTP 服务器
   ./server -m models/ggml-base.bin -p 8765
   ```

2. **配置翻译 API**
   - 编辑 `config/default.toml` 中的 `[translate]` 部分
   - 设置 `base_url` 和 `api_key`

3. **运行程序**
   ```bash
   ./target/release/asrtranslate --log-level info
   ```

4. **实时监控**
   - 程序默认启动 **Tauri 透明字幕浮窗**
   - 浮窗顶部可切换音频设备、开始/停止、切换点击穿透、关闭
   - 字幕会自动去重与合并重叠文本，避免重复显示
   - 日志仍会输出到控制台，便于调试

### 系统托盘与快捷键

程序启动后会在系统托盘常驻一个图标，提供：
- **显示**：显示并聚焦字幕浮窗
- **开始 / 停止**：控制字幕采集
- **退出**：退出程序

全局快捷键（任何窗口下都有效）：

| 快捷键 | 功能 |
|--------|------|
| `Ctrl+Alt+S` | 显示/聚焦浮窗 |
| `Ctrl+Alt+R` | 开始采集 |
| `Ctrl+Alt+E` | 停止采集 |
| `Ctrl+Alt+Q` | 退出程序 |

> 单击托盘图标可显示/隐藏浮窗；双击托盘图标可唤出浮窗。

### 命令行选项

```
USAGE:
    asrtranslate [OPTIONS]

OPTIONS:
    -c, --config <CONFIG>      指定配置文件路径
    -l, --list-devices         列出可用音频设备后退出
    --log-level <LOG_LEVEL>    日志级别 (trace/debug/info/warn/error)
                               [default: info]
    -h, --help                 显示帮助信息
```

## 体系结构

```
┌─────────────────────────────────────────────┐
│       Rust 后端 + 核心处理流程               │
├─────────────────────────────────────────────┤
│  配置 │ 日志 │ 错误处理 │ 异步任务管理       │
├─────────────────────────────────────────────┤
│  音频采集 (cpal)                             │
│  ├─ 枚举设备                                 │
│  ├─ 打开麦克风/回环                         │
│  └─ 实时 PCM 流输出                         │
├─────────────────────────────────────────────┤
│  音频处理                                    │
│  ├─ 固定分片 (2s)                           │
│  ├─ 重叠处理 (300ms)                        │
│  ├─ 静音检测                                 │
│  └─ WAV 编码                                 │
├─────────────────────────────────────────────┤
│  ASR 客户端 (Whisper HTTP)                  │
│  ├─ 分片上传                                 │
│  ├─ 响应解析                                 │
│  └─ 文本输出                                 │
├─────────────────────────────────────────────┤
│  翻译客户端 (OpenAI API)                    │
│  ├─ Chat API 调用                           │
│  ├─ 结果缓存                                 │
│  └─ 去重处理                                 │
├─────────────────────────────────────────────┤
│  字幕状态管理                                │
│  ├─ 历史滚动                                 │
│  ├─ 原文/译文切换                           │
│  └─ 事件推送                                 │
└─────────────────────────────────────────────┘
```

## 开发阶段

### Phase 1: 完成 ✅
- [x] 项目骨架和配置
- [x] 错误处理框架
- [x] 日志系统

### Phase 2: 完成 ✅
- [x] 音频设备枚举
- [x] 音频采集（cpal）
- [x] 音频分片和编码

### Phase 3: 完成 ✅
- [x] Whisper API 客户端
- [x] ASR 请求/响应处理

### Phase 4: 完成 ✅
- [x] OpenAI 兼容客户端
- [x] 翻译接口实现

### Phase 5: 完成 ✅
- [x] Tauri 透明字幕浮窗
- [x] 窗口拖动和样式
- [x] 开始/停止/穿透/关闭控制

### Phase 6: 计划中
- [ ] 完整体验优化
- [ ] Windows 打包
- [ ] 性能调优

## 关键设计

### 音频处理策略

**问题**：固定分片可能导致一句话被截断

**解决方案**：
1. **固定分片** + **25% 重叠** 
   - 相邻分片有 300-500ms 的重叠
   - 跨片的完整句子能在后一片中继续出现

2. **ASR 后去重**
   - 相邻文本的前缀/后缀去重
   - 避免重复显示

3. **字幕轻量合并**
   - 相邻片段的文本在显示前做简单合并
   - 降低用户感受到的"断句"问题

### 错误恢复

- ASR 失败不影响后续处理
- 翻译失败会记录日志，继续处理下一片
- 网络超时可配置重试（未来版本）

## 日志示例

```
2024-09-28T10:30:45.123Z INFO asrtranslate: Starting ASR Translate Application
2024-09-28T10:30:45.456Z INFO asrtranslate: Config loaded successfully
2024-09-28T10:30:46.789Z INFO asrtranslate::audio::capture: Opening audio device: Device(0)
2024-09-28T10:30:47.012Z INFO asrtranslate::audio::capture: Audio stream started
2024-09-28T10:30:50.234Z INFO asrtranslate::asr: Sending audio to ASR: http://127.0.0.1:8765/v1/audio/transcriptions (size: 64000 bytes)
2024-09-28T10:30:51.567Z INFO asrtranslate::asr: ASR result: Hello, everyone
2024-09-28T10:30:52.890Z INFO asrtranslate::translate: Translated: Hello, everyone -> 大家好
2024-09-28T10:30:52.891Z INFO asrtranslate: Subtitle: Hello, everyone => 大家好
```

## 已知限制

- 🟡 **复杂 VAD**：当前使用简单音量阈值，不上复杂端点检测
- 🟡 **Windows 浮窗实机细节**：建议在目标 Windows 环境验证透明、置顶、点击穿透效果
- 🟡 **点击穿透状态下的交互**：开启点击穿透后，浮窗按钮不能再点击，需用托盘菜单或快捷键操作

## 下一步计划

1. **浮窗体验优化**
   - 更细的字幕样式配置
   - 双语分层排版
   - 快捷键与托盘控制

2. **性能优化**
   - 内存池复用
   - 更高效的去重算法

3. **高级功能**
   - 字幕导出（SRT/VTT）
   - 实时统计（WPM/准确率）
   - 语言自动切换

## 故障排除

### ASR 连接失败
```
ERROR: ASR request failed with status 503
```
- 检查 Whisper 服务是否运行：`curl http://127.0.0.1:8765/v1/models`
- 检查 base_url 配置

### 翻译超时
```
ERROR: Translation request failed with status 504
```
- 增加 `translate.timeout_secs`
- 检查网络连接
- 确认 API 配额

### 音频采集无输入
```
WARN: No input devices found
```
- 检查麦克风是否启用
- 使用 `--list-devices` 列出设备
- 确认系统音量未静音

## 贡献

欢迎提交 Issue 和 Pull Request！

## 许可证

MIT

## 参考资源

- [cpal - Audio I/O](https://github.com/RustAudio/cpal)
- [Whisper API](https://platform.openai.com/docs/guides/speech-to-text)
- [OpenAI Chat API](https://platform.openai.com/docs/guides/gpt)
- [whisper.cpp HTTP Server](https://github.com/ggerganov/whisper.cpp)
