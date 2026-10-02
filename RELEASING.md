# 发布流程 (Releasing)

本项目使用 GitHub Actions 自动构建各平台产物并发布到 GitHub Release。

## 一键发布

推送一个 `v*` 格式的 tag 即可触发：

```bash
# 确保代码已提交
git add -A
git commit -m "chore: release v0.1.0"

# 打 tag 并推送
git tag v0.1.0
git push origin master
git push origin v0.1.0
```

推送 tag 后，workflow 会自动：

1. 在三个平台并行构建 release 二进制
   - `linux-x86_64` (Ubuntu 22.04)
   - `windows-x86_64` (Windows Server)
   - `macos-universal` (Apple Silicon + Intel 通用二进制)
2. 打包产物
   - Linux → `tar.gz`
   - Windows → `zip`
   - macOS → **`LiveTranslate.app`**（ad-hoc 签名）+ `zip`
3. 用 [git-cliff](https://git-cliff.org) 按提交信息生成本版本的 Release 说明
4. 创建 GitHub Release 并上传全部产物

## Release 说明（更新日志）

Release 页面上的说明由 `cliff.toml` 根据 Conventional Commits 自动生成，无需手写：

| 提交类型 | 分组 |
|----------|------|
| `feat` | 新功能 |
| `fix` | 问题修复 |
| `perf` | 性能优化 |
| `refactor` | 重构 |
| `docs` | 文档 |
| `chore` / `ci` / `build` / `style` / `test` | 不展示 |

不符合 `type: 描述` 格式的提交不会出现在说明里，提交时请保持这个格式。
`feat(ui): ...` 这样的 scope 会以粗体前缀显示。

打 tag 前可以在本地预览下一版的说明：

```bash
# 需先安装 git-cliff（brew install git-cliff 或 cargo install git-cliff）
git cliff --unreleased --tag v0.1.7 --strip header
```

## 手动触发

在 GitHub 上进入 **Actions → Build & Release → Run workflow**，
填入发布 tag（例如 `v0.1.0`）后运行。

## 本地构建

发布构建**必须**启用 `custom-protocol` feature（本项目已将它设为默认 feature，
因此直接 `cargo build --release` 即可）：

```bash
# 本平台构建
cargo build --release

# 从 macOS/Linux 交叉编译到 Windows（需先安装 cargo-xwin）
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

### 本地打 macOS .app

```bash
cargo build --release
./scripts/make-macos-app.sh target/release/livetranslate dist macos-arm64
open dist/LiveTranslate.app
```

> **为什么需要 `custom-protocol`？**
> Tauri 在未启用该 feature 时认为是 dev 模式（从 `devPath` 加载资源）。
> 交叉编译场景下，`tauri-codegen` 会因为读不到 `TARGET` 而回退到 host 判定，
> 在 macOS 上就会错误生成 `tauri::embed_plist` 调用，导致 Windows 构建报
> `could not find embed_plist in tauri`。启用该 feature 后 dev 分支被关闭，问题消失。

## 产物命名

| 平台 | 产物 |
|------|------|
| Linux x86_64 | `livetranslate-x86_64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `livetranslate-x86_64-pc-windows-msvc.zip` |
| macOS (Universal) | `LiveTranslate-universal-apple-darwin.zip`（内含 `LiveTranslate.app`） |

内容：

- **Linux / Windows**：可执行文件 + ONNX Runtime 动态库 + `config/default.toml` + 文档
- **macOS**：`LiveTranslate.app` bundle（应用图标 + `Info.plist` + 二进制 + `Contents/Frameworks/` 下的 ONNX Runtime）

### 关于随包附带的 ONNX Runtime

`silero` VAD 后端需要一个 ONNX Runtime 动态库。CI 在打包时会自动从
[官方 release](https://github.com/microsoft/onnxruntime/releases) 下载（版本由
`scripts/fetch-onnxruntime.sh` 中的 `ORT_VERSION` 控制）并放进产物：

| 平台 | 位置 |
|------|------|
| Linux | `libonnxruntime.so*`（与二进制同级） |
| Windows | `onnxruntime.dll`（与 exe 同级） |
| macOS | `LiveTranslate.app/Contents/Frameworks/libonnxruntime.dylib` |

程序启动时会在这些位置自动发现它（详见 README 的查找顺序），因此**用户无需单独安装**。

> 注意：ONNX Runtime 官方只为 **Apple Silicon** 提供 macOS 预编译库，
> 没有 x86_64 版本。所以 Intel Mac 上不会内置该库，
> `silero` 后端会自动回退到能量 VAD（不会报错）。

## 使用发布产物

### Linux / Windows

```bash
# 解压
tar -xzf livetranslate-x86_64-unknown-linux-gnu.tar.gz
cd livetranslate-x86_64-unknown-linux-gnu

# 编辑配置（填入翻译 API Key 等）
vim config/default.toml

# 运行
./livetranslate --log-level info
```

> Windows 解压后双击 `livetranslate.exe`。

### macOS

```bash
# 解压
unzip LiveTranslate-universal-apple-darwin.zip

# 放到应用程序目录（可选）
mv LiveTranslate.app /Applications/

# 首次运行：从网络下载的 zip 会带 quarantine 属性，需移除
xattr -dr com.apple.quarantine /Applications/LiveTranslate.app

# 启动
open /Applications/LiveTranslate.app
```

> 首次运行会请求**麦克风权限**，请允许；否则采集不到声音。
>
> 首次运行还会在
> `~/Library/Application Support/com.chinuno.LiveTranslate/config.toml`
> 自动生成默认配置，填入翻译 API Key 后重启生效。
> 也可以用托盘菜单 → **打开配置目录** 直接打开该目录。

## 各平台依赖

- **Linux**：`libwebkit2gtk-4.0`、`libgtk-3`、`libasound2`、`libayatana-appindicator3`
- **Windows**：WebView2 Runtime（Win10/11 通常已预装）
- **macOS**：无需额外依赖；首次运行需授予**麦克风权限**

## CI（持续集成）

`.github/workflows/ci.yml` 会在 push / PR 时对三平台执行：

- `cargo check`
- `cargo test`
- `cargo clippy`（不阻塞）
- `cargo fmt --check`（不阻塞）

用于在打 tag 前尽早发现跨平台编译问题。

## 注意事项

- `tauri.conf.json` 已设置 `bundle.active = true` 与 `macOSPrivateApi = true`，
  以及完整多尺寸图标。
- macOS 产物由 `scripts/make-macos-app.sh` 组装为 `.app` 并做 **ad-hoc 签名**
  （`codesign --sign -`），适用于本地与 CI，不依赖 `tauri-cli`。
- 如需 `.dmg` / `.msi` / `.AppImage` 等安装器，可在安装 `tauri-cli` 后执行
  `cargo tauri build`（扁平结构下 CLI 会自动定位根目录的 `tauri.conf.json`）。
- macOS `.app` 是 **未公证（not notarized）** 的，从网络下载后首次运行：
  先 `xattr -dr com.apple.quarantine <app>`，再到“系统设置 → 隐私与安全性”放行。
