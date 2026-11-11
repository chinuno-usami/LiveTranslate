# 项目结构与实现总结

> v0.2 变更摘要
> - `src/config.rs` → `src/config/{mod,writer,schema,profiles}.rs`
> - 新增 `src/output/{mod,osc,steamvr/*}.rs`（SteamVR / OSC 双输出，feature gate）
> - `app.rs`：单流水线 → 每源一条 `run_source`，事件带 `source_id/label`
> - `main.rs`：`SharedState.config` 改为 `RwLock`，新增 `SourceRegistry` / `OutputManager` / 设置页命令
> - 新增 `tauri-ui/settings.{html,js,css}` 独立设置窗口
> - 配置档：`profiles/<name>.toml` + `profiles.toml`（`config.toml` 始终权威）

## 📁 项目文件结构

```
livetranslate/
├── Cargo.toml                      # Rust 项目配置与 feature 定义（silero-vad / steamvr / osc / extras）
├── Cargo.lock                      # 依赖版本锁定
├── README.md                       # 用户指南
├── PLAN.md                         # 技术方案文档
│
├── config/
│   └── default.toml               # 开发与内置默认配置（打包脚本据此生成随包 config.example.toml）
│
├── tauri-ui/                      # 静态前端（无前端打包链）
│   ├── index.html / styles.css / app.js          # 桌面透明字幕浮窗
│   └── settings.html / settings.css / settings.js # 独立设置窗口
│
├── src/
│   ├── main.rs                    # 程序入口 / Tauri 命令 / SharedState / SourceRegistry
│   ├── app.rs                     # 单源流水线 run_source / 控制台与设备枚举辅助
│   ├── tray.rs                    # 系统托盘菜单与全局快捷键
│   ├── config/                    # 配置管理（结构体 / 写入 / schema / 配置档）
│   │   ├── mod.rs
│   │   ├── writer.rs              # toml_edit 定点写入 + 先校验后落盘 + 写入串行化
│   │   ├── schema.rs              # 设置页字段描述
│   │   └── profiles.rs            # 多配置档
│   ├── output/                    # 统一输出层
│   │   ├── mod.rs                 # OutputManager + 状态通知
│   │   ├── osc.rs                 # VRChat Chatbox（组包/限流/typing）
│   │   └── steamvr/               # Overlay 线程 + cosmic-text 渲染 + stub
│   ├── error.rs                   # 错误类型定义
│   │
│   ├── audio/                     # 音频采集与分段模块
│   │   ├── mod.rs
│   │   ├── capture.rs             # 音频采集（cpal / WASAPI Loopback）
│   │   ├── resample.rs            # 单声道下混与线性重采样
│   │   ├── vad.rs                 # 自适应噪声底能量 VAD
│   │   ├── silero.rs              # Silero VAD v6.2（ONNX Runtime 动态加载）
│   │   ├── detector.rs            # VAD 后端抽象与自动降级
│   │   ├── chunker.rs             # 固定时长分片回退路径
│   │   └── wav.rs                 # WAV 编码
│   │
│   ├── asr/                       # 语音识别模块
│   │   ├── mod.rs                 # OpenAI/Whisper 兼容客户端与幻听过滤
│   │   ├── edge.rs                # Edge 在线语音识别客户端
│   │   └── languages.rs           # 识别语言候选与后端归一化
│   │
│   ├── translate/                 # 翻译模块
│   │   └── mod.rs                 # OpenAI 兼容客户端与思考块剥离
│   │
│   ├── subtitle/                  # 字幕管理
│   │   ├── mod.rs                 # 多源字幕历史与来源标签格式化
│   │   └── processor.rs           # 去重 / 固定分片重叠裁剪
│   │
│   └── ui/                        # 前后端事件名与 IPC 载荷
│       ├── mod.rs
│       └── overlay.rs             # 浮窗相关载荷辅助
│
└── docs/
    ├── ARCHITECTURE.md            # 架构说明
    ├── BUILD_FLAVORS.md           # 标准版 / Full 版构建说明
    └── V0.2_FEATURE_PLAN.md       # v0.2 功能规划
```

## 🏗️ 模块设计

### 1. audio/capture.rs - 音频采集

**职责**：跨平台采集麦克风与系统回环（WASAPI Loopback）音频流

**关键特性**：
- 使用 `cpal` 进行麦克风采集，Windows 下额外提供 WASAPI Loopback 系统回环采集
- 支持多种采样格式（F32、I16、U16）自动转换与单声道下混重采样（`audio/resample.rs`）
- 异步数据推送通过 tokio mpsc 通道，停止时析构流句柄立即释放硬件占用

**关键函数**：
```rust
pub fn list_devices() -> AppResult<Vec<DeviceInfo>>
pub async fn new(device_spec: &str, sample_rate: u32, channels: u16) -> AppResult<Self>
pub async fn next_chunk(&mut self) -> Option<Vec<f32>>
```

### 2. audio/{detector,vad,silero,chunker}.rs - 语音活动检测与分段

**职责**：按自然语段切分连续音频流（VAD 优先，固定时长分片回退）

**关键特性**：
- 默认启用 VAD（`SpeechSegmenter`）：支持 `silero`（ONNX Runtime 动态加载 Silero VAD v6.2）与 `energy`（自适应噪声底能量阈值）双后端，初始化失败自动回退到能量 VAD
- 关闭 VAD 时回退到 `AudioChunker`（固定分片长度 + 25% 重叠 + 能量静音过滤 + 相邻片段文本去重）

### 3. audio/wav.rs - WAV 编码

**职责**：将 f32 PCM 数据编码为 16-bit 单声道 WAV 格式

**处理流程**：
- f32 样本（范围 -1.0 到 1.0）→ i16（范围 -32768 到 32767）
- 添加标准 RIFF/WAV 头部，供语音识别接口直接消费

### 4. asr/{mod,edge,languages}.rs - 语音识别客户端

**职责**：调用自建 Whisper 兼容 API 或微软 Edge 在线识别服务

**关键特性**：
- `whisper` 后端：`multipart/form-data` POST 到 `{base_url}{request_path}`，兼容 faster-whisper-server / whisper.cpp / OpenAI
- `edge` 后端：免部署在线语音识别，按后端规范化语言代码（`asr/languages.rs`）
- 内置静音/常见幻听短语过滤（`filter_hallucination`）

### 5. translate/mod.rs - 翻译客户端

**职责**：调用 OpenAI 兼容 Chat Completions API 进行翻译

**关键特性**：
- 兼容 OpenAI / DeepSeek / 本地推理服务，支持 `extra_body` 扩展参数透传
- 自动剥离推理模型 `<think>...</think>` 思考块，支持全局与单源独立的翻译开关

### 6. subtitle/mod.rs - 多源字幕管理

**职责**：维护多输入源滚动字幕历史与来源标识格式化

**核心结构**：
```rust
pub struct SubtitleLine {
    pub source_id: String,
    pub label: String,
    pub source: String,
    pub translated: String,
}

pub struct DisplayLine {
    pub source_id: String,
    pub label: Option<String>,
    pub source: String,
    pub translated: String,
    pub lines: Vec<String>,
    pub text: String,
}
```

**关键特性**：
- 保留各输出（桌面浮窗 / SteamVR / OSC）所需的最大历史窗口
- `LabelMode`（`Auto` / `Always` / `Never`）在多源并发或历史混排时自动附加 `[来源]` 标签

### 7. config/ - 配置管理与多配置档

**职责**：加载、校验、保留注释定点写回及多配置档切换

**结构体**：
```rust
pub struct AppConfig {
    pub audio: AudioConfig,         // 音频与多输入源设置
    pub vad: VadConfig,             // Energy / Silero VAD 分段设置
    pub asr: AsrConfig,             // OpenAI 兼容 / Edge 识别设置
    pub translate: TranslateConfig, // 翻译与思考控制设置
    pub subtitle: SubtitleConfig,   // 桌面浮窗与字幕排版设置
    pub output: OutputConfig,       // SteamVR / OSC 外部输出设置
}
```

**配置加载顺序**：
1. 命令行 `--config <path>` 显式指定的路径（失败即直接报错退出）
2. 可执行文件同级的 `config/default.toml`（便携模式）
3. 当前工作目录的 `./config/default.toml`（源码开发模式）
4. 系统用户配置目录的 `config.toml`（不存在则自动生成默认配置；损坏则隔离备份并恢复）
5. 程序内置默认值（`AppConfig::default()`）

### 8. app.rs & output/ - 多源流水线与统一输出层

**职责**：每路启用输入源独立运行 `run_source`，汇总事件后分发到桌面与 VR 输出

## 🔄 数据流

```
┌────────────────────────────┐
│  多路音频输入源 (1..N)      │
│  Microphone / Loopback     │
└─────────────┬──────────────┘
              │ 每源独立 run_source 任务
              ▼
   ┌──────────────────────┐
   │ AudioCapture + 重采样 │
   └──────────┬───────────┘
              ▼
   ┌──────────────────────┐
   │ SpeechSegmenter(VAD) │
   │  / AudioChunker 回退 │
   └──────────┬───────────┘
              │ 语段队列 (utterance mpsc)
              ▼
   ┌──────────────────────┐
   │ ASR (Whisper / Edge) │
   │ + 可选 LLM 翻译      │
   └──────────┬───────────┘
              │ PipelineEvent::Subtitle
              ▼
   ┌──────────────────────┐
   │    SubtitleState     │
   │ (多源历史与标签排版) │
   └────┬─────────┬───────┘
        │         └──────────────────────────┐
        ▼                                    ▼
┌───────────────────┐            ┌───────────────────────┐
│ Tauri 桌面透明浮窗 │            │     OutputManager     │
│  (subtitle://...) │            │ ├─ SteamVR Overlay    │
└───────────────────┘            │ └─ VRChat OSC Chatbox │
                                 └───────────────────────┘
```

## 🔑 关键设计决策

### 1. 异步架构

采用 `tokio` 异步运行时，使得：
- 音频采集不阻塞 ASR 和翻译调用
- 多个网络请求可以并发处理
- 桌面浮窗与设置页通过 Tauri 事件 / IPC 做响应式更新

### 2. 音频分片策略

**问题**：固定 2 秒分片可能截断一句话

**解决方案**：
```
分片长度: 2s
重叠比例: 25%
重叠时长: 500ms

时间轴:
0s ─────── 2s ─────── 4s ─────── 6s
├─ Chunk1 ──┤
      └─ Chunk2 (overlap 500ms) ──┤
             └─ Chunk3 ──┤

优点:
- 一句话跨片时，后一片仍能补齐前面的内容
- ASR 后通过相邻文本去重避免重复显示
```

### 3. 错误恢复

所有外部调用（网络、设备）都有独立的错误处理：
- ASR 失败 → 记录日志，继续处理下一片
- 翻译失败 → 记录日志，继续处理下一片
- 音频设备失败 → 退出程序（无法恢复）

### 4. 配置外部化

所有 API 地址、模型、参数都可通过 TOML 配置：
- 无需重新编译即可切换 ASR 后端
- 支持多种翻译服务
- 便于部署和定制

## 📊 性能特性

### 内存占用

- 音频缓冲区：~500KB（16kHz, 16bit, 2s 分片）
- 请求队列：~10KB（默认 128 缓冲区）
- 字幕历史：~50KB（3 行，每行 500 字符）
- **总计**：< 2 MB

### 延迟

- 音频采集 → ASR：1-2 秒
- ASR 响应：1-3 秒（取决于硬件）
- ASR 输出 → 翻译 API：< 100ms
- 翻译响应：1-3 秒（取决于网络和模型）
- **端到端延迟**：4-8 秒

### 吞吐量

- 单线程音频采集：无限制（实时）
- 单线程 ASR：连续处理
- 单线程翻译：连续处理
- 整体：~1 条字幕 / 2-4 秒

## 🚀 编译和部署

### 开发编译
```bash
cargo check        # 快速检查
cargo build        # Debug 编译
cargo run          # 直接运行
```

### 发布编译
```bash
cargo build --release    # 优化编译（3-5x 快）
cargo xwin build --release --target x86_64-pc-windows-msvc  # 跨平台编译（推荐）
```

### 依赖大小
```
cpal           - 音频采集
reqwest        - HTTP 客户端
tokio          - 异步运行时
serde          - 序列化
hound          - WAV 编码
tracing        - 日志系统
windows        - Win32 API（可选）
```

## 🔮 已完成里程碑与未来改进方向

### v0.1 – v0.2 已完成 ✅
- [x] Tauri 透明无边框桌面字幕浮窗、点击穿透、位置尺寸持久化
- [x] 独立设置窗口（Schema 驱动表单 + 实时预览 + 原始 TOML + 多配置档）
- [x] 多输入源并行采集、Silero / Energy 双 VAD 后端、Edge / Whisper 双 ASR 后端
- [x] SteamVR Overlay 与 VRChat OSC Chatbox 输出通道（按 feature gate 拆包）
- [x] 系统托盘菜单与全局快捷键

### 后续演进方向
- [ ] 字幕导出（SRT/VTT）
- [ ] 实时统计和延迟分析
- [ ] 内存池复用与音频重采样进一步优化
- [ ] GPU 推理支持（ONNX Runtime EP 可配置）

## 📖 参考文档

- [PLAN.md](../PLAN.md) - 完整的技术方案
- [README.md](../README.md) - 用户指南
- [cpal 文档](https://docs.rs/cpal/)
- [Whisper API](https://platform.openai.com/docs/guides/speech-to-text)
- [OpenAI Chat API](https://platform.openai.com/docs/api-reference/chat)
