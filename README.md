# LiveTranslate - 实时语音识别翻译字幕程序

用 Rust 编写的实时语音识别 + 翻译 + 透明浮窗字幕工具。
从麦克风或系统回环采集音频，经 **VAD 按语音边界切句**后送入识别后端，
再用 OpenAI 兼容 API 翻译，结果以透明浮窗字幕实时显示。

## 特性

✅ **音频采集**
- 麦克风 / 系统回环采集（Windows 走 WASAPI Loopback）
- 自动设备枚举，可在浮窗下拉框热切换
- 自动下混单声道 + 重采样到 16kHz

✅ **VAD 语音分段**
- 按语音边界切句，而不是固定时长硬切
- 自适应噪声底 + 迟滞，抗环境噪声与句中停顿
- 可调余量、最短静音、最长片段等参数

✅ **双识别后端**
- `whisper`：自建 / 兼容的 Whisper HTTP 服务（可配 base_url 与访问令牌）
- `edge`：微软 Edge 内置语音识别的在线服务，无需自建

✅ **实时翻译**
- 对接 OpenAI 兼容 API（OpenAI / Azure OpenAI / 本地 LLM 等）
- 可配置目标语言（默认中文）

✅ **透明浮窗字幕**
- 透明、无边框、置顶、可拖动
- 字号滑块、原文开关，快捷键弹窗
- 鼠标移出自动收起工具栏；开启点击穿透后只保留字幕
- 字幕去重与重叠拼接（支持中文等无空格语言）

✅ **系统集成**
- 系统托盘菜单 + 全局快捷键
- 用户级配置文件，首次运行自动生成
- 日志文件 + 错误弹窗（Windows）

## 系统要求

- **Windows 10/11**（主要目标）、macOS、Linux
- Rust 1.70+
- 识别后端：自建 Whisper 兼容服务，或使用 `edge` 后端（需联网）
- OpenAI API 或兼容的翻译服务

## 安装

### 从源码编译

```bash
git clone <repo>
cd livetranslate
cargo build --release
```

编译后的二进制在 `target/release/livetranslate`

### 快速测试

```bash
# 列出可用的音频设备
./target/release/livetranslate --list-devices

# 使用默认设备运行（需要配置文件）
./target/release/livetranslate

# 指定日志级别
./target/release/livetranslate --log-level debug
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
# 识别后端: "whisper"（自建/兼容服务）或 "edge"（Edge 在线识别）
backend = "whisper"
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
# 访问令牌（可选）。留空则不发送认证头；
# 设置后以 Authorization: Bearer <api_key> 发送
api_key = ""
# 认证头名称（可选，默认 Authorization；部分服务用 api-key）
auth_header = ""

# edge 后端（仅 backend = "edge" 时生效）
[asr.edge]
language = "en-US"    # BCP-47 标签，不能用 auto
timeout_secs = 15

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

[vad]
# 是否启用 VAD 分段（推荐开启）
# 开启后按语音边界切句，而不是固定 2 秒硬切，能显著减少句子被截断
enabled = true
frame_ms = 20
margin_db = 8.0          # 嘈杂环境调大（10~12），安静环境可调小（6）
noise_percentile = 0.1
min_speech_ms = 200      # 语音持续多久才确认开始
min_silence_ms = 400     # 静音持续多久才确认说完
max_speech_ms = 12000    # 单片段最长时长，超过强制切分
pre_pad_ms = 200
post_pad_ms = 200

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

## 浮窗面板

工具栏从左右到依次是：

| 控件 | 作用 |
|------|------|
| 状态灯 + 文本 | 显示运行状态 / 错误提示 |
| 设备下拉框 | 切换音频输入设备（运行中会自动重启流水线） |
| **字号滑块** | 实时调整字幕文字大小（12–72），松手后写回配置文件 |
| **原文开关** | 切换是否在译文上方显示原文，立即重绘当前字幕 |
| 开始 / 停止 | 控制采集 |
| 穿透: 开/关 | 切换点击穿透（开启后只能用托盘或 `Ctrl/⌘+Alt+T` 关掉） |
| **快捷键** | 弹窗展示全部快捷键 |
| 关闭 | 退出程序 |

### 工具栏自动收起

- **鼠标移出浮窗**：工具栏自动收起，只留字幕内容
- **鼠标移入浮窗**：工具栏重新展开
- **开启点击穿透**：窗口不再接收鼠标事件，无法悬停唤出，
  因此工具栏保持收起，只显示字幕（此时用托盘菜单或
  `Ctrl/⌘+Alt+T` 关闭穿透）
- 快捷键弹窗打开期间不会自动收起

> 按住浮窗任意空白处可拖动窗口。
> 字号与原文开关的修改会**就地写入配置文件并保留原有注释**；
> 窗口位置目前仅本次运行有效，重启后仍以配置中的 `position_x/position_y` 为准。

## 语音识别后端

通过 `[asr] backend` 切换，两个后端各自独立配置：

| 后端 | 说明 | 优点 | 缺点 |
|------|------|------|------|
| `whisper`（默认） | 对接自建/兼容的 Whisper HTTP 服务 | 可离线、可控、隐私好 | 需自己部署并保证显存/性能 |
| `edge` | 微软 Edge 内置语音识别的在线服务 | 无需部署、开箱即用 | 需联网、依赖服务端策略、可能限流 |

```toml
[asr]
backend = "edge"

[asr.edge]
language = "en-US"    # BCP-47 标签，**【不能】用 auto**
timeout_secs = 15
```

> `edge` 后端使用微软 Edge 浏览器语音识别所用的 WebSocket 服务（
> `speech.platform.bing.com`）。握手需要一组会轮换的客户端身份参数，
> 默认值已内置；若将来出现 401 / 握手失败，可在 `[asr.edge]` 里覆盖
> `trusted_client_token` / `chromium_full_version` / `origin`。

### 怎么选

- 有本地 GPU / 想离线：用 `whisper`
- 想快速试用、不想搭服务：用 `edge`（注意先把 `language` 改成目标语言，
  例如英文语音用 `en-US`）

启动日志会明确告诉你当前用的是哪个后端：

```
INFO livetranslate::app: ASR backend: edge (Edge ASR (en-US))
```

## VAD 语音分段

默认开启（`vad.enabled = true`），用**按语音边界切句**代替固定时长硬切：

```text
静音 ──┐            ┌──────────────┐
       │  确认开始   │   语音持续    │  确认结束
       └────────────┘              └───────────
         ↑ pre_pad_ms                ↑ post_pad_ms
```

- 语音需持续 `min_speech_ms` 才确认开始（过滤毛刺噪声）
- 静音需持续 `min_silence_ms` 才确认说完（句中停顿不会被切断）
- 超过 `max_speech_ms` 强制切分，避免延迟无限增长
- 开始前 / 结束后各保留一段音频，避免吃掉首尾音素

检测算法是**纯 Rust**的「自适应噪声底 + 迟滞」：实时估计环境噪声底，
只有明显高于它才算语音；噪声底只在非语音段更新，避免长句把自己的阈值抬高。

### 调参建议

| 现象 | 调整 |
|------|------|
| 噪声被当成语音（乱出字幕） | 调大 `margin_db`（如 10~12） |
| 说话声音小/听不到 | 调小 `margin_db`（如 6） |
| 句子仍被句中停顿切断 | 调大 `min_silence_ms`（如 600~800） |
| 延迟太大 | 调小 `max_speech_ms` |
| 首字/尾音丢失 | 调大 `pre_pad_ms` / `post_pad_ms` |

日志（`--log-level debug`）会周期性打印当前噪声底与阈值，便于对照调整：

```
DEBUG livetranslate::app: VAD 噪声底 ≈ -52.3 dBFS (speech 阈值 ≈ -44.3 dBFS)
```

> 若关闭 VAD，则回退到固定时长切片（`audio.chunk_seconds` + 重叠），
> 效果会明显变差，仅用于对比排查。

## 配置文件位置

程序按以下**优先级**查找配置（第一个存在的被使用）：

| 优先级 | 位置 | 适用场景 |
|--------|------|----------|
| 1 | `--config <路径>` | 显式指定（找不到会直接报错） |
| 2 | `<可执行文件目录>/config/default.toml` | Windows 便携包（解压即用） |
| 3 | `./config/default.toml` | 源码 / 开发模式下运行 |
| 4 | 用户配置目录 | 打包后的 `.app` / 安装版 |
| 5 | 内置默认值 | 都没有时（`api_key` 为占位符） |

**用户配置目录**由 `directories` 推导，首次运行会自动生成一份默认配置：

| 平台 | 路径 |
|------|------|
| macOS | `~/Library/Application Support/com.chinuno.LiveTranslate/config.toml` |
| Windows | `%APPDATA%\chinuno\LiveTranslate\config\config.toml` |
| Linux | `~/.config/livetranslate/config.toml` |

> 托盘菜单 → **打开配置目录** 可直接在文件管理器中打开该目录。

日志中会打印实际使用的配置路径，例如：
```
INFO livetranslate: Config file: config/default.toml
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
   ./target/release/livetranslate --log-level info
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
- **切换点击穿透**：开启/关闭鼠标穿透
- **退出**：退出程序

> ⚠️ 开启点击穿透后，浮窗本身不再接收鼠标事件，面板上的按钮会失效。
> 此时请通过**托盘菜单 → 切换点击穿透**，或快捷键 `Ctrl+Alt+T` 关闭穿透。

全局快捷键（任何窗口下都有效）：

| 快捷键 | 功能 |
|--------|------|
| `Ctrl+Alt+S` | 显示/聚焦浮窗 |
| `Ctrl+Alt+R` | 开始采集 |
| `Ctrl+Alt+E` | 停止采集 |
| `Ctrl+Alt+T` | 切换点击穿透 |
| `Ctrl+Alt+Q` | 退出程序 |

> 单击托盘图标可显示/隐藏浮窗；双击托盘图标可唤出浮窗。

### 命令行选项

```
USAGE:
    livetranslate [OPTIONS]

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
2024-09-28T10:30:45.123Z INFO livetranslate: Starting LiveTranslate Application
2024-09-28T10:30:45.456Z INFO livetranslate: Config loaded successfully
2024-09-28T10:30:46.789Z INFO livetranslate::audio::capture: Opening audio device: Device(0)
2024-09-28T10:30:47.012Z INFO livetranslate::audio::capture: Audio stream started
2024-09-28T10:30:50.234Z INFO livetranslate::asr: Sending audio to ASR: http://127.0.0.1:8765/v1/audio/transcriptions (size: 64000 bytes)
2024-09-28T10:30:51.567Z INFO livetranslate::asr: ASR result: Hello, everyone
2024-09-28T10:30:52.890Z INFO livetranslate::translate: Translated: Hello, everyone -> 大家好
2024-09-28T10:30:52.891Z INFO livetranslate: Subtitle: Hello, everyone => 大家好
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
