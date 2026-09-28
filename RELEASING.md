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
2. 打包为 `tar.gz` / `zip`
3. 创建 GitHub Release 并上传全部产物

## 手动触发

在 GitHub 上进入 **Actions → Build & Release → Run workflow**，
填入发布 tag（例如 `v0.1.0`）后运行。

## 产物命名

| 平台 | 产物 |
|------|------|
| Linux x86_64 | `livetranslate-x86_64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `livetranslate-x86_64-pc-windows-msvc.zip` |
| macOS (Universal) | `livetranslate-universal-apple-darwin.tar.gz` |

每个压缩包内包含：

- 可执行文件（`livetranslate` / `livetranslate.exe`）
- `config/default.toml`（配置示例）
- `README.md` / `README_CN.md` / `QUICKSTART.md`

## 使用发布产物

```bash
# 解压
tar -xzf livetranslate-x86_64-unknown-linux-gnu.tar.gz
cd livetranslate-x86_64-unknown-linux-gnu

# 编辑配置（填入翻译 API Key 等）
vim config/default.toml

# 运行
./livetranslate --log-level info
```

> Windows 双击 `livetranslate.exe` 或运行 `run.bat`（如已包含）。

## 各平台依赖

发布产物是**单个可执行文件**，但运行环境需要：

- **Linux**：`libwebkit2gtk-4.0`、`libgtk-3`、`libasound2`、`libayatana-appindicator3`
- **Windows**：WebView2 Runtime（Win10/11 通常已预装）
- **macOS**：无需额外依赖

## CI（持续集成）

`.github/workflows/ci.yml` 会在 push / PR 时对三平台执行：

- `cargo check`
- `cargo test`
- `cargo clippy`（不阻塞）
- `cargo fmt --check`（不阻塞）

用于在打 tag 前尽早发现跨平台编译问题。

## 注意事项

- 当前 bundle 配置为 `bundle.active = false`，因此产出的是**可执行文件压缩包**，
  而不是 `.dmg` / `.msi` / `.AppImage` 安装包。
- 如需安装包，需先在 `tauri.conf.json` 启用 `bundle.active`，并提供完整的
  多尺寸图标（`.ico` / `.icns` / 各尺寸 `.png`），随后可改用
  `tauri-apps/tauri-action` 生成安装器。
- macOS 产物是**未签名**的，首次运行可能需要在“系统设置 → 隐私与安全性”中放行，
  或执行 `xattr -dr com.apple.quarantine livetranslate`。
