//! SteamVR 手势定位纯函数（3C / v0.4）：矩阵、射线、增量换算、free 映射与状态机。
//!
//! 不依赖 `openvr` 类型，便于非 Windows 单测。
//! 权威：docs/V0.3_FEATURE_PLAN.md §4.2–§4.8；docs/V0.4_GESTURE_POSE_PLAN.md §3 / §4 / §6.3。
//!
//! # 可调常量（本文件顶部，真机调参改这里）
//!
//! | 常量 | 用途 |
//! | --- | --- |
//! | [`R_MIN`] | free+ray：控制器相对 HMD pivot 距离下限，防方位角退化 |
//! | [`EL_MAX`] | free+ray：面板仰角绝对值上限（默认 75°） |
//! | [`DEPTH_GAIN`] | free+ray：深度比例增益（1/米）；实际 `G = DEPTH_GAIN * sensitivity` |
//! | [`JUMP_M`] | free：手柄单帧位移超此值则丢帧 + rebase |
//! | [`MAX_FRAME_DELTA_M`] | plane：单帧世界位移上限（free 不用） |
//!
//! 拖动轮询提频见 `imp.rs` 的 `FASTER_INTERVAL_WHILE_DRAGGING` / `DRAGGING_FRAME_INTERVAL`。

use std::time::Duration;

/// 双手 grip 进入拖动前的按住时长。
pub const ARMING_HOLD: Duration = Duration::from_millis(250);

/// VR 内解锁组合（双手 trigger 同时按住）的时长。
pub const UNLOCK_HOLD: Duration = Duration::from_millis(1500);

/// VR 内解锁触发后的冷却，避免一次长按被判定成多次。
pub const UNLOCK_COOLDOWN: Duration = Duration::from_millis(1000);

/// 单帧世界位移上限（米），防止追踪丢失/近平行射线瞬移。
/// `space = plane` 增量路径使用；`space = free` 改用 [`JUMP_M`] 丢帧 + rebase。
pub const MAX_FRAME_DELTA_M: f32 = 0.25;

/// free + ray：手贴脸时控制器相对 pivot 距离下限（米），防方位角退化。
pub const R_MIN: f32 = 0.15;

/// free + ray：面板仰角绝对值上限（弧度，75°），远离极点奇异。
pub const EL_MAX: f32 = 75.0_f32 * std::f32::consts::PI / 180.0;

/// free + ray：深度比例增益（1/米）。实际 `G = DEPTH_GAIN * sensitivity`。
/// `sensitivity=1` 时前推 0.3m ≈ ×e^{0.9} ≈ 2.46。
pub const DEPTH_GAIN: f32 = 3.0;

/// free：手柄单帧位移超过此阈值（米）则丢弃本帧并在下一有效帧 rebase。
pub const JUMP_M: f32 = 0.3;

/// `controller_yaw`：手柄前向水平投影短于此（米，前向近似单位向量时≈cos 俯仰）则冻结扭转。
pub const CONTROLLER_YAW_MIN_HORIZ: f32 = 0.2;

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
    clamp_hmd_offsets_checked(o).0
}

/// Clamp HMD 偏移；`true` 表示至少一维触边界（用于每会话一次 haptic）。
pub fn clamp_hmd_offsets_checked(o: HmdOffsets) -> (HmdOffsets, bool) {
    let c = HmdOffsets {
        horizontal_offset_m: clamp_f32(o.horizontal_offset_m, OFFSET_M_RANGE.0, OFFSET_M_RANGE.1),
        height_offset_m: clamp_f32(o.height_offset_m, OFFSET_M_RANGE.0, OFFSET_M_RANGE.1),
        distance_m: clamp_f32(o.distance_m, DISTANCE_M_RANGE.0, DISTANCE_M_RANGE.1),
    };
    let hit = c.horizontal_offset_m != o.horizontal_offset_m
        || c.height_offset_m != o.height_offset_m
        || c.distance_m != o.distance_m;
    (c, hit)
}

/// Clamp 世界坐标每轴到 validate 区间。
pub fn clamp_world_position(p: [f32; 3]) -> [f32; 3] {
    clamp_world_position_checked(p).0
}

/// Clamp 世界坐标；`true` 表示至少一轴触边界（用于每会话一次 haptic）。
pub fn clamp_world_position_checked(p: [f32; 3]) -> ([f32; 3], bool) {
    let c = [
        clamp_f32(p[0], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
        clamp_f32(p[1], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
        clamp_f32(p[2], WORLD_POSITION_RANGE.0, WORLD_POSITION_RANGE.1),
    ];
    let hit = c[0] != p[0] || c[1] != p[1] || c[2] != p[2];
    (c, hit)
}

/// 将弧度包到 `(-π, π]`。
pub fn wrap_pi(a: f32) -> f32 {
    let pi = std::f32::consts::PI;
    let tau = 2.0 * pi;
    // rem_euclid → [0, τ)；减 π → [-π, π)；再把 -π 映到 +π。
    let mut x = (a + pi).rem_euclid(tau) - pi;
    if x <= -pi {
        x = pi;
    }
    x
}

/// 将角度（度）归一化到 `(-180, 180]`（写回 `world_rotation_deg[1]` 前使用）。
pub fn normalize_deg(deg: f32) -> f32 {
    let mut d = deg % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// 向量的方位角 / 仰角（弧度）：`az = atan2(x, z)`，`el = atan2(y, hypot(x,z))`。
///
/// 零向量返回 `(fallback_az, fallback_el)`。
pub fn az_el_of(v: [f32; 3], fallback_az: f32, fallback_el: f32) -> (f32, f32) {
    let horiz = (v[0] * v[0] + v[2] * v[2]).sqrt();
    if horiz < 1e-8 && v[1].abs() < 1e-8 {
        return (fallback_az, fallback_el);
    }
    (v[0].atan2(v[2]), v[1].atan2(horiz))
}

/// 由方位角 / 仰角得到单位方向（Y-up，az=0 朝 +Z）。
pub fn dir_from_az_el(az: f32, el: f32) -> [f32; 3] {
    let (sin_el, cos_el) = el.sin_cos();
    let (sin_az, cos_az) = az.sin_cos();
    [cos_el * sin_az, sin_el, cos_el * cos_az]
}

/// 刚体逆：`R^T`，`t' = -R^T t`（假定旋转正交）。
pub fn mat3x4_inverse_rigid(m: &Mat3x4) -> Mat3x4 {
    let t = m.translation();
    let nt = m.rotation_transpose_mul(t);
    Mat3x4([
        [m.0[0][0], m.0[1][0], m.0[2][0], -nt[0]],
        [m.0[0][1], m.0[1][1], m.0[2][1], -nt[1]],
        [m.0[0][2], m.0[1][2], m.0[2][2], -nt[2]],
    ])
}

/// 面板正对 `pivot` 的偏航（弧度）：水平化 `(pivot - p)` 后 `atan2(x, z)`。
///
/// 面板可视面朝局部 +Z；与 [`compose_world_rows`] / `world_matrix_rows` 一致。
pub fn face_user_yaw(pivot: [f32; 3], p: [f32; 3]) -> f32 {
    let fx = pivot[0] - p[0];
    let fz = pivot[2] - p[2];
    if fx.abs() < 1e-8 && fz.abs() < 1e-8 {
        return 0.0;
    }
    // `+ 0.0` 去掉符号零，避免 atan2(-0, -1) = -π 与 +π 抖动
    wrap_pi((fx + 0.0).atan2(fz + 0.0))
}

/// 手柄前向在水平面投影的方位角（弧度）。投影过短（近竖直）返回 `None`。
pub fn controller_yaw(m: &Mat3x4) -> Option<f32> {
    let f = controller_forward(m);
    let horiz_len = (f[0] * f[0] + f[2] * f[2]).sqrt();
    if horiz_len < CONTROLLER_YAW_MIN_HORIZ {
        None
    } else {
        Some(wrap_pi((f[0] + 0.0).atan2(f[2] + 0.0)))
    }
}

/// 站立系：仅绕 Y 的偏航矩阵（弧度）+ 平移。与 `imp::world_matrix_rows` 同构。
pub fn compose_world_rows(pos: [f32; 3], yaw_rad: f32) -> Mat3x4 {
    let (sin, cos) = yaw_rad.sin_cos();
    let [x, y, z] = pos;
    Mat3x4([[cos, 0.0, sin, x], [0.0, 1.0, 0.0, y], [-sin, 0.0, cos, z]])
}

/// 从仅偏航的世界矩阵提取 yaw（弧度）：`atan2(r02, r00)`。
pub fn extract_yaw(m: &Mat3x4) -> f32 {
    m.0[0][2].atan2(m.0[0][0])
}

/// free + ray 抓取基准（[`map_sphere`] 输入）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SphereGrab {
    pub pivot: [f32; 3],
    pub r0: f32,
    pub az_c0: f32,
    pub el_c0: f32,
    pub d0: f32,
    pub az_p0: f32,
    pub el_p0: f32,
}

impl SphereGrab {
    /// 抓取瞬间由冻结的 HMD pivot、手柄位置、面板位置初始化。
    pub fn from_poses(pivot: [f32; 3], controller_pos: [f32; 3], panel_pos: [f32; 3]) -> Self {
        let v0 = sub3(controller_pos, pivot);
        let r0 = length3(v0).max(R_MIN);
        let (az_c0, el_c0) = az_el_of(v0, 0.0, 0.0);
        let u0 = sub3(panel_pos, pivot);
        let d0 = length3(u0);
        let (az_p0, el_p0) = az_el_of(u0, 0.0, 0.0);
        Self {
            pivot,
            r0,
            az_c0,
            el_c0,
            d0,
            az_p0,
            el_p0,
        }
    }
}

/// free + ray：以头为中心的球坐标 + 比例深度映射，返回面板世界位置。
///
/// `sensitivity` 乘到 [`DEPTH_GAIN`] 上；`d` 在映射内 clamp 到 [`DISTANCE_M_RANGE`]。
pub fn map_sphere(grab: &SphereGrab, controller_pos: [f32; 3], sensitivity: f32) -> [f32; 3] {
    let v = sub3(controller_pos, grab.pivot);
    let r_raw = length3(v);
    let (az_c, el_c) = az_el_of(v, grab.az_c0, grab.el_c0);
    let r = r_raw.max(R_MIN);
    let daz = wrap_pi(az_c - grab.az_c0);
    let del = el_c - grab.el_c0;
    let az_p = grab.az_p0 + daz;
    let el_p = clamp_f32(grab.el_p0 + del, -EL_MAX, EL_MAX);
    let g = DEPTH_GAIN * sensitivity;
    let d = clamp_f32(
        grab.d0 * (g * (r - grab.r0)).exp(),
        DISTANCE_M_RANGE.0,
        DISTANCE_M_RANGE.1,
    );
    add3(grab.pivot, scale3(dir_from_az_el(az_p, el_p), d))
}

/// free + near：1:1 位置跟随（`t_attach = P0 - C0`，不用控制器旋转）。
pub fn map_follow(controller_pos: [f32; 3], t_attach: [f32; 3]) -> [f32; 3] {
    add3(controller_pos, t_attach)
}

/// 由当前 HMD 刚体与面板世界位置反推 hmd 锚点偏移（未 clamp）。
///
/// `L = inv(H) · p` → `horizontal=L.x`，`height=L.y`，`distance=-L.z`。
pub fn hmd_offsets_from_world(hmd: &Mat3x4, panel_world: [f32; 3]) -> HmdOffsets {
    let inv = mat3x4_inverse_rigid(hmd);
    let local = inv.transform_point(panel_world);
    HmdOffsets {
        horizontal_offset_m: local[0],
        height_offset_m: local[1],
        distance_m: -local[2],
    }
}

/// 用 `src` 的旋转列 + 新平移构造矩阵（hmd 锚点拖动期：当前 HMD 旋转 + 映射位置）。
pub fn mat_with_translation(src: &Mat3x4, pos: [f32; 3]) -> Mat3x4 {
    Mat3x4([
        [src.0[0][0], src.0[0][1], src.0[0][2], pos[0]],
        [src.0[1][0], src.0[1][1], src.0[1][2], pos[1]],
        [src.0[2][0], src.0[2][1], src.0[2][2], pos[2]],
    ])
}

/// free 拖动映射模式（`gesture.mode`：ray→Sphere，near→Follow）。
#[derive(Debug, Clone, PartialEq)]
pub enum FreeDragMode {
    Sphere {
        grab: SphereGrab,
        yaw_ctrl0: f32,
        yaw_at_grab: f32,
        /// `controller_yaw`：上一帧有效的手柄扭转偏移；投影过短时沿用。
        last_yaw_ctrl_delta: f32,
    },
    Follow {
        pivot: [f32; 3],
        t_attach: [f32; 3],
        yaw_ctrl0: f32,
        yaw_at_grab: f32,
        last_yaw_ctrl_delta: f32,
    },
}

/// free 空间拖动会话（`DragStarted` 当帧初始化；imp 持有）。
#[derive(Debug, Clone, PartialEq)]
pub struct FreeDragSession {
    pub mode: FreeDragMode,
    pub prev_controller: [f32; 3],
    pub needs_rebase: bool,
    pub panel: Mat3x4,
}

impl FreeDragSession {
    /// `ray_mode=true` → 球坐标；`false` → 1:1 跟随。
    pub fn start(ray_mode: bool, hmd: &Mat3x4, controller: &Mat3x4, panel: Mat3x4) -> Self {
        let pivot = hmd.translation();
        let cpos = controller.translation();
        let ppos = panel.translation();
        let yaw_at_grab = extract_yaw(&panel);
        let yaw_ctrl0 = controller_yaw(controller).unwrap_or(0.0);
        let mode = if ray_mode {
            FreeDragMode::Sphere {
                grab: SphereGrab::from_poses(pivot, cpos, ppos),
                yaw_ctrl0,
                yaw_at_grab,
                last_yaw_ctrl_delta: 0.0,
            }
        } else {
            FreeDragMode::Follow {
                pivot,
                t_attach: sub3(ppos, cpos),
                yaw_ctrl0,
                yaw_at_grab,
                last_yaw_ctrl_delta: 0.0,
            }
        };
        Self {
            mode,
            prev_controller: cpos,
            needs_rebase: false,
            panel,
        }
    }

    fn pivot(&self) -> [f32; 3] {
        match &self.mode {
            FreeDragMode::Sphere { grab, .. } => grab.pivot,
            FreeDragMode::Follow { pivot, .. } => *pivot,
        }
    }

    fn yaw_at_grab(&self) -> f32 {
        match &self.mode {
            FreeDragMode::Sphere { yaw_at_grab, .. } | FreeDragMode::Follow { yaw_at_grab, .. } => {
                *yaw_at_grab
            }
        }
    }

    fn yaw_ctrl0(&self) -> f32 {
        match &self.mode {
            FreeDragMode::Sphere { yaw_ctrl0, .. } | FreeDragMode::Follow { yaw_ctrl0, .. } => {
                *yaw_ctrl0
            }
        }
    }

    fn last_yaw_ctrl_delta_mut(&mut self) -> &mut f32 {
        match &mut self.mode {
            FreeDragMode::Sphere {
                last_yaw_ctrl_delta,
                ..
            }
            | FreeDragMode::Follow {
                last_yaw_ctrl_delta,
                ..
            } => last_yaw_ctrl_delta,
        }
    }

    /// 以当前面板 + 手柄重建抓取基准（毛刺丢帧后的下一有效帧）。pivot 保持冻结。
    pub fn rebase(&mut self, controller: &Mat3x4) {
        let cpos = controller.translation();
        let ppos = self.panel.translation();
        let yaw_ctrl = controller_yaw(controller).unwrap_or(self.yaw_ctrl0());
        match &mut self.mode {
            FreeDragMode::Sphere {
                grab,
                yaw_ctrl0,
                yaw_at_grab,
                last_yaw_ctrl_delta,
            } => {
                let pivot = grab.pivot;
                *grab = SphereGrab::from_poses(pivot, cpos, ppos);
                *yaw_ctrl0 = yaw_ctrl;
                *yaw_at_grab = extract_yaw(&self.panel);
                *last_yaw_ctrl_delta = 0.0;
            }
            FreeDragMode::Follow {
                t_attach,
                yaw_ctrl0,
                yaw_at_grab,
                last_yaw_ctrl_delta,
                ..
            } => {
                *t_attach = sub3(ppos, cpos);
                *yaw_ctrl0 = yaw_ctrl;
                *yaw_at_grab = extract_yaw(&self.panel);
                *last_yaw_ctrl_delta = 0.0;
            }
        }
        self.prev_controller = cpos;
        self.needs_rebase = false;
    }

    /// 计算 world 锚点偏航（弧度）。`orientation`：face_user / keep / controller_yaw。
    pub fn resolve_yaw(
        &mut self,
        orientation: &str,
        panel_pos: [f32; 3],
        controller: &Mat3x4,
    ) -> f32 {
        let pivot = self.pivot();
        let yaw_face = face_user_yaw(pivot, panel_pos);
        match orientation {
            "keep" => self.yaw_at_grab(),
            "controller_yaw" => {
                let delta = if let Some(yaw_c) = controller_yaw(controller) {
                    let d = wrap_pi(yaw_c - self.yaw_ctrl0());
                    *self.last_yaw_ctrl_delta_mut() = d;
                    d
                } else {
                    *self.last_yaw_ctrl_delta_mut()
                };
                wrap_pi(yaw_face + delta)
            }
            _ => yaw_face, // face_user 及未知值
        }
    }

    /// 推进一帧。手柄跳变超过 [`JUMP_M`] 时丢帧并标记 rebase，返回 `false`。
    ///
    /// `world_anchor`：true 用 orientation 偏航直立矩阵；false 用当前 HMD 旋转 + 映射位置。
    pub fn tick(
        &mut self,
        controller: &Mat3x4,
        hmd: &Mat3x4,
        sensitivity: f32,
        orientation: &str,
        world_anchor: bool,
    ) -> bool {
        let cpos = controller.translation();
        if length3(sub3(cpos, self.prev_controller)) > JUMP_M {
            self.needs_rebase = true;
            self.prev_controller = cpos;
            return false;
        }
        if self.needs_rebase {
            self.rebase(controller);
        } else {
            self.prev_controller = cpos;
        }

        let p = match &self.mode {
            FreeDragMode::Sphere { grab, .. } => map_sphere(grab, cpos, sensitivity),
            FreeDragMode::Follow { t_attach, .. } => map_follow(cpos, *t_attach),
        };

        if world_anchor {
            let yaw = self.resolve_yaw(orientation, p, controller);
            self.panel = compose_world_rows(p, yaw);
        } else {
            self.panel = mat_with_translation(hmd, p);
        }
        true
    }

    /// 将当前面板钳到 validate 区间（拖动中触边提示 / 减少松手跳变）。
    ///
    /// - world：钳 `world_position` 范围，保留当前偏航。
    /// - hmd：经 [`hmd_offsets_from_world`] 钳偏移后再映回世界位置（当前 HMD 旋转）。
    ///
    /// 返回是否触边界。`world_anchor=false` 且缺 HMD 时不改动，返回 `false`。
    pub fn clamp_to_validate(&mut self, world_anchor: bool, hmd: Option<&Mat3x4>) -> bool {
        let p = self.panel.translation();
        if world_anchor {
            let (next, hit) = clamp_world_position_checked(p);
            if hit {
                let yaw = extract_yaw(&self.panel);
                self.panel = compose_world_rows(next, yaw);
            }
            hit
        } else if let Some(hmd_pose) = hmd {
            let (next, hit) = clamp_hmd_offsets_checked(hmd_offsets_from_world(hmd_pose, p));
            if hit {
                let local = Mat3x4([
                    [1.0, 0.0, 0.0, next.horizontal_offset_m],
                    [0.0, 1.0, 0.0, next.height_offset_m],
                    [0.0, 0.0, 1.0, -next.distance_m],
                ]);
                let clamped_p = hmd_pose.multiply(&local).translation();
                self.panel = mat_with_translation(hmd_pose, clamped_p);
            }
            hit
        } else {
            false
        }
    }
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
    ///
    /// free 拖动中请用 [`chord_pointer_for_space`] 喂入 `pointer_hit`：absolute 面板与
    /// `live_config` 命中不同步，拖动期若仍要求真实命中会误松手。
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

/// free 拖动中对 `both_grips` 的 pointer 门闩。
///
/// - arming / plane：返回真实 `pointer_on_panel`（进入抓取仍要命中）。
/// - free + dragging：返回 `true`，只靠握持维持拖动（命中相对抓取前 `live_config` 会假阴性）。
pub fn chord_pointer_for_space(space: &str, dragging: bool, pointer_on_panel: bool) -> bool {
    if dragging && space == "free" {
        true
    } else {
        pointer_on_panel
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

    #[test]
    fn clamp_checked_reports_boundary_hit() {
        let inside = HmdOffsets {
            horizontal_offset_m: 0.5,
            height_offset_m: -0.2,
            distance_m: 1.6,
        };
        let (_, hit_in) = clamp_hmd_offsets_checked(inside);
        assert!(!hit_in);

        let (_, hit_out) = clamp_hmd_offsets_checked(HmdOffsets {
            horizontal_offset_m: 9.0,
            height_offset_m: 0.0,
            distance_m: 1.6,
        });
        assert!(hit_out);

        let (_, w_in) = clamp_world_position_checked([1.0, 2.0, 3.0]);
        assert!(!w_in);
        let (wp, w_out) = clamp_world_position_checked([200.0, 0.0, 0.0]);
        assert!(w_out);
        assert_eq!(wp[0], 100.0);
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

    // --- A13: v0.4 free 映射 / 偏航 / 刚体逆 ---

    #[test]
    fn wrap_pi_and_normalize_deg_ranges() {
        let pi = std::f32::consts::PI;
        assert!((wrap_pi(0.0)).abs() < 1e-6);
        assert!((wrap_pi(pi) - pi).abs() < 1e-5);
        assert!((wrap_pi(-pi) - pi).abs() < 1e-5); // (-π, π]：-π → π
        assert!((wrap_pi(1.5 * pi) + 0.5 * pi).abs() < 1e-5);
        assert!((wrap_pi(-1.5 * pi) - 0.5 * pi).abs() < 1e-5);

        assert!((normalize_deg(0.0) - 0.0).abs() < 1e-5);
        assert!((normalize_deg(180.0) - 180.0).abs() < 1e-5);
        assert!((normalize_deg(-180.0) - 180.0).abs() < 1e-5);
        assert!((normalize_deg(190.0) + 170.0).abs() < 1e-5);
        assert!((normalize_deg(-190.0) - 170.0).abs() < 1e-5);
        assert!((normalize_deg(540.0) - 180.0).abs() < 1e-5);
        assert!((normalize_deg(-540.0) - 180.0).abs() < 1e-5);
    }

    #[test]
    fn az_el_dir_roundtrip() {
        let az = 0.7_f32;
        let el = -0.3_f32;
        let d = dir_from_az_el(az, el);
        assert!((length3(d) - 1.0).abs() < 1e-5);
        let (az2, el2) = az_el_of(d, 0.0, 0.0);
        assert!((az2 - az).abs() < 1e-5);
        assert!((el2 - el).abs() < 1e-5);
        // az=0 → +Z
        approx_eq3(dir_from_az_el(0.0, 0.0), [0.0, 0.0, 1.0], 1e-5);
    }

    #[test]
    fn compose_extract_yaw_inverse() {
        for &yaw_deg in &[-135.0_f32, -90.0, -45.0, 0.0, 30.0, 90.0, 179.0] {
            let yaw = yaw_deg.to_radians();
            let m = compose_world_rows([1.0, 2.0, 3.0], yaw);
            assert!((extract_yaw(&m) - yaw).abs() < 1e-5);
            // 与 imp::world_matrix_rows 同构：Z 列 = (sin, 0, cos)
            let (s, c) = yaw.sin_cos();
            assert!((m.0[0][2] - s).abs() < 1e-6);
            assert!((m.0[2][2] - c).abs() < 1e-6);
            assert_eq!(m.translation(), [1.0, 2.0, 3.0]);
        }
    }

    #[test]
    fn face_user_yaw_faces_pivot() {
        let pivot = [0.0, 1.6, 0.0];
        // 面板在 -Z：应朝向 +Z（yaw=0）
        let yaw0 = face_user_yaw(pivot, [0.0, 1.6, -2.0]);
        assert!(yaw0.abs() < 1e-5);
        // 面板在 +X：朝向 -X ⇒ yaw = -π/2（Z 列 = (-1,0,0)）
        let yaw_neg_x = face_user_yaw(pivot, [2.0, 1.6, 0.0]);
        assert!((yaw_neg_x + std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // 面板在 -X：朝向 +X ⇒ yaw = π/2
        let yaw_pos_x = face_user_yaw(pivot, [-2.0, 1.6, 0.0]);
        assert!((yaw_pos_x - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // 高度差不影响偏航
        let yaw_h = face_user_yaw(pivot, [0.0, 0.0, -2.0]);
        assert!(yaw_h.abs() < 1e-5);
    }

    #[test]
    fn map_sphere_depth_scales_proportionally() {
        let pivot = [0.0, 1.6, 0.0];
        // 手柄与面板同向 -Z；面板 1.5m，手柄 0.5m
        let c0 = [0.0, 1.6, -0.5];
        let p0 = [0.0, 1.6, -1.5];
        let grab = SphereGrab::from_poses(pivot, c0, p0);
        assert!((grab.d0 - 1.5).abs() < 1e-5);
        assert!((grab.r0 - 0.5).abs() < 1e-5);

        // 前推 0.3m（手柄更远）→ d ≈ 1.5 * exp(3*0.3) ≈ 1.5 * 2.4596 ≈ 3.69
        let p_fwd = map_sphere(&grab, [0.0, 1.6, -0.8], 1.0);
        let d_fwd = length3(sub3(p_fwd, pivot));
        assert!(
            d_fwd >= 1.5 + 0.8,
            "A1-like: push 0.3m from 1.5m should grow ≥0.8m, got d={d_fwd}"
        );
        assert!((d_fwd - 1.5 * (DEPTH_GAIN * 0.3).exp()).abs() < 1e-3);

        // 面板 3m，前推 0.3m → 增大 ≥ 1.5m（A2）
        let p0_far = [0.0, 1.6, -3.0];
        let grab_far = SphereGrab::from_poses(pivot, c0, p0_far);
        let p_far = map_sphere(&grab_far, [0.0, 1.6, -0.8], 1.0);
        let d_far = length3(sub3(p_far, pivot));
        assert!(
            d_far >= 3.0 + 1.5,
            "A2-like: from 3m push 0.3m should grow ≥1.5m, got d={d_far}"
        );
        assert!(d_far <= DISTANCE_M_RANGE.1 + 1e-5);

        // 后拉回到约 1.5m
        let p_back = map_sphere(&grab, c0, 1.0);
        approx_eq3(p_back, p0, 1e-4);

        // clamp 上界
        let p_max = map_sphere(&grab_far, [0.0, 1.6, -2.0], 3.0);
        let d_max = length3(sub3(p_max, pivot));
        assert!((d_max - DISTANCE_M_RANGE.1).abs() < 1e-4);
    }

    #[test]
    fn map_sphere_azimuth_follows_controller() {
        let pivot = [0.0, 1.6, 0.0];
        let c0 = [0.0, 1.6, -0.5];
        let p0 = [0.0, 1.6, -2.0];
        let grab = SphereGrab::from_poses(pivot, c0, p0);
        // 手柄转到 +X（同距离）
        let p = map_sphere(&grab, [0.5, 1.6, 0.0], 1.0);
        // 面板应大致在 +X 方向、距离仍约 2m
        assert!(p[0] > 1.5, "panel should move toward +X, got {p:?}");
        assert!(p[2].abs() < 0.5, "panel mostly on +X axis, got {p:?}");
        let d = length3(sub3(p, pivot));
        assert!((d - 2.0).abs() < 0.05);
    }

    #[test]
    fn map_follow_is_one_to_one_translation() {
        let t_attach = [0.1, -0.2, 0.3];
        let c = [1.0, 2.0, 3.0];
        approx_eq3(map_follow(c, t_attach), [1.1, 1.8, 3.3], 1e-6);
        // 不用旋转：同一 t_attach 仅随位置平移
        approx_eq3(map_follow([0.0, 0.0, 0.0], t_attach), t_attach, 1e-6);
    }

    #[test]
    fn mat3x4_inverse_rigid_roundtrip() {
        let yaw = 0.4_f32;
        let (s, c) = yaw.sin_cos();
        let m = Mat3x4([[c, 0.0, s, 1.0], [0.0, 1.0, 0.0, 2.0], [-s, 0.0, c, 3.0]]);
        let inv = mat3x4_inverse_rigid(&m);
        let p = [4.0, 5.0, 6.0];
        let back = inv.transform_point(m.transform_point(p));
        approx_eq3(back, p, 1e-5);
        let id = m.multiply(&inv);
        approx_eq3(id.translation(), [0.0, 0.0, 0.0], 1e-5);
        assert!((id.0[0][0] - 1.0).abs() < 1e-5);
        assert!((id.0[1][1] - 1.0).abs() < 1e-5);
        assert!((id.0[2][2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn hmd_offsets_from_world_matches_local_layout() {
        // identity HMD at origin：面板在局部 (0.2, -0.1, -1.5) → distance=1.5
        let hmd = Mat3x4::IDENTITY;
        let panel = [0.2, -0.1, -1.5];
        let o = hmd_offsets_from_world(&hmd, panel);
        assert!((o.horizontal_offset_m - 0.2).abs() < 1e-5);
        assert!((o.height_offset_m + 0.1).abs() < 1e-5);
        assert!((o.distance_m - 1.5).abs() < 1e-5);

        // 旋转 90° 偏航后的 HMD：局部点经 H 变换再反推应还原
        let yaw = std::f32::consts::FRAC_PI_2;
        let hmd_r = compose_world_rows([0.0, 1.6, 0.0], yaw);
        let local = [0.3, 0.1, -2.0];
        let world = hmd_r.transform_point(local);
        let o2 = hmd_offsets_from_world(&hmd_r, world);
        assert!((o2.horizontal_offset_m - 0.3).abs() < 1e-5);
        assert!((o2.height_offset_m - 0.1).abs() < 1e-5);
        assert!((o2.distance_m - 2.0).abs() < 1e-5);

        let clamped = clamp_hmd_offsets(HmdOffsets {
            horizontal_offset_m: 5.0,
            height_offset_m: -5.0,
            distance_m: 0.01,
        });
        assert_eq!(clamped.horizontal_offset_m, OFFSET_M_RANGE.1);
        assert_eq!(clamped.height_offset_m, OFFSET_M_RANGE.0);
        assert_eq!(clamped.distance_m, DISTANCE_M_RANGE.0);
    }

    #[test]
    fn controller_yaw_uses_horizontal_forward() {
        // identity：前向 -Z → yaw = atan2(0,-1) = π
        let yaw0 = controller_yaw(&Mat3x4::IDENTITY).expect("horiz");
        assert!((yaw0 - std::f32::consts::PI).abs() < 1e-5);

        // 俯仰过大：前向近竖直 → None
        // 绕 X 俯仰 ~90°：局部 -Z → 世界 -Y
        let pitched = Mat3x4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
        ]);
        // forward = -Z_col = (0, -1, 0) after... Z column is [0,1,0], forward = -Z = [0,-1,0]
        assert!(controller_yaw(&pitched).is_none());
    }

    #[test]
    fn free_session_sphere_moves_with_controller() {
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        let c0 = compose_world_rows([0.0, 1.6, -0.5], 0.0);
        let panel0 = compose_world_rows([0.0, 1.6, -2.0], 0.0);
        let mut session = FreeDragSession::start(true, &hmd, &c0, panel0);
        let c1 = compose_world_rows([0.3, 1.6, -0.5], 0.0);
        assert!(session.tick(&c1, &hmd, 1.0, "face_user", true));
        let p = session.panel.translation();
        assert!(
            p[0] > 0.05,
            "azimuth should follow controller +X, got {p:?}"
        );
        // face_user：面板朝向 pivot
        let yaw = extract_yaw(&session.panel);
        let expect = face_user_yaw(hmd.translation(), p);
        assert!((yaw - expect).abs() < 1e-4);
    }

    #[test]
    fn free_session_jump_skips_and_rebases() {
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        let c0 = compose_world_rows([0.0, 1.6, -0.5], 0.0);
        let panel0 = compose_world_rows([0.0, 1.6, -2.0], 0.0);
        let mut session = FreeDragSession::start(true, &hmd, &c0, panel0);
        let before = session.panel.translation();
        // 跳变超过 JUMP_M
        let c_jump = compose_world_rows([0.0, 1.6, -0.5 + JUMP_M + 0.05], 0.0);
        assert!(!session.tick(&c_jump, &hmd, 1.0, "keep", true));
        assert!(session.needs_rebase);
        // 不变量 6 / A12：丢帧后 panel 不变；imp 须继续 absolute(panel)，禁止回落 apply_transform。
        approx_eq3(session.panel.translation(), before, 1e-5);
        // 下一帧小幅移动：rebase 后继续
        let c2 = compose_world_rows([0.05, 1.6, -0.5 + JUMP_M + 0.05], 0.0);
        assert!(session.tick(&c2, &hmd, 1.0, "keep", true));
        assert!(!session.needs_rebase);
    }

    #[test]
    fn free_session_keep_preserves_yaw() {
        let yaw0 = 0.7f32;
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        let c0 = compose_world_rows([0.0, 1.6, -0.5], 0.0);
        let panel0 = compose_world_rows([0.0, 1.6, -2.0], yaw0);
        let mut session = FreeDragSession::start(true, &hmd, &c0, panel0);
        let c1 = compose_world_rows([0.15, 1.6, -0.45], 0.0);
        assert!(session.tick(&c1, &hmd, 1.0, "keep", true));
        assert!((extract_yaw(&session.panel) - yaw0).abs() < 1e-4);
    }

    #[test]
    fn free_session_follow_is_one_to_one() {
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        let c0 = compose_world_rows([0.2, 1.5, -0.3], 0.0);
        let panel0 = compose_world_rows([0.5, 1.4, -1.0], 0.2);
        let mut session = FreeDragSession::start(false, &hmd, &c0, panel0);
        let c1 = compose_world_rows([0.3, 1.55, -0.35], 0.0);
        assert!(session.tick(&c1, &hmd, 1.0, "face_user", true));
        let expect = map_follow(
            c1.translation(),
            sub3(panel0.translation(), c0.translation()),
        );
        approx_eq3(session.panel.translation(), expect, 1e-5);
    }

    #[test]
    fn free_session_controller_yaw_adds_twist() {
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        // 手柄初始朝 -Z（yaw=π 或 按 forward=-Z → atan2(0,-1)=π）
        let c0 = compose_world_rows([0.0, 1.6, -0.5], 0.0);
        let panel0 = compose_world_rows([0.0, 1.6, -2.0], 0.0);
        let mut session = FreeDragSession::start(true, &hmd, &c0, panel0);
        // 绕 Y 转 45°：前向水平投影方位变化
        let yaw_twist = 45.0_f32.to_radians();
        let c1 = compose_world_rows([0.0, 1.6, -0.5], yaw_twist);
        assert!(session.tick(&c1, &hmd, 1.0, "controller_yaw", true));
        let yaw = extract_yaw(&session.panel);
        let face = face_user_yaw(hmd.translation(), session.panel.translation());
        // 叠加扭转后应明显偏离纯 face_user
        assert!(
            (yaw - face).abs() > 0.2,
            "expected controller twist on top of face_user, yaw={yaw} face={face}"
        );
    }

    #[test]
    fn jump_m_constant_is_above_plane_frame_cap() {
        // 文档：free 用 JUMP_M 丢帧；plane 保留 MAX_FRAME_DELTA_M
        const {
            assert!(JUMP_M > MAX_FRAME_DELTA_M);
        }
        assert!((R_MIN - 0.15).abs() < 1e-6);
        assert!((DEPTH_GAIN - 3.0).abs() < 1e-6);
        assert!((EL_MAX - 75.0_f32.to_radians()).abs() < 1e-5);
    }

    #[test]
    fn chord_pointer_for_space_relaxes_only_while_free_dragging() {
        assert!(!chord_pointer_for_space("free", false, false));
        assert!(chord_pointer_for_space("free", false, true));
        assert!(chord_pointer_for_space("free", true, false));
        assert!(chord_pointer_for_space("free", true, true));
        // plane：始终透传真实命中（拖动中离开面板仍可松手）
        assert!(!chord_pointer_for_space("plane", true, false));
        assert!(chord_pointer_for_space("plane", true, true));
    }

    #[test]
    fn free_session_clamp_to_validate_world_and_hmd() {
        let hmd = compose_world_rows([0.0, 1.6, 0.0], 0.0);
        let c0 = compose_world_rows([0.0, 1.6, -0.5], 0.0);
        let yaw0 = 0.4_f32;
        let mut session =
            FreeDragSession::start(true, &hmd, &c0, compose_world_rows([150.0, 0.0, 0.0], yaw0));
        assert!(session.clamp_to_validate(true, None));
        approx_eq3(session.panel.translation(), [100.0, 0.0, 0.0], 1e-5);
        assert!((extract_yaw(&session.panel) - yaw0).abs() < 1e-5);

        // hmd：超大偏移钳回 OFFSET / DISTANCE 后映回世界点
        let mut session_hmd =
            FreeDragSession::start(false, &hmd, &c0, compose_world_rows([9.0, 1.6, -1.0], 0.0));
        assert!(session_hmd.clamp_to_validate(false, Some(&hmd)));
        let o = hmd_offsets_from_world(&hmd, session_hmd.panel.translation());
        assert_eq!(o.horizontal_offset_m, OFFSET_M_RANGE.1);
        assert!((o.height_offset_m - 0.0).abs() < 1e-4);
        assert!((o.distance_m - 1.0).abs() < 1e-4);
        // 缺 HMD 不改
        let before = session_hmd.panel.translation();
        assert!(!session_hmd.clamp_to_validate(false, None));
        approx_eq3(session_hmd.panel.translation(), before, 1e-5);
    }
}
