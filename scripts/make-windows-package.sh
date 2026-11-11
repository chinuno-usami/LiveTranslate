#!/usr/bin/env bash
#
# 在本机组装 Windows 产物包（交叉编译 + 附带 ONNX Runtime）
#
# 用法:
#   scripts/make-windows-package.sh [out-dir]
#
# 产出:
#   <out-dir>/livetranslate-x86_64-pc-windows-msvc/
#   <out-dir>/livetranslate-x86_64-pc-windows-msvc.zip
#
# 注意: 本脚本只产出标准版（无 SteamVR/OSC）。Full 版需要
#       `--features extras`，且 openvr-sys 需要 CMake + MSVC，
#       只能在 Windows 本机或 CI 的 Windows runner 上构建。
#
# 前置: 已安装 cargo-xwin（cargo install cargo-xwin）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="${1:-dist}"
TARGET="x86_64-pc-windows-msvc"
BIN_NAME="livetranslate"
STAGE="${OUT_DIR}/${BIN_NAME}-${TARGET}"

echo "==> 编译 Windows 目标 (${TARGET})"
if command -v cargo-xwin >/dev/null 2>&1 || cargo xwin --version >/dev/null 2>&1; then
  cargo xwin build --release --target "$TARGET"
else
  echo "未找到 cargo-xwin，请先执行: cargo install cargo-xwin" >&2
  exit 1
fi

echo "==> 准备产物目录 ${STAGE}"
rm -rf "$STAGE"
mkdir -p "$STAGE"

cp "target/${TARGET}/release/${BIN_NAME}.exe" "$STAGE/"

# 附带配置样例（默认使用用户配置目录以支持多配置档；重命名为 default.toml 可启用便携模式）
if [ -f config/default.toml ]; then
  mkdir -p "$STAGE/config"
  cp config/default.toml "$STAGE/config/config.example.toml"
fi
for f in README.md QUICKSTART.md docs/BUILD_FLAVORS.md; do
  [ -f "$f" ] && cp "$f" "$STAGE/"
done

echo "==> 下载并放入 ONNX Runtime（silero VAD 用）"
ORT_TMP="$(mktemp -d)"
trap 'rm -rf "$ORT_TMP"' EXIT

# 复用统一的下载/校验/解压逻辑（含 SHA-256 完整性校验与版本来源）
bash "$SCRIPT_DIR/fetch-onnxruntime.sh" win-x64 "$ORT_TMP/ort"
cp "$ORT_TMP/ort/onnxruntime.dll" "$STAGE/"
echo "    + onnxruntime.dll ($(du -h "$STAGE/onnxruntime.dll" | cut -f1))"

echo "==> 打包"
rm -f "${OUT_DIR}/${BIN_NAME}-${TARGET}.zip"
( cd "$OUT_DIR" && zip -q -r "${BIN_NAME}-${TARGET}.zip" "$(basename "$STAGE")" )

echo
echo "完成:"
echo "  目录: ${STAGE}"
echo "  压缩包: ${OUT_DIR}/${BIN_NAME}-${TARGET}.zip"
echo
echo "内容:"
ls -lh "$STAGE" | tail -n +2 | awk '{printf "  %-24s %s\n", $9, $5}'
