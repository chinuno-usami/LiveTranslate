# 模型文件

## silero_vad.onnx

用于 `[vad] backend = "silero"` 的语音活动检测模型。

- 来源: [snakers4/silero-vad](https://github.com/snakers4/silero-vad)（Silero VAD v5）
- 许可证: MIT
- 输入: `input` [1, 512] float32（16kHz 单声道）+ `state` [2, 1, 128] + `sr` int64
- 输出: `output` [1, 1] 语音概率 + `stateN` [2, 1, 128]

若模型文件缺失，或运行环境没有 ONNX Runtime，
`silero` 后端会自动回退到内置的能量 VAD 并在面板上给出提示。
