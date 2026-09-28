# LiveTranslate

实时语音识别 + 翻译 + 透明浮窗字幕。

Rust 实现的实时音频识别、翻译和字幕显示系统。

## 特性

- 支持 Windows 麦克风和系统回环音频采集
- 调用本地 Whisper 兼容 API 进行语音识别
- 使用 OpenAI 兼容 API 进行实时翻译
- 透明浮窗字幕显示
- 系统托盘与全局快捷键
- TOML 配置文件支持

## 快速开始

### 系统要求

- Windows 10/11
- Rust 1.70+

### 安装

```bash
cargo build --release
```

### 配置

编辑 `config/default.toml` 配置 API 和音频设备。

### 运行

```bash
cargo run --release
```

## 文档

详见 `PLAN.md` 了解完整的技术方案和开发计划。
