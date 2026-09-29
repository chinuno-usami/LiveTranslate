# 模型文件

## silero_vad.onnx

用于 `[vad] backend = "silero"` 的语音活动检测模型。

- 来源: [snakers4/silero-vad](https://github.com/snakers4/silero-vad)（`master` / v6.2，sha256 `1a153a22…8788e3`）
- 许可证: MIT
- 输入: `input` [1, 576] float32（16kHz 单声道）+ `state` [2, 1, 128] + `sr` int64
- 输出: `output` [1, 1] 语音概率 + `stateN` [2, 1, 128]

> ⚠️ v6.2 的 `input` **必须**是 `64 样本 context + 512 新样本 = 576`（8kHz 为 `32 + 256 = 288`）。
> context 取上一帧模型输入的最后 64（8k 为 32）个样本，逐帧滚动。
> 若只喂 512（不带 context），模型会退化为恒输出 ~0 的概率，导致“有人说话也检测不到”。
> 实现见 `src/audio/silero.rs`。

若模型文件缺失，或运行环境没有 ONNX Runtime，
`silero` 后端会自动回退到内置的能量 VAD 并在面板上给出提示。
