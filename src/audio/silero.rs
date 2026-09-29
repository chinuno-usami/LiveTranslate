//! 基于 Silero VAD 的语音活动检测（ONNX Runtime）
//!
//! 相比内置的能量 VAD，Silero 是训练出来的语音分类模型，
//! 能区分「音乐 / 噪声」与「人声」，也能在低信噪比下工作。
//!
//! 模型为 Silero VAD v6.2（MIT 许可），已内嵌进二进制；
//! 也可通过 `[vad] silero_model` 指定外部模型文件。
//!
//! 运行期需要 ONNX Runtime 动态库（`onnxruntime`）：
//! 可用 `ORT_DYLIB_PATH` 指定路径，或放到系统库搜索路径里。
//! 加载失败时上层会自动回退到能量 VAD。

use ort::session::Session;
use ort::value::Tensor;

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// 内嵌的 Silero VAD v6.2 模型（models/silero_vad.onnx）
const EMBEDDED_MODEL: &[u8] = include_bytes!("../../models/silero_vad.onnx");

/// LSTM 隐状态维度（模型固定值）
const STATE_DIM: usize = 128;

/// Silero v6.2 需要把上一帧末尾的 context 拼到本帧前面再送入 ONNX。
///
/// 16kHz：context 64 + 新样本 512 = 模型输入 576；
/// 缺少 context 时模型输出会退化成恒 ~0 的概率（有人说话也检测不到）。
const CONTEXT_LEN_16K: usize = 64;
/// 8kHz：context 32 + 新样本 256 = 模型输入 288
const CONTEXT_LEN_8K: usize = 32;

/// 退出门槛相对进入门槛的下调量（迟滞）
///
/// 上游 Silero 用 `neg_threshold = threshold - 0.15`：概率在门槛附近
/// 抖动时不把一句话切碎/漏掉，比单一阈值稳定得多。
const NEG_THRESHOLD_MARGIN: f32 = 0.15;

/// 概率观测窗口长度（帧）。32ms/帧 时约 2s。
const PROB_WINDOW: usize = 64;

/// 单步迟滞判决
///
/// - 非语音态：`prob >= threshold` 才进入语音
/// - 语音态：`prob >= neg_threshold` 就继续保持（更宽松）
///
/// 独立成函数是为了能在没有 ONNX Runtime 的环境下做单测。
fn hysteresis_step(active: bool, prob: f32, threshold: f32, neg_threshold: f32) -> bool {
    if active {
        prob >= neg_threshold
    } else {
        prob >= threshold
    }
}

/// 各平台 ONNX Runtime 动态库文件名
#[cfg(target_os = "windows")]
const ORT_LIB_NAME: &str = "onnxruntime.dll";
#[cfg(target_os = "macos")]
const ORT_LIB_NAME: &str = "libonnxruntime.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
const ORT_LIB_NAME: &str = "libonnxruntime.so";

pub struct SileroVad {
    session: Session,
    /// LSTM 状态 [2, 1, 128]
    state: Vec<f32>,
    /// 上一帧末尾的上下文样本（16k 为 64，8k 为 32）
    context: Vec<f32>,
    /// 未凑满一帧的剩余样本
    buffer: Vec<f32>,
    /// 每步新增的样本数（16k 为 512，8k 为 256）
    frame_len: usize,
    /// 送入模型前拼接的上下文样本数（见 [`CONTEXT_LEN_16K`] / [`CONTEXT_LEN_8K`]）
    context_len: usize,
    sample_rate: i64,
    /// 进入语音的门槛
    threshold: f32,
    /// 离开语音的门槛（迟滞，低于它才算静音）
    neg_threshold: f32,
    /// 当前是否处于语音段（迟滞状态）
    active: bool,
    /// 最近 [`PROB_WINDOW`] 帧的概率，用于 debug 观测
    prob_window: VecDeque<f32>,
}

impl SileroVad {
    /// 每步新增的样本数（16k 为 512，8k 为 256）
    ///
    /// 注意：这不是模型的实际输入长度——实际输入还要再拼上 `context_len`
    /// 个历史样本（见 [`Self::context_len_for`]）。
    pub fn frame_len_for(sample_rate: u32) -> AppResult<usize> {
        match sample_rate {
            16_000 => Ok(512),
            8_000 => Ok(256),
            other => Err(AppError::Audio(format!(
                "Silero VAD 仅支持 8kHz / 16kHz 采样率，当前配置为 {other} Hz"
            ))),
        }
    }

    /// 送入模型前需要拼接的 context 样本数：16k 为 64，8k 为 32
    fn context_len_for(sample_rate: u32) -> AppResult<usize> {
        match sample_rate {
            16_000 => Ok(CONTEXT_LEN_16K),
            8_000 => Ok(CONTEXT_LEN_8K),
            other => Err(AppError::Audio(format!(
                "Silero VAD 仅支持 8kHz / 16kHz 采样率，当前配置为 {other} Hz"
            ))),
        }
    }

    pub fn new(config: &crate::config::VadConfig, sample_rate: u32) -> AppResult<Self> {
        // 允许把 ONNX Runtime 随包分发：先寻找程序目录旁的动态库
        ensure_ort_library_env();

        // 仅“能 dlopen”不足以判断可用：系统里可能存在 API 版本不兼容的
        // onnxruntime，此时 ort 会在初始化时 panic，并把其内部互斥锁毒化；
        // 进程退出时 ort 的 atexit 钩子会再次 panic（无法 unwind）直接 abort。
        // 所以这里先做一次**功能性**探测，不通过就完全不碰 ort。
        let ort_version = match probe_onnxruntime() {
            Ok(version) => version,
            Err(why) => {
                return Err(AppError::Audio(format!(
                    "ONNX Runtime 不可用：{why}。\
                     可按 README 把动态库放到程序同级目录，或用 ORT_DYLIB_PATH 指定路径"
                )));
            }
        };
        tracing::info!("ONNX Runtime {} 就绪", ort_version);

        // ort 内部仍有可能 panic（例如模型无法加载），
        // 这里兜住以保证回退到能量 VAD 而不是拖崩程序。
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::create(config, sample_rate)
        })) {
            Ok(result) => result,
            Err(_) => Err(AppError::Audio(
                "初始化 Silero 时发生 panic（多半是模型或运行时异常）".to_string(),
            )),
        }
    }

    fn create(config: &crate::config::VadConfig, sample_rate: u32) -> AppResult<Self> {
        let frame_len = Self::frame_len_for(sample_rate)?;
        let context_len = Self::context_len_for(sample_rate)?;

        let model_path = config
            .silero_model
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty());

        // Silero 模型极小，默认让 ORT 把每次推理 fan-out 到所有核心会带来
        // 大量线程调度 / spin 开销（实测 CPU 可达单线程的 3~4 倍），
        // 而单线程的墙钟耗时几乎一样，因此这里固定为单线程。
        let build_cpu_session = |path: Option<&str>| -> AppResult<Session> {
            let builder = Session::builder()
                .map_err(|e| AppError::Audio(format!("创建 ONNX Runtime SessionBuilder 失败: {e}")))?;
            let builder = builder
                .with_intra_threads(1)
                .map_err(|e| AppError::Audio(format!("设置 intra 线程数失败: {e}")))?;
            let mut builder = builder
                .with_inter_threads(1)
                .map_err(|e| AppError::Audio(format!("设置 inter 线程数失败: {e}")))?;

            match path {
                Some(path) => builder
                    .commit_from_file(path)
                    .map_err(|e| AppError::Audio(format!("加载 Silero 模型 `{path}` 失败: {e}"))),
                None => builder
                    .commit_from_memory(EMBEDDED_MODEL)
                    .map_err(|e| AppError::Audio(format!("加载内置 Silero 模型失败: {e}"))),
            }
        };

        let session = build_cpu_session(model_path)?;

        let threshold = config.silero_threshold.clamp(0.0, 1.0);
        let neg_threshold = (threshold - NEG_THRESHOLD_MARGIN).max(0.01);

        tracing::info!(
            "Silero VAD 已加载 (input={} samples = {} context + {} new @ {}Hz, threshold={:.2} 进 / {:.2} 出)",
            context_len + frame_len,
            context_len,
            frame_len,
            sample_rate,
            threshold,
            neg_threshold
        );

        Ok(Self {
            session,
            state: vec![0.0; 2 * STATE_DIM],
            context: vec![0.0; context_len],
            buffer: Vec::with_capacity(frame_len * 2),
            frame_len,
            context_len,
            sample_rate: sample_rate as i64,
            threshold,
            neg_threshold,
            active: false,
            prob_window: VecDeque::with_capacity(PROB_WINDOW),
        })
    }

    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// 输入一帧样本，返回是否属于语音
    ///
    /// 传入长度可以不等于 `frame_len`（会自动攒够），
    /// 返回值对应当前处理到最后一个完整帧的判定。
    pub fn is_speech(&mut self, frame: &[f32]) -> bool {
        self.buffer.extend_from_slice(frame);

        let mut speech = self.active;
        while self.buffer.len() >= self.frame_len {
            let chunk: Vec<f32> = self.buffer.drain(..self.frame_len).collect();

            match self.infer(&chunk) {
                Ok(probability) => {
                    self.remember_probability(probability);
                    // 迟滞：进入用 threshold，退出用更低的 neg_threshold，
                    // 避免概率在门槛附近抖动导致整句漏检 / 被切碎
                    self.active = hysteresis_step(
                        self.active,
                        probability,
                        self.threshold,
                        self.neg_threshold,
                    );
                    speech = self.active;
                }
                Err(e) => {
                    tracing::warn!("Silero 推理失败: {e}");
                    speech = self.active;
                }
            }
        }
        speech
    }

    fn remember_probability(&mut self, probability: f32) {
        if self.prob_window.len() == PROB_WINDOW {
            self.prob_window.pop_front();
        }
        self.prob_window.push_back(probability);
    }

    /// 最近 [`PROB_WINDOW`] 帧的概率统计，供 `--log-level debug` 调参观测
    ///
    /// 返回 `(max, avg, threshold, neg_threshold, active)`；
    /// 还没推理过时概率按 0 计。
    pub fn probability_stats(&self) -> (f32, f32, f32, f32, bool) {
        let (max, avg) = if self.prob_window.is_empty() {
            (0.0, 0.0)
        } else {
            let max = self.prob_window.iter().copied().fold(f32::MIN, f32::max);
            let avg = self.prob_window.iter().sum::<f32>() / self.prob_window.len() as f32;
            (max, avg)
        };
        (max, avg, self.threshold, self.neg_threshold, self.active)
    }

    fn infer(&mut self, chunk: &[f32]) -> AppResult<f32> {
        // Silero v6.2 的 ONNX 输入 = 上一帧末尾的 context ++ 本帧新样本。
        // 少了 context，模型会退化成恒输出 ~0 的概率（有人说话也测不到）。
        let mut input = Vec::with_capacity(self.context_len + self.frame_len);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(chunk);
        let input_len = input.len();

        let input_tensor = Tensor::from_array(([1usize, input_len], input.clone()))
            .map_err(|e| AppError::Audio(format!("构造输入张量失败: {e}")))?;
        let state = Tensor::from_array(([2usize, 1usize, STATE_DIM], self.state.clone()))
            .map_err(|e| AppError::Audio(format!("构造状态张量失败: {e}")))?;
        let sample_rate =
            Tensor::from_array((Vec::<i64>::new(), vec![self.sample_rate]))
                .map_err(|e| AppError::Audio(format!("构造采样率张量失败: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![input_tensor, state, sample_rate])
            .map_err(|e| AppError::Audio(format!("Silero 推理失败: {e}")))?;

        let probability = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Audio(format!("解析 Silero 输出失败: {e}")))?
            .1
            .first()
            .copied()
            .unwrap_or(0.0);

        // 把新的 LSTM 状态存回去，供下一帧使用
        if let Ok((_, next_state)) = outputs[1].try_extract_tensor::<f32>() {
            if next_state.len() == self.state.len() {
                self.state.copy_from_slice(next_state);
            }
        }

        // context = 本次模型输入的最后 context_len 个样本
        self.context
            .copy_from_slice(&input[input_len - self.context_len..]);

        Ok(probability)
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.0);
        self.context.iter_mut().for_each(|v| *v = 0.0);
        self.buffer.clear();
        self.prob_window.clear();
        self.active = false;
    }
}

/// 随包附带 ONNX Runtime 时的候选位置
fn bundled_library_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(ORT_LIB_NAME));
            candidates.push(dir.join("lib").join(ORT_LIB_NAME));
            // macOS .app: Contents/MacOS/ -> Contents/Frameworks/
            candidates.push(dir.join("..").join("Frameworks").join(ORT_LIB_NAME));
        }
    }

    candidates
}

/// 若用户未显式指定，且程序目录里随包附带了 ONNX Runtime，则自动使用它
///
/// 必须在调用任何 `ort` API 之前执行：`ort` 只在首次使用时读该环境变量。
///
/// 注意：本函数会写入进程级环境变量 `ORT_DYLIB_PATH`。
/// 建议在创建任何线程/异步运行时**之前**调用一次
/// （见 `detector::prepare_ort_library`），以规避多线程下并发读写环境变量的风险。
pub fn ensure_ort_library_env() {
    if std::env::var("ORT_DYLIB_PATH")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return;
    }

    for candidate in bundled_library_candidates() {
        if candidate.is_file() {
            if let Ok(absolute) = candidate.canonicalize() {
                tracing::info!("使用随附的 ONNX Runtime: {}", absolute.display());
                std::env::set_var("ORT_DYLIB_PATH", &absolute);
            }
            return;
        }
    }
}

/// 待加载的 ONNX Runtime 路径（环境变量优先，否则用平台默认文件名交给系统搜索）
fn ort_library_path() -> String {
    std::env::var("ORT_DYLIB_PATH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ORT_LIB_NAME.to_string())
}

fn cstring_to_string(ptr: *const std::ffi::c_char) -> String {
    if ptr.is_null() {
        return "unknown".to_string();
    }
    // SAFETY: ONNX Runtime 保证该指针指向以 NUL 结尾的静态字符串
    unsafe { std::ffi::CStr::from_ptr(ptr).to_string_lossy().to_string() }
}

/// 功能性探测 ONNX Runtime：可加载 + API 版本兼容
///
/// 成功返回库自身版本号。必须在调用任何 `ort` API 之前执行。
fn probe_onnxruntime() -> Result<String, String> {
    probe_onnxruntime_at(&ort_library_path())
}

/// 与 [`probe_onnxruntime`] 相同，但显式指定库路径（便于测试，避免改动全局环境变量）
fn probe_onnxruntime_at(path: &str) -> Result<String, String> {
    use ort::sys::{OrtApiBase, ORT_API_VERSION};

    // SAFETY: 只读取符号与查询版本，不做其他调用；
    // 库句柄在函数结束前一直有效。
    unsafe {
        let library = libloading::Library::new(path)
            .map_err(|e| format!("无法加载 `{path}`：{e}"))?;

        let get_api_base = library
            .get::<unsafe extern "system" fn() -> *const OrtApiBase>(b"OrtGetApiBase\0")
            .map_err(|e| format!("`{path}` 不是有效的 ONNX Runtime（缺少 OrtGetApiBase）：{e}"))?;

        let base = get_api_base();
        if base.is_null() {
            return Err(format!("`{path}` 的 OrtGetApiBase 返回空指针"));
        }

        // 关键：只有库支持该 API 版本时 GetApi 才返回非空
        let api = ((*base).GetApi)(ORT_API_VERSION);
        let version = cstring_to_string(((*base).GetVersionString)());

        if api.is_null() {
            return Err(format!(
                "`{path}` 版本过低（库版本 {version}，需要 API 版本 {ORT_API_VERSION}）"
            ));
        }

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VadConfig;

    /// Silero 测试需要真实的 ONNX Runtime，默认跳过（opt-in）
    ///
    /// 用 `ORT_DYLIB_PATH` 显式指定库后才会执行，例如：
    ///   ORT_DYLIB_PATH=/path/to/libonnxruntime.dylib cargo test --bin livetranslate
    ///
    /// 这样做的原因：若让测试去探测系统里的库，CI 上可能加载到版本不兼容的
    /// onnxruntime，导致 ort 初始化 panic 并把内部锁毒化，
    /// 最终在进程退出时 abort（表现为测试全通过但整体失败）。
    fn test_vad() -> Option<SileroVad> {
        let configured = std::env::var("ORT_DYLIB_PATH")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        if !configured {
            eprintln!("跳过 Silero 测试：未设置 ORT_DYLIB_PATH（需要 ONNX Runtime 才可运行）");
            return None;
        }

        match SileroVad::new(&VadConfig::default(), 16_000) {
            Ok(vad) => Some(vad),
            Err(e) => {
                eprintln!("跳过 Silero 测试（ONNX Runtime 不可用）: {e}");
                None
            }
        }
    }

    /// 与 Python 参考实现完全一致的确定性输入（可被 f32 精确表示）
    fn frame(i: usize) -> Vec<f32> {
        (0..512)
            .map(|j| {
                let v = ((j * 37 + i * 11) % 101) as i64 - 50;
                v as f32 / 128.0
            })
            .collect()
    }

    #[test]
    fn matches_onnxruntime_reference() {
        let Some(mut vad) = test_vad() else {
            return;
        };

        // 由 ONNX Runtime 1.30 跑同一模型 + 同一输入（含 context）得到。
        // 注意：这些值只有在正确拼接 64 样本 context 时才成立；
        // 旧实现喂 512 无 context，概率会退化成 ~0.001，这里会直接失败。
        let expected = [
            0.0176422f32,
            0.0080044,
            0.0057910,
            0.0046635,
            0.0036013,
        ];

        for (i, want) in expected.iter().enumerate() {
            let got = vad.infer(&frame(i)).expect("推理失败");
            assert!(
                (got - want).abs() < 1e-5,
                "第 {i} 帧概率与参考不一致: got={got}, want={want}"
            );
        }

        // LSTM 状态也应一致，说明状态确实被正确传递
        let sum: f32 = vad.state.iter().sum();
        assert!(
            (sum - 1.1207).abs() < 1e-2,
            "LSTM 状态和与参考不一致: got={sum}, want≈1.1207"
        );
    }

    /// 回归：context 必须逐帧滚动更新，否则模型输入退化为 512 → 概率恒 ~0
    #[test]
    fn context_is_carried_between_frames() {
        let Some(mut vad) = test_vad() else {
            return;
        };

        assert_eq!(vad.context_len, 64, "16kHz 的 context 应为 64 样本");

        // 第一帧：context 仍为全零，推理后应变成 frame(0) 的末 64 个样本
        let first = frame(0);
        vad.infer(&first).expect("推理失败");
        assert_eq!(&vad.context[..], &first[first.len() - 64..]);

        // 第二帧：推理后 context 应为 frame(1) 的末 64 个样本
        let second = frame(1);
        vad.infer(&second).expect("推理失败");
        assert_eq!(&vad.context[..], &second[second.len() - 64..]);

        // reset 必须清空 context
        vad.reset();
        assert!(vad.context.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn threshold_controls_decision() {
        let Some(mut vad) = test_vad() else {
            return;
        };

        // 该输入参考概率约 0.0176，阈值高于它应判为非语音
        vad.reset();
        vad.threshold = 0.5;
        assert!(!vad.is_speech(&frame(0)));

        vad.reset();
        vad.threshold = 0.001;
        assert!(vad.is_speech(&frame(0)));
    }

    #[test]
    fn probe_reports_clear_reason_when_library_missing() {
        // 探测一个不存在的路径，必须返回可读的错误而不是 panic
        let err = probe_onnxruntime_at("/nonexistent/definitely/not/libonnxruntime.dylib")
            .expect_err("不存在的库应探测失败");
        assert!(err.contains("无法加载"), "错误信息应说明无法加载: {err}");
    }

    #[test]
    fn probe_fails_for_a_non_library_file() {
        // 现成的非库文件（本文件）应被识别为“不是有效的 ONNX Runtime”
        let err = probe_onnxruntime_at(file!()).expect_err("普通文件不应探测成功");
        assert!(
            err.contains("无法加载") || err.contains("OrtGetApiBase"),
            "错误信息应说明原因: {err}"
        );
    }

    #[test]
    fn hysteresis_uses_lower_threshold_to_exit() {
        // 非语音态：必须达到进入阈值
        assert!(!hysteresis_step(false, 0.49, 0.5, 0.35));
        assert!(hysteresis_step(false, 0.5, 0.5, 0.35));
        // 已处于语音态：掉到 0.35~0.5 仍算继续说话（迟滞）
        assert!(hysteresis_step(true, 0.4, 0.5, 0.35));
        assert!(hysteresis_step(true, 0.35, 0.5, 0.35));
        // 只有低于退出门槛才结束
        assert!(!hysteresis_step(true, 0.34, 0.5, 0.35));
    }

    #[test]
    fn frame_len_matches_sample_rate() {
        assert_eq!(SileroVad::frame_len_for(16_000).unwrap(), 512);
        assert_eq!(SileroVad::frame_len_for(8_000).unwrap(), 256);
        assert!(SileroVad::frame_len_for(44_100).is_err());
    }

    #[test]
    fn context_len_matches_sample_rate() {
        assert_eq!(SileroVad::context_len_for(16_000).unwrap(), 64);
        assert_eq!(SileroVad::context_len_for(8_000).unwrap(), 32);
        assert!(SileroVad::context_len_for(44_100).is_err());
        // 8kHz 的实际模型输入应为 32 + 256 = 288
        assert_eq!(
            SileroVad::context_len_for(8_000).unwrap() + SileroVad::frame_len_for(8_000).unwrap(),
            288
        );
    }

    #[test]
    fn bundled_candidates_cover_expected_locations() {
        let candidates = bundled_library_candidates();
        assert!(!candidates.is_empty());
        // 全部应以平台对应的库名结尾
        assert!(candidates.iter().all(|p| p.ends_with(ORT_LIB_NAME)));
        // 至少包含“可执行文件同级”这一项
        let exe_dir = std::env::current_exe().unwrap();
        let exe_dir = exe_dir.parent().unwrap();
        assert!(candidates.contains(&exe_dir.join(ORT_LIB_NAME)));
        // 以及 macOS .app 的 Frameworks 目录
        assert!(candidates
            .iter()
            .any(|p| p.to_string_lossy().contains("Frameworks")));
    }
}
