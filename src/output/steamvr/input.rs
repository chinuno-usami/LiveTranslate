//! SteamVR Input 2.0：3A 游戏输入屏蔽（仅 Windows + `steamvr`）。
//!
//! - 解压内嵌 resources → `SetActionManifestPath` + `AddApplicationManifest`
//! - 移动模式提交 `nPriority = 0x01000000` 的 `/actions/move` action set
//! - 退出/取消/断开路径必须调用 [`InputBlocker::clear_priority`] 归零
//! - 只读检测 `steamvr/globalActionSetPriority`，绝不写入 SteamVR 全局设置
//!
//! 失败一律返回可读 `String`，不 panic。由 `imp` 在 `block_game_input` 拖动时懒初始化。

use super::assets::{ensure_extracted, SteamVrAssetPaths};
use openvr::errors::{VRApplicationError, VRInputError};
use openvr::input::{VRActionHandle, VRActionSetHandle, VRActiveActionSet};
use openvr::{Context, Input, Settings};
use openvr_sys::VRActiveActionSet_t;
use std::ffi::CString;
use std::path::Path;

/// Valve `k_nActionSetOverlayGlobalPriorityMin`：overlay-global 区间下限。
pub const OVERLAY_GLOBAL_PRIORITY_MIN: i32 = openvr_sys::k_nActionSetOverlayGlobalPriorityMin;

/// 与 actions.json 一致：set=`/actions/move`，actions 在 `/actions/move/in/...`。
pub const ACTION_SET_PATH: &str = "/actions/move";
pub const ACTION_GRIP_LEFT: &str = "/actions/move/in/grip_left";
pub const ACTION_GRIP_RIGHT: &str = "/actions/move/in/grip_right";
pub const ACTION_TRIGGER_LEFT: &str = "/actions/move/in/trigger_left";
pub const ACTION_TRIGGER_RIGHT: &str = "/actions/move/in/trigger_right";

pub const APP_KEY: &str = "com.chinuno.livetranslate";

const SETTINGS_SECTION: &str = "steamvr";
const SETTINGS_KEY_GLOBAL_PRIORITY: &str = "globalActionSetPriority";

/// 实验设置 / manifest / 注册 / binding 的可读状态（只提示，不写全局设置）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputBlockDiagnostics {
    /// `Some(true/false)` 表示成功读到实验开关；`None` 表示读取失败。
    pub experimental_enabled: Option<bool>,
    pub experimental_message: String,
    pub manifest_ok: bool,
    pub manifest_message: String,
    pub app_registered: bool,
    pub app_register_message: String,
    pub bindings_ok: bool,
    pub bindings_message: String,
}

impl InputBlockDiagnostics {
    /// 汇总成一行给人看的状态（设置页 / `input_block_message`）。
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.experimental_message.is_empty() {
            parts.push(self.experimental_message.as_str());
        }
        if !self.manifest_message.is_empty() {
            parts.push(self.manifest_message.as_str());
        }
        if !self.app_register_message.is_empty() {
            parts.push(self.app_register_message.as_str());
        }
        if !self.bindings_message.is_empty() {
            parts.push(self.bindings_message.as_str());
        }
        parts.join("；")
    }

    /// 是否具备尝试高优先级屏蔽的前置条件（manifest+注册；实验开关另计）。
    #[allow(dead_code)] // t6 状态页会用
    pub fn ready_for_blocking(&self) -> bool {
        self.manifest_ok && self.app_registered
    }
}

/// 已初始化的 Input 2.0 屏蔽句柄集合。
pub struct InputBlocker {
    paths: SteamVrAssetPaths,
    action_set: VRActionSetHandle,
    grip_left: VRActionHandle,
    grip_right: VRActionHandle,
    trigger_left: VRActionHandle,
    trigger_right: VRActionHandle,
    /// 当前是否以 overlay-global priority 提交着 action set。
    priority_active: bool,
    diagnostics: InputBlockDiagnostics,
}

impl InputBlocker {
    /// 解压资源 → 注册 manifest → 取 handles → 只读实验设置与 binding 探测。
    ///
    /// 任一步失败返回 `Err(可读原因)`，调用方应降级 3C（`input_blocked=false`）。
    pub fn try_init(ctx: &Context, active_config_path: Option<&Path>) -> Result<Self, String> {
        let paths = ensure_extracted(active_config_path)
            .map_err(|e| format!("SteamVR 资源解压失败: {e}"))?;

        let mut input = ctx
            .input()
            .map_err(|e| format!("获取 IVRInput 失败: {e:?}"))?;
        let mut application = ctx
            .application()
            .map_err(|e| format!("获取 IVRApplications 失败: {e:?}"))?;
        let mut settings = ctx
            .settings()
            .map_err(|e| format!("获取 IVRSettings 失败: {e:?}"))?;

        let mut diagnostics = InputBlockDiagnostics::default();

        // 1) 应用清单（temporary：不永久污染 Steam 库；已存在视为成功）
        match application.add_application_manifest(&paths.vrmanifest, true) {
            Ok(()) => {
                diagnostics.app_registered = true;
                diagnostics.app_register_message =
                    format!("已注册应用清单 {}", paths.vrmanifest.display());
            }
            Err(VRApplicationError::AppKeyAlreadyExists) => {
                diagnostics.app_registered = true;
                diagnostics.app_register_message = format!("应用清单已存在（{APP_KEY}）");
            }
            Err(err) => {
                // 再查是否其实已安装
                match application.is_application_installed(APP_KEY) {
                    Ok(true) => {
                        diagnostics.app_registered = true;
                        diagnostics.app_register_message =
                            format!("AddApplicationManifest 返回 {err:?}，但 {APP_KEY} 已安装");
                    }
                    _ => {
                        return Err(format!(
                            "AddApplicationManifest 失败: {err:?}（{})",
                            paths.vrmanifest.display()
                        ));
                    }
                }
            }
        }

        // 2) action manifest（绝对路径）
        input
            .set_action_manifest(&paths.actions_manifest)
            .map_err(|e| {
                format!(
                    "SetActionManifestPath 失败: {e:?}（{})",
                    paths.actions_manifest.display()
                )
            })?;
        diagnostics.manifest_ok = true;
        diagnostics.manifest_message = format!(
            "已加载 action manifest {}",
            paths.actions_manifest.display()
        );

        // 3) handles（仅 grip/trigger）
        let action_set = input
            .get_action_set_handle(ACTION_SET_PATH)
            .map_err(|e| format!("GetActionSetHandle({ACTION_SET_PATH}) 失败: {e:?}"))?;
        let grip_left = get_action(&mut input, ACTION_GRIP_LEFT)?;
        let grip_right = get_action(&mut input, ACTION_GRIP_RIGHT)?;
        let trigger_left = get_action(&mut input, ACTION_TRIGGER_LEFT)?;
        let trigger_right = get_action(&mut input, ACTION_TRIGGER_RIGHT)?;

        // 4) 只读实验设置（绝不 set_bool / 写入）
        refresh_experimental_setting(&mut settings, &mut diagnostics);

        // 5) binding 探测（无手柄时可能为空，记为提示而非硬失败）
        probe_bindings(
            &mut input,
            &[grip_left, grip_right, trigger_left, trigger_right],
            &mut diagnostics,
        );

        Ok(Self {
            paths,
            action_set,
            grip_left,
            grip_right,
            trigger_left,
            trigger_right,
            priority_active: false,
            diagnostics,
        })
    }

    #[allow(dead_code)] // 调试 / t6 状态扩展
    pub fn paths(&self) -> &SteamVrAssetPaths {
        &self.paths
    }

    pub fn diagnostics(&self) -> &InputBlockDiagnostics {
        &self.diagnostics
    }

    pub fn priority_active(&self) -> bool {
        self.priority_active
    }

    #[allow(dead_code)] // binding 探测已内化；保留查询入口
    pub fn action_handles(&self) -> [VRActionHandle; 4] {
        [
            self.grip_left,
            self.grip_right,
            self.trigger_left,
            self.trigger_right,
        ]
    }

    /// 刷新只读实验设置（例如用户中途在 SteamVR 里打开开关）。
    pub fn refresh_settings(&mut self, ctx: &Context) -> Result<(), String> {
        let mut settings = ctx
            .settings()
            .map_err(|e| format!("获取 IVRSettings 失败: {e:?}"))?;
        refresh_experimental_setting(&mut settings, &mut self.diagnostics);
        Ok(())
    }

    /// 普通优先级提交（nPriority=0）：不压制 scene app。
    #[allow(dead_code)] // 对称 API；imp 退出路径走 clear_priority
    pub fn update_normal(&mut self, input: &mut Input) -> Result<(), String> {
        self.submit(input, 0)
    }

    /// 移动模式：overlay-global 高优先级，压制已绑定的 grip/trigger。
    pub fn update_blocking(&mut self, input: &mut Input) -> Result<(), String> {
        if self.diagnostics.experimental_enabled == Some(false) {
            return Err(
                "SteamVR 实验设置未开启：Settings → Developer → Enable global input from overlays (experimental)".to_string(),
            );
        }
        self.submit(input, OVERLAY_GLOBAL_PRIORITY_MIN)
    }

    /// 显式归零：退出移动 / cancel / 断开 / Dashboard / 异常 / 线程退出必须调用。
    pub fn clear_priority(&mut self, input: &mut Input) -> Result<(), String> {
        // 即使当前未处于 blocking，也提交一次 priority=0，避免残留。
        self.submit(input, 0)
    }

    fn submit(&mut self, input: &mut Input, priority: i32) -> Result<(), String> {
        let mut sets = [make_active_set(self.action_set, priority)];
        input.update_actions(&mut sets).map_err(|e: VRInputError| {
            format!("UpdateActionState(nPriority={priority}) 失败: {e:?}")
        })?;
        self.priority_active = priority != 0;
        Ok(())
    }
}

fn get_action(input: &mut Input, name: &str) -> Result<VRActionHandle, String> {
    input
        .get_action_handle(name)
        .map_err(|e| format!("GetActionHandle({name}) 失败: {e:?}"))
}

/// 构造带 `nPriority` 的 `VRActiveActionSet`（经 openvr_sys 字段写入）。
pub fn make_active_set(action_set: VRActionSetHandle, priority: i32) -> VRActiveActionSet {
    VRActiveActionSet(VRActiveActionSet_t {
        ulActionSet: action_set.0,
        ulRestrictedToDevice: openvr_sys::k_ulInvalidInputValueHandle,
        ulSecondaryActionSet: 0,
        unPadding: 0,
        nPriority: priority,
    })
}

fn refresh_experimental_setting(settings: &mut Settings, diag: &mut InputBlockDiagnostics) {
    let Ok(section) = CString::new(SETTINGS_SECTION) else {
        diag.experimental_enabled = None;
        diag.experimental_message = "设置 section 无效".to_string();
        return;
    };
    let Ok(key) = CString::new(SETTINGS_KEY_GLOBAL_PRIORITY) else {
        diag.experimental_enabled = None;
        diag.experimental_message = "设置 key 无效".to_string();
        return;
    };
    match settings.get_bool(&section, &key) {
        Ok(enabled) => {
            diag.experimental_enabled = Some(enabled);
            diag.experimental_message = if enabled {
                "实验设置 globalActionSetPriority 已开启".to_string()
            } else {
                "未开启实验设置：SteamVR → 设置 → 开发者 → Enable global input from overlays (experimental)".to_string()
            };
        }
        Err(err) => {
            diag.experimental_enabled = None;
            diag.experimental_message = format!(
                "无法读取 steamvr/globalActionSetPriority（{err:?}）；请在 SteamVR 开发者设置中确认"
            );
        }
    }
}

fn probe_bindings(
    input: &mut Input,
    actions: &[VRActionHandle; 4],
    diag: &mut InputBlockDiagnostics,
) {
    let mut bound = 0usize;
    let mut errors = Vec::new();
    for handle in actions {
        match input.get_action_binding_info(*handle) {
            Ok(info) if !info.is_empty() => bound += 1,
            Ok(_) => {}
            Err(err) => errors.push(format!("{err:?}")),
        }
    }
    if bound > 0 {
        diag.bindings_ok = true;
        diag.bindings_message = format!("已探测到 {bound}/4 个 grip/trigger binding");
    } else if errors.is_empty() {
        // 无手柄或尚未加载默认 binding：不阻断初始化，实机再验
        diag.bindings_ok = false;
        diag.bindings_message =
            "尚未探测到 grip/trigger binding（可能无手柄或默认 binding 未加载）".to_string();
    } else {
        diag.bindings_ok = false;
        diag.bindings_message = format!(
            "binding 探测失败: {}",
            errors.into_iter().take(2).collect::<Vec<_>>().join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openvr::input::VRActionSetHandle;

    #[test]
    fn overlay_priority_matches_valve_constant() {
        assert_eq!(OVERLAY_GLOBAL_PRIORITY_MIN, 0x0100_0000);
        assert_eq!(
            OVERLAY_GLOBAL_PRIORITY_MIN,
            openvr_sys::k_nActionSetOverlayGlobalPriorityMin
        );
    }

    #[test]
    fn make_active_set_writes_n_priority() {
        let set = VRActionSetHandle(42);
        let blocking = make_active_set(set, OVERLAY_GLOBAL_PRIORITY_MIN);
        assert_eq!(blocking.0.ulActionSet, 42);
        assert_eq!(blocking.0.nPriority, OVERLAY_GLOBAL_PRIORITY_MIN);
        assert_eq!(
            blocking.0.ulRestrictedToDevice,
            openvr_sys::k_ulInvalidInputValueHandle
        );

        let cleared = make_active_set(set, 0);
        assert_eq!(cleared.0.nPriority, 0);
    }

    #[test]
    fn action_paths_are_grip_trigger_only() {
        let actions = [
            ACTION_GRIP_LEFT,
            ACTION_GRIP_RIGHT,
            ACTION_TRIGGER_LEFT,
            ACTION_TRIGGER_RIGHT,
        ];
        for path in actions {
            assert!(
                path.contains("grip") || path.contains("trigger"),
                "{path} 不是 grip/trigger"
            );
            assert!(!path.contains("joystick") && !path.contains("menu"));
        }
        assert_eq!(ACTION_SET_PATH, "/actions/move");
    }

    #[test]
    fn diagnostics_summary_and_ready() {
        let mut d = InputBlockDiagnostics::default();
        assert!(!d.ready_for_blocking());
        assert!(d.summary().is_empty());

        d.manifest_ok = true;
        d.manifest_message = "manifest ok".into();
        d.app_registered = true;
        d.app_register_message = "registered".into();
        d.experimental_message = "exp off".into();
        assert!(d.ready_for_blocking());
        let s = d.summary();
        assert!(s.contains("exp off"));
        assert!(s.contains("manifest ok"));
        assert!(s.contains("registered"));
    }
}
