//! 基于 Silero VAD 的语音活动检测（ONNX Runtime）
//!
//! 相比内置的能量 VAD，Silero 是训练出来的语音分类模型，
//! 能区分「音乐 / 噪声」与「人声」，也能在低信噪比下工作。
//!
//! 模型为 Silero VAD v5（MIT 许可），已内嵌进二进制；
//! 也可通过 `[vad] silero_model` 指定外部模型文件。
//!
//! 运行期需要 ONNX Runtime 动态库（`onnxruntime`）：
//! 可用 `ORT_DYLIB_PATH` 指定路径，或放到系统库搜索路径里。
//! 加载失败时上层会自动回退到能量 VAD。

use ort::session::Session;
use ort::value::Tensor;

use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// 内嵌的 Silero VAD v5 模型（models/silero_vad.onnx）
const EMBEDDED_MODEL: &[u8] = include_bytes!("../../models/silero_vad.onnx");

/// LSTM 隐状态维度（模型固定值）
const STATE_DIM: usize = 128;

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
    /// 未凑满一帧的剩余样本
    buffer: Vec<f32>,
    frame_len: usize,
    sample_rate: i64,
    threshold: f32,
}

impl SileroVad {
    /// 模型在 16kHz 下要求每次 512 个样本，8kHz 下 256 个
    pub fn frame_len_for(sample_rate: u32) -> AppResult<usize> {
        match sample_rate {
            16_000 => Ok(512),
            8_000 => Ok(256),
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

        let model_path = config
            .silero_model
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty());

        let session = match model_path {
            Some(path) => Session::builder()
                .and_then(|mut builder| builder.commit_from_file(path))
                .map_err(|e| AppError::Audio(format!("加载 Silero 模型 `{path}` 失败: {e}")))?,
            None => Session::builder()
                .and_then(|mut builder| builder.commit_from_memory(EMBEDDED_MODEL))
                .map_err(|e| AppError::Audio(format!("加载内置 Silero 模型失败: {e}")))?,
        };

        tracing::info!(
            "Silero VAD 已加载 (frame={} samples @ {}Hz, threshold={})",
            frame_len,
            sample_rate,
            config.silero_threshold
        );

        Ok(Self {
            session,
            state: vec![0.0; 2 * STATE_DIM],
            buffer: Vec::with_capacity(frame_len * 2),
            frame_len,
            sample_rate: sample_rate as i64,
            threshold: config.silero_threshold,
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

        let mut speech = false;
        while self.buffer.len() >= self.frame_len {
            let chunk: Vec<f32> = self.buffer.drain(..self.frame_len).collect();

            match self.infer(&chunk) {
                Ok(probability) => speech = probability >= self.threshold,
                Err(e) => {
                    tracing::warn!("Silero 推理失败: {e}");
                    speech = false;
                }
            }
        }
        speech
    }

    fn infer(&mut self, chunk: &[f32]) -> AppResult<f32> {
        let input = Tensor::from_array(([1usize, self.frame_len], chunk.to_vec()))
            .map_err(|e| AppError::Audio(format!("构造输入张量失败: {e}")))?;
        let state = Tensor::from_array(([2usize, 1usize, STATE_DIM], self.state.clone()))
            .map_err(|e| AppError::Audio(format!("构造状态张量失败: {e}")))?;
        let sample_rate =
            Tensor::from_array((Vec::<i64>::new(), vec![self.sample_rate]))
                .map_err(|e| AppError::Audio(format!("构造采样率张量失败: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![input, state, sample_rate])
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

        Ok(probability)
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.0);
        self.buffer.clear();
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

        // 由 ONNX Runtime 1.30 跑同一模型 + 同一输入得到
        let expected = [
            0.0013514161f32,
            0.0009435713,
            0.0009604394,
            0.0009388328,
            0.0009409189,
        ];

        for (i, want) in expected.iter().enumerate() {
            let got = vad.infer(&frame(i)).expect("推理失败");
            assert!(
                (got - want).abs() < 1e-6,
                "第 {i} 帧概率与参考不一致: got={got}, want={want}"
            );
        }

        // LSTM 状态也应一致，说明状态确实被正确传递
        let sum: f32 = vad.state.iter().sum();
        assert!(
            (sum - 21.05366898).abs() < 1e-2,
            "LSTM 状态和与参考不一致: got={sum}, want≈21.05366898"
        );
    }

    #[test]
    fn threshold_controls_decision() {
        let Some(mut vad) = test_vad() else {
            return;
        };

        // 该输入参考概率约 0.0014，阈值高于它应判为非语音
        vad.reset();
        vad.threshold = 0.5;
        assert!(!vad.is_speech(&frame(0)));

        vad.reset();
        vad.threshold = 0.0005;
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
    fn frame_len_matches_sample_rate() {
        assert_eq!(SileroVad::frame_len_for(16_000).unwrap(), 512);
        assert_eq!(SileroVad::frame_len_for(8_000).unwrap(), 256);
        assert!(SileroVad::frame_len_for(44_100).is_err());
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
