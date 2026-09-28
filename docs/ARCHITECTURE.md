# 项目结构与实现总结

## 📁 项目文件结构

```
livetranslate/
├── Cargo.toml                      # Rust 项目配置
├── Cargo.lock                      # 依赖版本锁定
├── README.md                       # 英文 README
├── README_CN.md                    # 中文 README
├── PLAN.md                         # 完整的技术方案文档
│
├── config/
│   └── default.toml               # 配置文件示例
│
├── src/
│   ├── main.rs                    # 程序入口
│   ├── app.rs                     # 应用核心逻辑
│   ├── config.rs                  # 配置管理
│   ├── error.rs                   # 错误类型定义
│   │
│   ├── audio/                     # 音频采集模块
│   │   ├── mod.rs
│   │   ├── capture.rs             # 音频采集（cpal）
│   │   ├── chunker.rs             # 音频分片和去重
│   │   └── wav.rs                 # WAV 编码
│   │
│   ├── asr/                       # 语音识别模块
│   │   └── mod.rs                 # Whisper 客户端
│   │
│   ├── translate/                 # 翻译模块
│   │   └── mod.rs                 # OpenAI 兼容客户端
│   │
│   ├── subtitle/                  # 字幕管理
│   │   └── mod.rs                 # 字幕状态管理
│   │
│   └── ui/                        # UI 模块（未来实现）
│       ├── mod.rs
│       └── overlay.rs             # 透明浮窗（计划中）
│
├── docs/
│   └── ARCHITECTURE.md            # 详细的架构文档
│
├── run.sh                         # Linux/macOS 启动脚本
├── run.bat                        # Windows 启动脚本
│
└── target/
    └── release/
        └── livetranslate           # 编译后的二进制文件
```

## 🏗️ 模块设计

### 1. audio/capture.rs - 音频采集

**职责**：从 Windows 音频设备采集音频流

**关键特性**：
- 使用 `cpal` 库进行跨平台音频采集
- 支持多种采样格式（F32、I16、U16）自动转换
- 异步数据推送通过 tokio mpsc 通道
- 支持设备枚举和选择

**关键函数**：
```rust
pub fn list_devices() -> AppResult<Vec<DeviceInfo>>
pub async fn new(device_name: &str, sample_rate: u32, channels: u16) -> AppResult<Self>
pub async fn next_chunk(&mut self) -> Option<Vec<f32>>
```

### 2. audio/chunker.rs - 音频分片

**职责**：将连续音频流切分为固定大小的块

**关键特性**：
- 支持固定分片长度（默认 2 秒）
- 支持重叠处理（25% 重叠 = 300-500ms）
- 静音检测和过滤
- 缓冲区管理

**关键算法**：
```
输入: 连续 PCM 流
→ 缓存到缓冲区
→ 当缓冲区 >= chunk_samples 时提取一个分片
→ 检查是否静音
  ├─ 是: 跳过
  └─ 否: 进行 ASR
```

### 3. audio/wav.rs - WAV 编码

**职责**：将 f32 PCM 数据编码为 WAV 格式

**处理流程**：
- f32 样本（范围 -1.0 到 1.0）→ i16（范围 -32768 到 32767）
- 添加 WAV 头部信息
- 返回完整的 WAV 二进制数据

### 4. asr/mod.rs - 语音识别客户端

**职责**：调用本地 Whisper 兼容 API 进行识别

**请求流程**：
```
WAV 数据
→ multipart/form-data 编码
→ POST 到 {base_url}{request_path}
  参数: file, model, language
→ JSON 响应解析
→ 返回识别文本
```

**关键特性**：
- 可配置 API base_url（不限于 localhost）
- 支持自定义请求路径
- 请求超时控制
- 错误处理和日志

### 5. translate/mod.rs - 翻译客户端

**职责**：调用 OpenAI 兼容 API 进行翻译

**请求流程**：
```
识别文本
→ 构造 Chat API 请求
  ├─ system: 翻译提示词
  └─ user: "Translate to {target_lang}: {text}"
→ POST 到 {base_url}/chat/completions
  Header: Authorization: Bearer {api_key}
→ JSON 响应解析
→ 返回翻译文本
```

**关键特性**：
- OpenAI API 完全兼容
- 支持 Azure OpenAI、LocalAI 等
- 可配置目标语言和提示词
- 温度参数设置为 0.2（降低幻觉）

### 6. subtitle/mod.rs - 字幕管理

**职责**：维护字幕历史和滚动显示

**数据结构**：
```rust
pub struct Subtitle {
    pub source: String,      // 原文
    pub translated: String,  // 译文
}

pub struct SubtitleState {
    history: VecDeque<Subtitle>,  // FIFO 字幕队列
    max_lines: usize,             // 最多显示行数
    show_source: bool,            // 是否显示原文
}
```

**关键方法**：
- `push(subtitle)` - 添加新字幕，自动维持最多 max_lines 条
- `get_text()` - 获取当前显示文本
- `clear()` - 清空历史

### 7. config.rs - 配置管理

**职责**：加载、验证和保存配置

**结构体**：
```rust
pub struct AppConfig {
    pub audio: AudioConfig,        // 音频设置
    pub asr: AsrConfig,           // ASR 设置
    pub translate: TranslateConfig, // 翻译设置
    pub subtitle: SubtitleConfig,  // 字幕设置
}
```

**配置加载顺序**：
1. 优先使用命令行指定的路径
2. 否则查找项目目录的 `config/default.toml`
3. 否则查找执行目录的 `config/default.toml`
4. 默认返回程序内置默认值

### 8. app.rs - 应用核心

**职责**：协调各个模块，管理数据流

**主循环流程**：
```
启动 → 初始化音频采集 → 后台异步处理循环
       
后台循环：
  1. 接收音频块 (from AudioCapture)
  2. 缓冲到 AudioChunker
  3. 当有完整分片时：
     ├─ 检查静音 → 跳过
     └─ 编码为 WAV
        ├─ 调用 ASR（Whisper）
        │  ├─ 失败 → 记录错误，继续
        │  └─ 成功 → 获取识别文本
        │     └─ 调用翻译（OpenAI）
        │        ├─ 失败 → 记录错误，继续
        │        └─ 成功 → 创建 Subtitle 对象
        │           └─ 通过 mpsc 通道发送
  4. 循环...

主线程：
  接收 Subtitle 通道消息 → 打印日志
```

## 🔄 数据流

```
┌──────────────────────────┐
│    Windows Audio Input    │
│  (Microphone/Loopback)    │
└────────────┬─────────────┘
             │ PCM f32 samples
             ▼
        ┌─────────────┐
        │AudioCapture │
        └────┬────────┘
             │ Vec<f32> chunks
             ▼
      ┌────────────────┐
      │ AudioChunker   │
      │  - Buffer      │
      │  - Chunk       │
      │  - Silence Det │
      └────┬───────────┘
           │ Vec<f32> frames
           ▼
    ┌──────────────────┐
    │  WAV Encoding    │
    └────┬─────────────┘
         │ Vec<u8> (WAV)
         ▼
   ┌──────────────────────────────┐
   │  Whisper HTTP API :8765      │
   │  POST /v1/audio/transcriptions│
   └────┬─────────────────────────┘
        │ text (识别结果)
        ▼
  ┌──────────────────────────────┐
  │  OpenAI Chat Completions API │
  │  POST /v1/chat/completions   │
  └────┬─────────────────────────┘
       │ text (翻译结果)
       ▼
  ┌──────────────────┐
  │  Subtitle {      │
  │    source,       │
  │    translated    │
  │  }               │
  └────┬─────────────┘
       │ mpsc channel
       ▼
  ┌──────────────────┐
  │   Main Thread    │
  │  (Console/UI)    │
  └──────────────────┘
```

## 🔑 关键设计决策

### 1. 异步架构

采用 `tokio` 异步运行时，使得：
- 音频采集不阻塞 ASR 和翻译调用
- 多个网络请求可以并发处理
- 响应式 UI 更新（未来浮窗实现时）

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
cargo build --release --target x86_64-pc-windows-gnu  # 跨平台编译
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

## 🔮 未来改进方向

### Phase 5: 浮窗实现
- [ ] egui/egui-winit 集成
- [ ] 透明窗口和置顶
- [ ] 拖动和配置保存

### Phase 6: 高级特性
- [ ] 字幕导出（SRT/VTT）
- [ ] 实时统计和分析
- [ ] 语言自动检测
- [ ] 多窗口支持
- [ ] 键盘快捷键

### Performance
- [ ] 内存池优化
- [ ] SIMD 加速（可选）
- [ ] GPU 推理支持（ONNX Runtime）

## 📖 参考文档

- [PLAN.md](../PLAN.md) - 完整的技术方案
- [README_CN.md](../README_CN.md) - 中文用户指南
- [cpal 文档](https://docs.rs/cpal/)
- [Whisper API](https://platform.openai.com/docs/guides/speech-to-text)
- [OpenAI Chat API](https://platform.openai.com/docs/api-reference/chat)
