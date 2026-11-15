//! SteamVR 手势定位纯函数（3C）：矩阵、射线、增量换算与状态机。
//!
//! 不依赖 `openvr` 类型，便于非 Windows 单测。权威：docs/V0.3_FEATURE_PLAN.md §4.2–§4.8。

use std::time::Duration;

/// 双手 grip 进入拖动前的按住时长。
pub const ARMING_HOLD: Duration = Duration::from_millis(250);

/// VR 内解锁组合（双手 trigger 同时按住）的时长。
pub const UNLOCK_HOLD: Duration = Duration::from_millis(1500);

/// VR 内解锁触发后的冷却，避免一次长按被判定成多次。
pub const UNLOCK_COOLDOWN: Duration = Duration::from_millis(1000);

/// 单帧世界位移上限（米），防止追踪丢失/近平行射线瞬移。
pub const MAX_FRAME_DELTA_M: f32 = 0.25;

/// `|dot(dir, normal)|` 低于此值视为近平行，跳过本帧移动。
pub const MIN_RAY_PLANE_DOT: f32 = 0.1;

/// 面板命中矩形边距（相对半宽/半高），命中可略超出物理边缘。
pub const PANEL_HIT_MARGIN_FRAC: f32 = 0.08;

/// 写入/校验区间（与 `AppConfig::validate` 一致）。
pub const DISTANCE_M_RANGE: (f32, f32) = (0.2, 10.0);
pub const OFFSET_M_RANGE: (f32, f32) = (-2.0, 2.0);
pub const WORLD_POSITION_RANGE: (f32, f32) = (-100.0, 100.0);

/// OpenVR 风格 3×4 行主序矩阵（前三列旋转，第 4 列平移）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3x4(pub [[f32; 4]; 3]);

impl Mat3x4 {
    pub const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);

    pub fn from_rows(rows: [[f32; 4]; 3]) -> Self {
        Self(rows)
    }

    /// 平移分量（第 4 列）。
    pub fn translation(&self) -> [f32; 3] {
        [self.0[0][3], self.0[1][3], self.0[2][3]]
    }

    /// `p' = R * p + t`
    pub fn transform_point(&self, p: [f32; 3]) -> [f32; 3] {
        [
            self.0[0][0] * p[0] + self.0[0][1] * p[1] + self.0[0][2] * p[2] + self.0[0][3],
            self.0[1][0] * p[0] + self.0[1][1] * p[1] + self.0[1][2] * p[2] + self.0[1][3],
            self.0[2][0] * p[0] + self.0[2][1] * p[1] + self.0[2][2] * p[2] + self.0[2][3],
        ]
    }

    /// `R^T * v`（只取旋转部分的转置乘）。
    pub fn rotation_transpose_mul(&self, v: [f32; 3]) -> [f32; 3] {
        [
            self.0[0][0] * v[0] + self.0[1][0] * v[1] + self.0[2][0] * v[2],
            self.0[0][1] * v[0] + self.0[1][1] * v[1] + self.0[2][1] * v[2],
            self.0[0][2] * v[0] + self.0[1][2] * v[1] + self.0[2][2] * v[2],
        ]
    }

    /// 矩阵乘 `H * L`（行主序 3×4）。
    pub fn multiply(&self, rhs: &Mat3x4) -> Mat3x4 {
        let mut out = [[0.0; 4]; 3];
        for (r, out_row) in out.iter_mut().enumerate() {
            for (c, cell) in out_row.iter_mut().take(3).enumerate() {
                *cell = self.0[r][0] * rhs.0[0][c]
                    + self.0[r][1] * rhs.0[1][c]
                    + self.0[r][2] * rhs.0[2][c];
            }
            out_row[3] = self.0[r][0] * rhs.0[0][3]
                + self.0[r][1] * rhs.0[1][3]
                + self.0[r][2] * rhs.0[2][3]
                + self.0[r][3];
        }
        Mat3x4(out)
    }

    /// 面板平面法线：局部 +Z 变换到世界（未归一化亦可供点积）。
    pub fn panel_normal(&self) -> [f32; 3] {
        [self.0[0][2], self.0[1][2], self.0[2][2]]
    }

    /// 面板局部 +X（右）在世界中的方向。
    pub fn panel_right(&self) -> [f32; 3] {
        [self.0[0][0], self.0[1][0], self.0[2][0]]
    }

    /// 面板局部 +Y（上）在世界中的方向。
    pub fn panel_up(&self) -> [f32; 3] {
        [self.0[0][1], self.0[1][1], self.0[2][1]]
    }
}

/// 控制器前向：局部 `-Z`。
pub fn controller_forward(m: &Mat3x4) -> [f32; 3] {
    [-m.0[0][2], -m.0[1][2], -m.0[2][2]]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale3(v: [f32; 3], s: f32) -> [f32; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

fn length3(v: [f32; 3]) -> f32 {
    dot3(v, v).sqrt()
}

fn clamp_f32(v: f32, min: f32, max: f32) -> f32 {
    v.max(min).min(max)
}

/// 射线-平面求交。平行或命中在射线后方 → `None`。
pub fn ray_plane_intersection(
    ray_origin: [f32; 3],
    ray_dir: [f32; 3],
    plane_point: [f32; 3],
    plane_normal: [f32; 3],
) -> Option<[f32; 3]> {
    let denom = dot3(ray_dir, plane_normal);
    if denom.abs() < 1e-8 {
        return None;
    }
    let t = dot3(sub3(plane_point, ray_origin), plane_normal) / denom;
    if t < 0.0 {
        return None;
    }
    Some(add3(ray_origin, scale3(ray_dir, t)))
}

/// 命中点是否落在面板矩形内（含相对边距）。
pub fn hit_in_panel_rect(
    hit: [f32; 3],
    panel_center: [f32; 3],
    panel_right: [f32; 3],
    panel_up: [f32; 3],
    half_width_m: f32,
    half_height_m: f32,
    margin_frac: f32,
) -> bool {
    let local = sub3(hit, panel_center);
    let x = dot3(local, panel_right);
    let y = dot3(local, panel_up);
    let mx = half_width_m * (1.0 + margin_frac);
    let my = half_height_m * (1.0 + margin_frac);
    x.abs() <= mx && y.abs() <= my
}

/// 近平行射线：本帧应跳过移动。
pub fn ray_nearly_parallel(ray_dir: [f32; 3], plane_normal: [f32; 3]) -> bool {
    dot3(ray_dir, plane_normal).abs() < MIN_RAY_PLANE_DOT
}

/// 将世界位移限制在单帧上限内。
pub fn limit_frame_delta(delta: [f32; 3], max_m: f32) -> [f32; 3] {
    let len = length3(delta);
    if len <= max_m || len < 1e-12 {
        delta
    } else {
        scale3(delta, max_m / len)
    }
}

/// 由本帧命中点计算世界位移增量；无命中或首帧只更新 `prev_hit`。
///
/// - `hit == None`：保持拖动但不移动，且不清除 `prev_hit`（下次有效命中可续）。
/// - 返回的位移已乘 `sensitivity` 并做过单帧限幅。
pub fn drag_world_delta(
    hit: Option<[f32; 3]>,
    prev_hit: &mut Option<[f32; 3]>,
    sensitivity: f32,
    max_frame_m: f32,
) -> Option<[f32; 3]> {
    let hit = hit?;
    let Some(prev) = *prev_hit else {
        *prev_hit = Some(hit);
        return None;
    };
    let raw = scale3(sub3(hit, prev), sensitivity);
    *prev_hit = Some(hit);
    Some(limit_frame_delta(raw, max_frame_m))
}

/// HMD 锚点下的可拖动偏移。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HmdOffsets {
    pub horizontal_offset_m: f32,
    pub height_offset_m: f32,
    pub distance_m: f32,
}

impl HmdOffsets {
    pub fn from_config_fields(horizontal: f32, height: f32, distance: f32) -> Self {
        Self {
            horizontal_offset_m: horizontal,
            height_offset_m: height,
            distance_m: distance,
        }
    }
}

/// 将世界增量转到当前 HMD 旋转下的局部增量并累加到起始偏移。
///
/// `distance_m = start.distance - Δz`（前向为 -Z）。
pub fn apply_hmd_delta(start: HmdOffsets, hmd_pose: &Mat3x4, delta_world: [f32; 3]) -> HmdOffsets {
    let local = hmd_pose.rotation_transpose_mul(delta_world);
    HmdOffsets {
        horizontal_offset_m: start.horizontal_offset_m + local[0],
        height_offset_m: start.height_offset_m + local[1],
        distance_m: start.distance_m - local[2],
    }
}

/// World 锚点：只改位置。
pub fn apply_world_delta(start: [f32; 3], delta_world: [f32; 3]) -> [f32; 3] {
    add3(start, delta_world)
}

/// Clamp HMD 偏移到 validate 区间。
pub fn clamp_hmd_offsets(o: HmdOffsets) -> HmdOffsets {
    HmdOffsets {
        horizontal_offset_m: clamp_f32(o.horizontal_offset_m, OFFSET_M_RANGE.0, OFFSET_M_RANGE.1),
        height_offset_m: clamp_f32(o.height_offset_m, OFFSET_M_RANGE.0, OFFSET_M_RANGE.1),
        distance_m: clamp_f32(o.distance_m, DISTANCE_M_RANGE.0, DISTANCE_M_RANGE.1),
    }
}

/// Clamp 世界坐标每轴到 validate 区间。
pub fn clamp_world_position(p: [f32; 3]) -> [f32; 3] {
    [
        clamp_f32(p[0], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
        clamp_f32(p[1], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
        clamp_f32(p[2], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
    ]
}

/// 抓取组合（与配置 `gesture.grab` 对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabChord {
    BothGrips,
    RightGrip,
    RightTrigger,
}

impl GrabChord {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "both_grips" => Some(Self::BothGrips),
            "right_grip" => Some(Self::RightGrip),
            "right_trigger" => Some(Self::RightTrigger),
            _ => None,
        }
    }

    /// 该组合是否需要左手柄参与（决定左手 tracking 失效时是否取消手势）。
    pub fn needs_left_hand(self) -> bool {
        matches!(self, Self::BothGrips)
    }

    /// `both_grips` 必须 pointer 命中才算激活；单手模式只需对应键。
    pub fn is_active(
        self,
        left_grip: bool,
        right_grip: bool,
        right_trigger: bool,
        pointer_hit: bool,
    ) -> bool {
        match self {
            Self::BothGrips => left_grip && right_grip && pointer_hit,
            Self::RightGrip => right_grip,
            Self::RightTrigger => right_trigger,
        }
    }
}

/// VR 内解锁组合状态机：双手 trigger 同时按住 1.5s，且 pointer 命中面板。
///
/// 独立于拖拽状态机：锁定时拖拽完全不被响应，但仍然可以只读轮询手柄来识别解锁。
/// 触发后进入冷却，避免一次长按反复解锁。
#[derive(Debug, Clone, Default)]
pub struct UnlockGestureState {
    hold_since: Option<Duration>,
    last_fire: Option<Duration>,
}

impl UnlockGestureState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 组合中断时清理持续计时（冷却保留）。
    pub fn reset(&mut self) {
        self.hold_since = None;
    }

    /// 推进状态机；恰好完成一次长按时返回 `true`。
    pub fn update(
        &mut self,
        left_trigger: bool,
        right_trigger: bool,
        pointer_on_panel: bool,
        now: Duration,
    ) -> bool {
        if self
            .last_fire
            .map(|fired| now.saturating_sub(fired) < UNLOCK_COOLDOWN)
            .unwrap_or(false)
        {
            self.hold_since = None;
            return false;
        }
        if !(left_trigger && right_trigger && pointer_on_panel) {
            self.hold_since = None;
            return false;
        }
        let Some(since) = self.hold_since else {
            self.hold_since = Some(now);
            return false;
        };
        if now.saturating_sub(since) < UNLOCK_HOLD {
            return false;
        }
        self.hold_since = None;
        self.last_fire = Some(now);
        true
    }
}

/// 状态机相位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    Locked,
    Idle,
    Arming,
    Dragging,
}

/// 一帧输入快照（由 imp 从 legacy 控制器填充）。
#[derive(Debug, Clone, Copy)]
pub struct GestureFrameInput {
    pub locked: bool,
    pub left_grip: bool,
    pub right_grip: bool,
    pub right_trigger: bool,
    /// 射线命中面板矩形（含边距）；`both_grips` arming 硬性要求。
    pub pointer_on_panel: bool,
    pub config_epoch: u64,
    pub now: Duration,
}

/// 状态机对外事件（主线程据此决定是否 persist）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureTransition {
    None,
    /// 进入拖动（可触发 haptic）。
    DragStarted,
    /// 松手，应写回最终位姿。
    DragReleased,
    /// 取消（epoch/锁定等），不写回。
    DragCancelled,
}

/// 手势状态机：locked / idle / arming / dragging。
#[derive(Debug, Clone)]
pub struct GestureState {
    phase: GesturePhase,
    arming_since: Option<Duration>,
    drag_epoch: u64,
    prev_hit: Option<[f32; 3]>,
    /// 拖动开始时的 HMD 偏移（累加基准）。
    drag_start_hmd: Option<HmdOffsets>,
    /// 拖动开始时的世界坐标。
    drag_start_world: Option<[f32; 3]>,
}

impl Default for GestureState {
    fn default() -> Self {
        Self::new()
    }
}

impl GestureState {
    pub fn new() -> Self {
        Self {
            phase: GesturePhase::Locked,
            arming_since: None,
            drag_epoch: 0,
            prev_hit: None,
            drag_start_hmd: None,
            drag_start_world: None,
        }
    }

    pub fn phase(&self) -> GesturePhase {
        self.phase
    }

    pub fn is_dragging(&self) -> bool {
        self.phase == GesturePhase::Dragging
    }

    pub fn prev_hit(&self) -> Option<[f32; 3]> {
        self.prev_hit
    }

    pub fn prev_hit_mut(&mut self) -> &mut Option<[f32; 3]> {
        &mut self.prev_hit
    }

    pub fn drag_start_hmd(&self) -> Option<HmdOffsets> {
        self.drag_start_hmd
    }

    pub fn drag_start_world(&self) -> Option<[f32; 3]> {
        self.drag_start_world
    }

    /// 强制取消拖动/arming（Dashboard、追踪失效、断开等）。
    pub fn cancel(&mut self) -> GestureTransition {
        let was_dragging = self.phase == GesturePhase::Dragging;
        let was_locked = self.phase == GesturePhase::Locked;
        self.clear_drag();
        self.phase = if was_locked {
            GesturePhase::Locked
        } else {
            GesturePhase::Idle
        };
        if was_dragging {
            GestureTransition::DragCancelled
        } else {
            GestureTransition::None
        }
    }

    fn clear_drag(&mut self) {
        self.arming_since = None;
        self.prev_hit = None;
        self.drag_start_hmd = None;
        self.drag_start_world = None;
        if self.phase == GesturePhase::Arming || self.phase == GesturePhase::Dragging {
            self.phase = GesturePhase::Idle;
        }
    }

    /// 推进状态机。`grab` 来自配置；`start_hmd`/`start_world` 在进入拖动时锁定为基准。
    pub fn update(
        &mut self,
        input: &GestureFrameInput,
        grab: GrabChord,
        start_hmd: HmdOffsets,
        start_world: [f32; 3],
    ) -> GestureTransition {
        if input.locked {
            let was_dragging = self.phase == GesturePhase::Dragging;
            self.arming_since = None;
            self.prev_hit = None;
            self.drag_start_hmd = None;
            self.drag_start_world = None;
            self.phase = GesturePhase::Locked;
            return if was_dragging {
                GestureTransition::DragCancelled
            } else {
                GestureTransition::None
            };
        }

        if self.phase == GesturePhase::Locked {
            self.phase = GesturePhase::Idle;
        }

        // epoch 变化：取消进行中的 arming/dragging（仅 dragging 视为需丢弃写回）
        if matches!(self.phase, GesturePhase::Arming | GesturePhase::Dragging)
            && input.config_epoch != self.drag_epoch
        {
            let was_dragging = self.phase == GesturePhase::Dragging;
            self.clear_drag();
            return if was_dragging {
                GestureTransition::DragCancelled
            } else {
                GestureTransition::None
            };
        }

        let chord_active = grab.is_active(
            input.left_grip,
            input.right_grip,
            input.right_trigger,
            input.pointer_on_panel,
        );

        match self.phase {
            GesturePhase::Locked => GestureTransition::None,
            GesturePhase::Idle => {
                if chord_active {
                    self.phase = GesturePhase::Arming;
                    self.arming_since = Some(input.now);
                    self.drag_epoch = input.config_epoch;
                }
                GestureTransition::None
            }
            GesturePhase::Arming => {
                if !chord_active {
                    self.clear_drag();
                    return GestureTransition::None;
                }
                let since = self.arming_since.unwrap_or(input.now);
                if input.now.saturating_sub(since) >= ARMING_HOLD {
                    self.phase = GesturePhase::Dragging;
                    self.arming_since = None;
                    self.prev_hit = None;
                    self.drag_start_hmd = Some(start_hmd);
                    self.drag_start_world = Some(start_world);
                    self.drag_epoch = input.config_epoch;
                    return GestureTransition::DragStarted;
                }
                GestureTransition::None
            }
            GesturePhase::Dragging => {
                if !chord_active {
                    // 松手：保留 start_* 供调用方读出最终位姿后再 take_drag_origins
                    self.phase = GesturePhase::Idle;
                    self.arming_since = None;
                    self.prev_hit = None;
                    return GestureTransition::DragReleased;
                }
                // epoch 变化已在函数入口统一取消，这里无需重复判断
                GestureTransition::None
            }
        }
    }

    /// 松手后取出拖动起点并清空（调用方已算完最终位姿）。
    pub fn take_drag_origins(&mut self) -> (Option<HmdOffsets>, Option<[f32; 3]>) {
        let hmd = self.drag_start_hmd.take();
        let world = self.drag_start_world.take();
        self.prev_hit = None;
        (hmd, world)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq3(a: [f32; 3], b: [f32; 3], eps: f32) {
        for i in 0..3 {
            assert!(
                (a[i] - b[i]).abs() <= eps,
                "axis {i}: {a:?} vs {b:?} (eps={eps})"
            );
        }
    }

    #[test]
    fn mat_transform_point_and_multiply() {
        let t = Mat3x4([
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 2.0],
            [0.0, 0.0, 1.0, 3.0],
        ]);
        assert_eq!(t.transform_point([0.0, 0.0, 0.0]), [1.0, 2.0, 3.0]);

        // 绕 Y 90°：x' = z, z' = -x
        let rot = Mat3x4([
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
        ]);
        let p = rot.transform_point([1.0, 0.0, 0.0]);
        approx_eq3(p, [0.0, 0.0, -1.0], 1e-5);

        let h = Mat3x4([
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        let l = Mat3x4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.5],
            [0.0, 0.0, 1.0, -2.0],
        ]);
        let w = h.multiply(&l);
        assert_eq!(w.translation(), [10.0, 0.5, -2.0]);
    }

    #[test]
    fn rotation_transpose_mul_undoes_rotation() {
        let rot = Mat3x4([
            [0.0, 0.0, 1.0, 5.0],
            [0.0, 1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
        ]);
        let v = [0.0, 0.0, -1.0];
        let world = rot.transform_point(v);
        // 仅旋转部分：R * [0,0,-1] = [ -1, 0, 0 ]（含平移前）
        let rotated = [
            rot.0[0][0] * v[0] + rot.0[0][1] * v[1] + rot.0[0][2] * v[2],
            rot.0[1][0] * v[0] + rot.0[1][1] * v[1] + rot.0[1][2] * v[2],
            rot.0[2][0] * v[0] + rot.0[2][1] * v[1] + rot.0[2][2] * v[2],
        ];
        let back = rot.rotation_transpose_mul(rotated);
        approx_eq3(back, v, 1e-5);
        let _ = world;
    }

    #[test]
    fn controller_forward_is_local_neg_z() {
        let m = Mat3x4::IDENTITY;
        assert_eq!(controller_forward(&m), [0.0, 0.0, -1.0]);
    }

    #[test]
    fn ray_parallel_returns_none() {
        let hit = ray_plane_intersection(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -2.0],
            [0.0, 0.0, 1.0],
        );
        assert!(hit.is_none());
    }

    #[test]
    fn ray_behind_plane_returns_none() {
        // 射线沿 +Z，平面在 -Z，法线 +Z → t < 0
        let hit = ray_plane_intersection(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -2.0],
            [0.0, 0.0, 1.0],
        );
        assert!(hit.is_none());
    }

    #[test]
    fn ray_hits_plane_ahead() {
        let hit = ray_plane_intersection(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [0.0, 0.0, -2.0],
            [0.0, 0.0, 1.0],
        )
        .expect("hit");
        approx_eq3(hit, [0.0, 0.0, -2.0], 1e-5);
    }

    #[test]
    fn hit_outside_panel_rejected() {
        let center = [0.0, 0.0, -2.0];
        let right = [1.0, 0.0, 0.0];
        let up = [0.0, 1.0, 0.0];
        assert!(hit_in_panel_rect(
            [0.1, 0.0, -2.0],
            center,
            right,
            up,
            0.5,
            0.5,
            0.0
        ));
        assert!(!hit_in_panel_rect(
            [0.9, 0.0, -2.0],
            center,
            right,
            up,
            0.5,
            0.5,
            0.0
        ));
    }

    #[test]
    fn frame_delta_is_capped() {
        let d = limit_frame_delta([1.0, 0.0, 0.0], MAX_FRAME_DELTA_M);
        approx_eq3(d, [MAX_FRAME_DELTA_M, 0.0, 0.0], 1e-5);
    }

    #[test]
    fn drag_delta_skips_first_frame_and_none_hit() {
        let mut prev = None;
        assert!(drag_world_delta(Some([1.0, 0.0, 0.0]), &mut prev, 1.0, 0.25).is_none());
        assert_eq!(prev, Some([1.0, 0.0, 0.0]));
        let d = drag_world_delta(Some([1.1, 0.0, 0.0]), &mut prev, 1.0, 0.25).unwrap();
        approx_eq3(d, [0.1, 0.0, 0.0], 1e-5);
        assert!(drag_world_delta(None, &mut prev, 1.0, 0.25).is_none());
        assert_eq!(prev, Some([1.1, 0.0, 0.0]));
    }

    #[test]
    fn hmd_delta_maps_right_and_distance() {
        let start = HmdOffsets {
            horizontal_offset_m: 0.0,
            height_offset_m: 0.0,
            distance_m: 2.0,
        };
        // identity HMD：世界 +X → horizontal；世界 -Z → distance 减小
        let hmd = Mat3x4::IDENTITY;
        let after_right = apply_hmd_delta(start, &hmd, [0.2, 0.0, 0.0]);
        assert!((after_right.horizontal_offset_m - 0.2).abs() < 1e-5);
        let after_fwd = apply_hmd_delta(start, &hmd, [0.0, 0.0, -0.3]);
        // delta_local.z = -0.3 → distance = 2 - (-0.3) = 2.3
        assert!((after_fwd.distance_m - 2.3).abs() < 1e-5);
    }

    #[test]
    fn world_delta_adds_position() {
        assert_eq!(
            apply_world_delta([1.0, 2.0, 3.0], [0.5, -0.25, 1.0]),
            [1.5, 1.75, 4.0]
        );
    }

    #[test]
    fn clamp_pose_fields_to_validate_ranges() {
        let o = clamp_hmd_offsets(HmdOffsets {
            horizontal_offset_m: 9.0,
            height_offset_m: -9.0,
            distance_m: 0.01,
        });
        assert_eq!(o.horizontal_offset_m, 2.0);
        assert_eq!(o.height_offset_m, -2.0);
        assert_eq!(o.distance_m, 0.2);

        let o2 = clamp_hmd_offsets(HmdOffsets {
            horizontal_offset_m: 0.0,
            height_offset_m: 0.0,
            distance_m: 99.0,
        });
        assert_eq!(o2.distance_m, 10.0);

        let p = clamp_world_position([200.0, -200.0, 0.0]);
        assert_eq!(p, [100.0, -100.0, 0.0]);
    }

    fn frame(
        locked: bool,
        left: bool,
        right: bool,
        pointer: bool,
        epoch: u64,
        ms: u64,
    ) -> GestureFrameInput {
        GestureFrameInput {
            locked,
            left_grip: left,
            right_grip: right,
            right_trigger: false,
            pointer_on_panel: pointer,
            config_epoch: epoch,
            now: Duration::from_millis(ms),
        }
    }

    #[test]
    fn gesture_locked_ignores_grips() {
        let mut s = GestureState::new();
        let t = s.update(
            &frame(true, true, true, true, 1, 0),
            GrabChord::BothGrips,
            HmdOffsets::from_config_fields(0.0, 0.0, 2.0),
            [0.0; 3],
        );
        assert_eq!(t, GestureTransition::None);
        assert_eq!(s.phase(), GesturePhase::Locked);
    }

    #[test]
    fn both_grips_requires_pointer_to_arm() {
        let mut s = GestureState::new();
        let hmd = HmdOffsets::from_config_fields(0.0, 0.0, 2.0);
        // unlock
        let _ = s.update(
            &frame(false, false, false, false, 1, 0),
            GrabChord::BothGrips,
            hmd,
            [0.0; 3],
        );
        assert_eq!(s.phase(), GesturePhase::Idle);
        // grips without pointer → still idle
        let _ = s.update(
            &frame(false, true, true, false, 1, 10),
            GrabChord::BothGrips,
            hmd,
            [0.0; 3],
        );
        assert_eq!(s.phase(), GesturePhase::Idle);
        // with pointer → arming
        let _ = s.update(
            &frame(false, true, true, true, 1, 20),
            GrabChord::BothGrips,
            hmd,
            [0.0; 3],
        );
        assert_eq!(s.phase(), GesturePhase::Arming);
    }

    #[test]
    fn arming_timeout_then_release_and_epoch_cancel() {
        let mut s = GestureState::new();
        let hmd = HmdOffsets::from_config_fields(0.1, -0.2, 1.5);
        let world = [1.0, 2.0, 3.0];
        let _ = s.update(
            &frame(false, false, false, false, 5, 0),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        let _ = s.update(
            &frame(false, true, true, true, 5, 10),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(s.phase(), GesturePhase::Arming);
        // too early
        let t = s.update(
            &frame(false, true, true, true, 5, 200),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(t, GestureTransition::None);
        assert_eq!(s.phase(), GesturePhase::Arming);
        // past 250ms
        let t = s.update(
            &frame(false, true, true, true, 5, 270),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(t, GestureTransition::DragStarted);
        assert_eq!(s.phase(), GesturePhase::Dragging);
        assert_eq!(s.drag_start_hmd(), Some(hmd));

        // epoch change cancels
        let t = s.update(
            &frame(false, true, true, true, 6, 300),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(t, GestureTransition::DragCancelled);
        assert_eq!(s.phase(), GesturePhase::Idle);

        // re-arm and release
        let _ = s.update(
            &frame(false, true, true, true, 6, 400),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        let _ = s.update(
            &frame(false, true, true, true, 6, 700),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(s.phase(), GesturePhase::Dragging);
        let t = s.update(
            &frame(false, false, true, true, 6, 710),
            GrabChord::BothGrips,
            hmd,
            world,
        );
        assert_eq!(t, GestureTransition::DragReleased);
        assert_eq!(s.phase(), GesturePhase::Idle);
    }

    #[test]
    fn nearly_parallel_helper() {
        assert!(ray_nearly_parallel([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
        assert!(!ray_nearly_parallel([0.0, 0.0, -1.0], [0.0, 0.0, 1.0]));
    }

    #[test]
    fn unlock_requires_both_triggers_on_panel_for_full_hold() {
        let mut s = UnlockGestureState::new();
        let ms = Duration::from_millis;
        // 没命中面板不算
        assert!(!s.update(true, true, false, ms(0)));
        assert!(!s.update(true, true, false, ms(2000)));
        // 只按一边不算
        assert!(!s.update(true, false, true, ms(2100)));
        // 双方按下但未满 1.5s
        assert!(!s.update(true, true, true, ms(2200)));
        assert!(!s.update(true, true, true, ms(3600)));
        // 满 1.5s 触发一次
        assert!(s.update(true, true, true, ms(3800)));
        // 冷却期内不重复触发
        assert!(!s.update(true, true, true, ms(3900)));
        assert!(!s.update(true, true, true, ms(4700)));
        // 冷却结束后重新计时
        assert!(!s.update(true, true, true, ms(4900)));
        assert!(s.update(true, true, true, ms(6500)));
    }

    #[test]
    fn unlock_reset_clears_partial_hold() {
        let mut s = UnlockGestureState::new();
        let ms = Duration::from_millis;
        assert!(!s.update(true, true, true, ms(0)));
        s.reset();
        // 重新计时后不足 1.5s 不应触发
        assert!(!s.update(true, true, true, ms(1600)));
    }

    #[test]
    fn only_both_grips_needs_left_hand() {
        assert!(GrabChord::BothGrips.needs_left_hand());
        assert!(!GrabChord::RightGrip.needs_left_hand());
        assert!(!GrabChord::RightTrigger.needs_left_hand());
    }
}
