# 实时语音识别翻译字幕程序计划

## 1. 目标

开发一个主要运行在 **Windows** 上的 Rust 程序：

- 从**指定音频输入源**采集音频
  - 支持：系统麦克风 / 指定输入设备
  - 支持：Windows 系统回环采集（桌面音频 / WASAPI loopback）
- 将音频分片发送到**可配置 base URL** 的 **Whisper 兼容 HTTP API** 做 ASR
- 将 ASR 文本发送到 **OpenAI 兼容 API** 的 LLM 做翻译
- 把翻译结果以**透明浮窗字幕**形式实时显示在屏幕上
- 翻译目标语言默认 **中文（zh-CN）**
- 支持基础配置、日志、错误恢复与低延迟显示

---

## 2. 需求拆解

### 2.1 功能需求

1. **音频输入选择**
   - 枚举可用输入设备
   - 允许通过配置或启动参数选择设备
   - 支持默认输入设备
   - 支持 Windows 系统回环采集（扬声器输出捕获）

2. **实时采集与分片**
   - 使用固定时长音频块（建议 1~3 秒）
   - 对音频做必要格式转换（采样率 / 声道 / PCM）
   - 控制缓存，避免延迟不断累积

3. **ASR 调用**
   - 对接可配置 `base_url` 的 Whisper 兼容 API
   - 默认可填写 `http://127.0.0.1:8765`
   - 支持 multipart/form-data 上传音频片段
   - 解析识别结果
   - 处理超时、失败重试、空结果过滤

4. **翻译调用**
   - 对接 OpenAI 兼容 API
   - 支持自定义：
     - `base_url`
     - `api_key`
     - `model`
   - 翻译 prompt 可配置
   - 仅翻译增量识别结果，避免重复输出

5. **字幕浮窗**
   - 透明背景
   - 置顶显示
   - 无边框
   - 可拖动 / 可配置位置
   - 支持字体大小、颜色、描边/阴影、最大行数
   - 可配置是否显示原文
   - 新字幕到来时刷新显示

6. **程序控制**
   - 启动 / 停止采集
   - 显示当前设备、ASR 状态、翻译状态
   - 退出时优雅关闭任务与窗口

7. **配置管理**
   - 配置文件保存 API、设备名、窗口样式、翻译参数
   - 首次启动自动生成默认配置

---

## 3. 非功能需求

- **平台重点**：Windows 10/11
- **低延迟**：目标端到端延迟控制在 2~5 秒内
- **稳定性**：ASR/翻译失败不导致主进程退出
- **可维护性**：模块化设计，便于后续扩展 OCR / TTS / 多语言
- **资源占用可控**：常驻运行时 CPU / 内存不能明显过高

---

## 4. 技术方案

## 4.1 推荐技术栈

- **语言**：Rust stable
- **异步运行时**：`tokio`
- **HTTP 客户端**：`reqwest`
- **序列化**：`serde`, `serde_json`
- **音频采集**：`cpal`
- **WAV 封装**：`hound`
- **配置**：`toml`, `directories`, `clap`（可选）
- **日志**：`tracing`, `tracing-subscriber`
- **GUI / 浮窗**：优先考虑原生 Rust 方案：
  1. `eframe/egui` + 透明 viewport
  2. `winit` + `windows`/Win32 API 实现原生透明窗口
- **Windows 音频回环**：优先考虑
  1. `wasapi` crate
  2. 或 `windows` crate 直接调用 WASAPI loopback

### GUI 方案建议

优先建议：**原生 Rust 透明窗口**

推荐路线：**`eframe/egui` + 必要的 Win32 API 补充**

原因：
- 基本仍属于原生 Rust 实现
- UI 开发速度比纯 Win32 更快
- 后续可扩展设置面板、托盘、调试视图
- 在 Windows 上可逐步补齐透明、置顶、穿透、拖动等能力

更底层备选：**`winit` + Win32 API**
- 优点：控制力最强
- 缺点：开发成本更高，文本排版和样式要自己处理更多细节

> 第一版建议先做：**Rust 原生后端 + 原生透明字幕窗口**，避免引入 WebView/Tauri。

---

## 5. 系统架构

```text
┌─────────────────────────────────────────────┐
│                 Rust 主程序                 │
├─────────────────────────────────────────────┤
│  配置模块  │  日志模块  │  状态管理模块      │
├─────────────────────────────────────────────┤
│               音频采集模块                  │
│      设备枚举 / 指定设备 / PCM 缓冲         │
├─────────────────────────────────────────────┤
│               音频分片模块                  │
│   重采样 / 单声道化 / 分段 / WAV 编码       │
├─────────────────────────────────────────────┤
│               ASR 客户端模块                │
│    Whisper-compatible HTTP API :8765       │
├─────────────────────────────────────────────┤
│               翻译客户端模块                │
│      OpenAI-compatible Chat/Responses       │
├─────────────────────────────────────────────┤
│               字幕聚合模块                  │
│   去重 / 合并 / 限长 / 历史滚动 / 节流      │
├─────────────────────────────────────────────┤
│               字幕显示模块                  │
│   透明浮窗 / 置顶 / 样式 / 实时刷新         │
└─────────────────────────────────────────────┘
```

---

## 6. 模块设计

### 6.1 audio_capture
职责：
- 枚举输入设备
- 打开指定输入流
- 支持 Windows WASAPI loopback
- 输出统一 PCM 数据流

关键点：
- 设备选择支持 `default` / `microphone` / `loopback` / `mic:N` / `loopback:N` / 索引 / 名称模糊匹配
- Windows 下枚举输出设备作为 loopback 源，cpal 0.18 打开输出设备时自动启用 loopback
- 回调中下混为单声道，消费端再做重采样（`audio/resample.rs`）
- 处理不同采样率/样本格式，统一转成 f32

> 实现状态：已完成（设备选择 + 下混 + 重采样），需在真实 Windows 环境验证 loopback。

### 6.2 audio_chunker
职责：
- 将连续音频切为固定长度片段
- 通过“固定分片 + 轻量重叠”减少一句话被截断的问题
- 可选静音检测（VAD-lite）减少空白上传

关键点：
- 第一版先不做复杂 VAD，但不建议完全生硬切片
- 建议 1.5~2 秒分片 + 300~500ms overlap
- 可基于音量阈值过滤纯静音片段
- 在 ASR 后做相邻文本去重/拼接，降低重叠带来的重复输出

> 实现状态：已完成。`subtitle/processor.rs` 提供相似度去重 + 重叠前缀移除，
> 同时支持空格分词语言与中文等无空格语言（字符级重叠，最少 3 字符）。

### 6.3 asr_client
职责：
- 调用本机 Whisper 兼容接口
- 提交音频文件并读取识别文本

待确认接口：
- 路径是否兼容 `/v1/audio/transcriptions`
- 字段名是否标准 Whisper 风格（如 `file`, `model`, `language`）
- 返回结构是否与 OpenAI Whisper 兼容

### 6.4 translator
职责：
- 把识别文本翻译为目标语言（默认中文）
- 避免重复翻译同一文本

策略：
- 做短文本实时翻译
- 为减少上下文漂移，默认按单段翻译
- 可附加最近 1~2 条历史作为辅助上下文

### 6.5 subtitle_state
职责：
- 维护最近几条字幕
- 做去重、合并、滚动显示

显示建议：
- 保留 2~4 行字幕
- 新字幕追加到底部
- 超长文本自动换行
- 可配置显示“仅译文”或“原文+译文”

### 6.6 overlay_window
职责：
- 透明浮窗显示
- 接收后端推送的字幕文本并刷新

Windows 目标特性：
- always on top
- transparent background
- borderless
- 可选 click-through
- 可记忆窗口位置

---

## 7. 数据流设计

```text
音频设备
  ↓
连续 PCM 流
  ↓
音频分片（1~3秒）
  ↓
静音过滤
  ↓
上传到本地 Whisper API
  ↓
ASR 文本
  ↓
去重 / 清洗
  ↓
发送到 OpenAI 兼容翻译接口
  ↓
翻译文本
  ↓
字幕状态更新
  ↓
透明浮窗刷新
```

---

## 8. 关键接口设计

## 8.1 配置文件示例

```toml
[audio]
device_name = "default"
chunk_seconds = 2
sample_rate = 16000
channels = 1
silence_threshold = 0.01

[asr]
base_url = "http://127.0.0.1:8765"
model = "whisper-1"
language = "auto"
timeout_secs = 20
request_path = "/v1/audio/transcriptions"

[translate]
base_url = "https://api.example.com/v1"
api_key = "YOUR_API_KEY"
model = "gpt-4o-mini"
target_language = "zh-CN"
system_prompt = "You are a real-time subtitle translator. Translate naturally and concisely."
timeout_secs = 20

[subtitle]
max_lines = 3
font_size = 28
text_color = "#FFFFFF"
stroke_color = "#000000"
background = "transparent"
show_source = false
window_width = 1200
window_height = 220
position_x = 200
position_y = 760
always_on_top = true
click_through = false
```

## 8.2 ASR 请求预期

优先按 OpenAI Whisper 兼容格式设计：

- `POST /v1/audio/transcriptions`
- `multipart/form-data`
  - `file`: wav 文件
  - `model`: whisper 模型名
  - `language`: 可选

预期返回：

```json
{
  "text": "hello world"
}
```

## 8.3 翻译请求预期

优先按 OpenAI Chat Completions 兼容格式：

- `POST /chat/completions` 或 `/v1/chat/completions`
- Header: `Authorization: Bearer <api_key>`

请求体示例：

```json
{
  "model": "gpt-4o-mini",
  "messages": [
    {
      "role": "system",
      "content": "You are a real-time subtitle translator. Translate naturally and concisely."
    },
    {
      "role": "user",
      "content": "Translate to zh-CN: Hello, everyone."
    }
  ],
  "temperature": 0.2
}
```

---

## 9. 开发阶段计划

## Phase 1：项目骨架
目标：完成基础工程初始化

任务：
- 创建 Rust workspace / binary 项目
- 加入配置、日志、错误处理基础设施
- 定义模块目录结构
- 提供示例配置文件

交付：
- 可启动空程序
- 能正确读取配置并打印状态

## Phase 2：音频采集 ✅ 已完成
目标：从指定输入设备或系统回环稳定获取音频

任务：
- 枚举输入设备
- 支持默认设备/指定设备
- 支持 Windows WASAPI loopback
- 建立采集流
- 输出标准 PCM 数据
- 下混为单声道 + 重采样到 16kHz

交付：
- 命令行可看到设备列表（含麦克风/回环分类）
- 可在浮窗下拉框切换设备
- 可在配置文件中指定具体设备

## Phase 3：ASR 集成
目标：接通本地 Whisper 兼容服务

任务：
- 音频分片
- WAV 编码
- 调用 `127.0.0.1:8765`
- 解析识别文本
- 增加失败重试与超时控制

交付：
- 能将输入音频转写成文本输出到控制台

## Phase 4：翻译集成
目标：接通 OpenAI 兼容翻译接口

任务：
- 封装通用 OpenAI 兼容客户端
- 实现翻译 prompt
- 处理空文本、重复文本、异常文本

交付：
- 控制台可输出“原文 + 译文”

## Phase 5：字幕浮窗
目标：完成透明字幕显示

任务：
- 建立原生透明无边框置顶窗口
- 接收后端事件刷新字幕
- 完成基础排版与样式
- 支持配置是否显示原文

交付：
- 屏幕显示实时更新字幕

## Phase 6：联调与体验优化
目标：形成可用 MVP

任务：
- 调整分片时长
- 优化字幕滚动策略
- 增加静音过滤
- 改善错误提示与日志
- 打包 Windows 可执行程序

交付：
- Windows 上可运行的 MVP 版本

---

## 10. 目录建议

```text
livetranslate/
├─ src/
│  ├─ main.rs
│  ├─ app.rs
│  ├─ config.rs
│  ├─ error.rs
│  ├─ audio/
│  │  ├─ mod.rs
│  │  ├─ capture.rs
│  │  ├─ chunker.rs
│  │  └─ wav.rs
│  ├─ asr/
│  │  ├─ mod.rs
│  │  └─ whisper_client.rs
│  ├─ translate/
│  │  ├─ mod.rs
│  │  └─ openai_client.rs
│  ├─ subtitle/
│  │  ├─ mod.rs
│  │  ├─ state.rs
│  │  └─ formatter.rs
│  └─ ui/
│     └─ overlay.rs
├─ assets/
├─ config/
│  └─ default.toml
├─ docs/
└─ PLAN.md
```

> 如果采用 Tauri，则会增加：

```text
├─ src-tauri/
├─ ui/          # 前端字幕界面
```

---

## 11. 风险与待确认项

### 11.1 Windows 音频源类型
已确认首版需要同时支持：
- 麦克风输入
- 系统回环音频（扬声器输出捕获）

> 回环采集建议通过 **WASAPI loopback** 实现，并尽早单独验证。

### 11.2 本地 Whisper 服务兼容度
需要确认：
- 是否完全兼容 OpenAI Whisper API
- 是否支持 wav 上传
- 推荐音频采样率
- 最大单次音频时长

### 11.3 字幕窗口技术选型
已确认优先：
- 尽量纯 Rust 原生 GUI 方案
- 避免 Tauri/WebView 依赖

### 11.4 实时翻译粒度
要平衡：
- 延迟
- 语义完整性

建议首版：
- 1.5~2 秒分片
- 300~500ms overlap
- 单片识别后立即翻译
- 相邻片段做文本去重与句子合并策略

---

## 12. MVP 范围建议

首版只做以下能力：

- Windows 运行
- 指定麦克风设备输入
- 支持系统回环采集
- 固定 1.5~2 秒音频分片 + 轻量重叠
- 调用可配置 base URL 的 Whisper HTTP API 做识别
- 调用 OpenAI 兼容 API 做翻译（默认翻译到中文）
- 透明置顶字幕浮窗显示最近 2~3 行
- 可配置显示仅译文或原文+译文
- 基于 TOML 配置文件运行

**暂不纳入首版：**
- 复杂 VAD
- 多窗口设置界面
- 历史字幕导出
- OCR / TTS

---

## 13. 下一步实施建议

建议按以下顺序开始实现：

1. 初始化 Rust 项目与配置结构
2. 完成设备枚举与测试录音
3. 对接本地 Whisper API
4. 对接 OpenAI 兼容翻译 API
5. 最后接入透明字幕浮窗

---

## 14. 验收标准

满足以下条件可视为首版完成：

- 在 Windows 上可选择或指定一个输入设备
- 程序能持续采集音频并分片
- 每个片段可被本地 Whisper 服务成功识别
- 识别文本可被 OpenAI 兼容接口成功翻译
- 翻译结果能以透明字幕浮窗持续显示
- 当网络/API/ASR 局部失败时程序不会直接崩溃

---

## 15. 建议补充信息

进入编码前，最好再确认以下内容：

1. `8765` Whisper 服务的**具体接口示例**与兼容程度
2. 系统回环采集目标环境（Windows 10/11、常见声卡）
3. 字幕默认样式与是否需要 click-through
4. 是否需要优先支持“仅译文”还是“双语字幕”模板
5. OpenAI 兼容翻译服务的接口路径是 `/v1/chat/completions` 还是其他变体

---

如果你确认这份计划，我下一步可以直接开始：

1. 初始化 Rust 项目骨架
2. 先实现配置 + 音频设备枚举 + 录音测试
3. 再逐步接入 ASR / 翻译 / 浮窗
