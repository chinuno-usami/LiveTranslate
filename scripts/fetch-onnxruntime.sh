#!/usr/bin/env bash
#
# 下载 ONNX Runtime 预编译库，并把动态库抽到指定目录。
#
# 用法:
#   scripts/fetch-onnxruntime.sh <linux-x64|macos-arm64> <out-dir>
#
# 可用 ORT_VERSION 覆盖版本（默认 1.30.0，与 ort 2.x 期望的 API 兼容）。
#
# 说明: ONNX Runtime 官方只提供动态库（无静态库），
# 因此这里是「随包附带动态库」，由程序启动时自动发现。
set -euo pipefail

PLATFORM="${1:?usage: fetch-onnxruntime.sh <linux-x64|macos-arm64> <out-dir>}"
OUT_DIR="${2:?usage: fetch-onnxruntime.sh <linux-x64|macos-arm64> <out-dir>}"
ORT_VERSION="${ORT_VERSION:-1.30.0}"

case "$PLATFORM" in
  linux-x64)
    ARCHIVE="onnxruntime-linux-x64-${ORT_VERSION}.tgz"
    ;;
  macos-arm64)
    ARCHIVE="onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
    ;;
  *)
    echo "unsupported platform: $PLATFORM (可选: linux-x64 | macos-arm64)" >&2
    exit 1
    ;;
esac

URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ARCHIVE}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "downloading ${URL}"
curl -sSL --retry 3 --retry-delay 2 --max-time 600 -o "${TMP_DIR}/${ARCHIVE}" "${URL}"
tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "${TMP_DIR}"

mkdir -p "$OUT_DIR"

copied=0
for lib in "${TMP_DIR}"/*/lib/libonnxruntime*; do
  # 只要库文件本体，跳过 .dSYM 等目录
  [ -f "$lib" ] || continue
  case "$lib" in *.dSYM*) continue ;; esac
  cp -a "$lib" "${OUT_DIR}/"
  echo "  + $(basename "$lib")"
  copied=$((copied + 1))
done

if [ "$copied" -eq 0 ]; then
  echo "archive did not contain any libonnxruntime library" >&2
  exit 1
fi

# 保证存在无版本号的文件名，程序的自动发现按这个名字查找
case "$PLATFORM" in
  macos-arm64)
    if [ ! -e "${OUT_DIR}/libonnxruntime.dylib" ]; then
      first="$(ls "${OUT_DIR}"/libonnxruntime*.dylib 2>/dev/null | head -1 || true)"
      [ -n "$first" ] && cp -a "$first" "${OUT_DIR}/libonnxruntime.dylib"
    fi
    ;;
  linux-x64)
    if [ ! -e "${OUT_DIR}/libonnxruntime.so" ]; then
      first="$(ls "${OUT_DIR}"/libonnxruntime.so* 2>/dev/null | head -1 || true)"
      [ -n "$first" ] && cp -a "$first" "${OUT_DIR}/libonnxruntime.so"
    fi
    ;;
esac

echo "onnxruntime placed in ${OUT_DIR}:"
ls -lh "$OUT_DIR"
