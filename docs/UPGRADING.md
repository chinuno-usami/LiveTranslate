# 升级说明

> 本文件会被拼进 GitHub Release 说明（`docs/BUILD_FLAVORS.md` 之后、git-cliff 生成的更新日志之前）。
> 写「升级时需要人工做什么」，不写逐条改动——逐条改动由 git-cliff 生成。

## 0.3.0 → 0.3.1

### 一句话

**配置文件一般不用改**；0.3.1 新增的 `gesture.space` / `gesture.orientation` / `hmd_face_user` 都有默认值。
若旧配置**没有**写 `space`，升级后按 serde 缺省变成 **`space = "free"`**（可推拉距离），不再是 v0.3 的纯平面平移。
手势默认仍是 `enabled = false` 且 `locked = true`，未开手势则体感不变。

想继续只用平面微调：在 `[output.steamvr.gesture]` 显式写 `space = "plane"`（见下方「回滚到平面」）。

### 行为变化（可能被人注意到）

| 变化 | 说明 |
| --- | --- |
| 缺省 `space = free` | 旧 TOML 未写 `space` 时，解锁拖动可推拉距离（ray=头中心球坐标+比例深度；near=1:1 跟随） |
| 缺省 `orientation = face_user` | 仅 `space=free` 且 **world** 锚点：拖动中面板偏航正对用户；松手写回 `world_rotation_deg[1]`（归一化到 `(-180, 180]`）；`[0]`/`[2]` 原样保留、不应用 |
| `space = plane` | 与 v0.3 一致，只沿面板平面移动 |
| free 拖动中 pointer 隐藏 | 面板走 absolute，避免与基于配置的命中点错位；松手后恢复 |
| clamp 触边 haptic | 开启 `haptics` 时，**每个拖动会话**触 validate 边界至多震一次 |
| `hmd_face_user`（默认 `false`） | hmd **渲染**是否按偏移自动偏航正对眼睛；与 `gesture.orientation`（拖动期、首版仅 world）无关 |

### 回滚到平面（保留 v0.3 手感）

```toml
[output.steamvr.gesture]
space = "plane"
# 可选：不想自动改偏航时（仅 free+world 有意义）
# orientation = "keep"
```

不需要删其它字段；`orientation` / `hmd_face_user` 对 `plane` 拖动无影响。

### 可选：朝向与 hmd 正视

- `orientation = "keep"`：free+world 拖动不改偏航（接近 v0.3）。
- `orientation = "controller_yaw"`：在 face_user 上叠加手柄绕竖直轴的扭转；俯仰/滚转不影响。
- `hmd_face_user = true`：hmd 锚点且 `horizontal_offset_m ≠ 0` 时，渲染只改偏航正对眼睛，减轻斜视（默认关，与 0.3 渲染一致）。

细节与排错见 `README.md`（手势表 /「只能平面 → 查 space」）与 `QUICKSTART.md`。

### 已知限制（0.3.1）

- **未在 Windows 真机 / SteamVR 运行时验收**：A1–A12 与 Index / Quest 矩阵见 `docs/V0.4_ACCEPTANCE.md`（全部标待验收）。开发机 macOS 上的单测与 stub / `cargo xwin clippy` **不代表**真机完成。
- hmd 锚点松手反推偏移时，若 `horizontal_offset_m` 等触 `[-2, 2]` 等 validate 边界，松手可能有一次跳变（触边会 haptic 一次）。
- `world_rotation_deg[0]` / `[2]` 仍忽略且原样保留；本期只持久化偏航 `[1]`。
- 不做空手追踪 / 俯仰滚转手势 / VR 内数值 HUD；Dashboard 打开、断追、锁定、epoch 取消语义与 0.3 相同。

### 回滚软件版本

0.3.1 写回的新字段对 0.3.0 多为未知键：降回 0.3.0 一般仍可启动（设置页可能提示未识别项），手势按 0.3.0 平面逻辑；若曾以 `free` 写回较远的 `distance_m` / `world_position` / 偏航，平面模式下这些值仍生效。

## 0.2.x → 0.3.0

### 一句话

**不用改配置，直接覆盖安装即可**；0.3 新增的配置项全部有默认值，旧 `config.toml` 启动后行为与 0.2 一致。
想用新能力（按源路由、自定义快捷键、VR 手势移动）再看下面的可选步骤。

### 可选：按源路由输出

`[[audio.sources]]` 新增三个开关，默认全 `true`（等价 0.2）：

```toml
[[audio.sources]]
id = "mic"
# ...
desktop = true    # 桌面浮窗
osc = true        # OSC / VRChat Chatbox
steamvr = true    # SteamVR Overlay
```

典型用法：桌面显示双方字幕，OSC / VR 只发麦克风 → 给游戏声音那条源关掉 `osc` / `steamvr`。

注意：`enabled = false` 的源仍保留历史，所以关掉某个源的输出后，该源此前的字幕会立刻从对应通道消失，
但历史窗口（不是浮窗）依旧能看到全部源。

### 可选：自定义全局快捷键（新增 `[hotkeys]`）

0.2 硬编码的 6 个快捷键在 0.3 里成为默认值（`CmdOrCtrl+Alt+S/R/E/T/Comma/Q`），文件里不写就是这 6 个。
留空字符串即禁用该项。设置页 → **快捷键** 分组可以直接录制；录制期间所有全局快捷键（含退出）会临时暂停。

录制只支持 Tauri 能解析的键名：带修饰键的字母/数字/符号，或 F1–F24。
小键盘乘号/小数点等不支持（录制时会被拒绝），手工写在配置里则会显示「未生效」。

### 可选：SteamVR 手柄移动字幕面板（新增 `[output.steamvr.gesture]`）

默认 `enabled = false` 且 `locked = true`，不开启完全不读手柄。开启后：

- 解锁：设置页 / 浮窗 **VR 位置** 按钮 / 全局快捷键 `toggle_steamvr_lock`（默认未绑定）
- 默认 `grab = "both_grips"`：双手 grip + 右手射线命中面板才进入拖动
- 松手后位置会自动写回配置（`distance_m` / `height_offset_m` / `horizontal_offset_m` 或 `world_position`）

`block_game_input = true` 可在拖动期屏蔽游戏 grip/trigger，但**需要**
SteamVR → 设置 → 开发者 → *Enable global input from overlays (experimental)*。
失败会自动降级成「仅移动面板」，设置页底部有只读状态说明。
若提示拿不到 overlay-global priority，可试 `output.steamvr.application_type = "overlay"` 并重启应用。

首次启用 3A 时会把内嵌的 action manifest / 默认 bindings 解压到
`<配置目录>/steamvr/<版本>-<指纹>/`（默认注册为临时应用清单，退出后不残留）。
换版本或把程序换目录会重新解压，旧目录可手动删除。

### 行为变化（可能被人注意到）

| 变化 | 说明 |
| --- | --- |
| 快捷键可配置 | 原来 6 个快捷键的默认值不变；新增 4 个可选项（默认关闭） |
| 单通道单源不再显示 `[来源]` 前缀 | `label_sources = "auto"` 时按「该通道可见且运行中的源」判定；`always` 可强制显示 |
| OSC 通道变空会主动清屏 | 如果把所有源都从 OSC 关掉，Chatbox 会立刻被清空而不是残留最后一句 |
| 输入源表新增「输出」列 | 路由 / 翻译 / 识别语言改动**热更新不重启**；设备、启用、ID、展示名改动仍会重启采集 |
| SteamVR 面板拖动会写配置 | 只写位姿相关字段，会在配置文件里留下新的偏移值 |

### 回滚

0.3 写回的新字段对 0.2 只是未知键：0.2 会照常启动（设置页会提示「未识别的配置项」），
手势相关配置不生效。回滚时无需清理配置文件。

### 已知限制（0.3.0）

- SteamVR 相关能力仅 Windows Full 版；标准版保留配置字段但不生效。
- 3A 输入屏蔽的 `application_type` 默认值（`background` vs `overlay`）仍需 Windows 真机确认；
  真机上若屏蔽无效，先按上文切换到 `overlay` 复测。
- 手势不支持空手/手部追踪；SteamVR Dashboard 打开时不可拖动；面板因静音淡出后需要先出字幕。
- 快捷键在 Linux Wayland 下可能无法注册（提示「未生效」），可用托盘菜单替代。
