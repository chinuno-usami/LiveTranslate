#!/usr/bin/env bash
#
# 下载 ONNX Runtime 预编译库，并把动态库抽到指定目录。
#
# 用法:
#   scripts/fetch-onnxruntime.sh <linux-x64|macos-arm64|win-x64> <out-dir>
#
# 可用 ORT_VERSION 覆盖版本（默认 1.30.0，与 ort 2.x 期望的 API 兼容）。
# 脚本内置了各平台/版本的 SHA-256，下载后会校验完整性；
# 也可用 ORT_SHA256 显式指定期望值（覆盖内置表）。
#
# 说明: ONNX Runtime 官方只提供动态库（无静态库），
# 因此这里是「随包附带动态库」，由程序启动时自动发现。
set -euo pipefail

PLATFORM="${1:?usage: fetch-onnxruntime.sh <linux-x64|macos-arm64|win-x64> <out-dir>}"
OUT_DIR="${2:?usage: fetch-onnxruntime.sh <linux-x64|macos-arm64|win-x64> <out-dir>}"
ORT_VERSION="${ORT_VERSION:-1.30.0}"

case "$PLATFORM" in
  linux-x64)
    ARCHIVE="onnxruntime-linux-x64-${ORT_VERSION}.tgz"
    ;;
  macos-arm64)
    ARCHIVE="onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
    ;;
  win-x64)
    ARCHIVE="onnxruntime-win-x64-${ORT_VERSION}.zip"
    ;;
  *)
    echo "unsupported platform: $PLATFORM (可选: linux-x64 | macos-arm64 | win-x64)" >&2
    exit 1
    ;;
esac

# 内置校验和表（版本/平台 -> SHA-256）；未知组合需用 ORT_SHA256 提供
case "${PLATFORM}/${ORT_VERSION}" in
  linux-x64/1.30.0)   DEFAULT_SHA256="a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd" ;;
  macos-arm64/1.30.0) DEFAULT_SHA256="6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012" ;;
  win-x64/1.30.0)     DEFAULT_SHA256="c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949" ;;
  *)                  DEFAULT_SHA256="" ;;
esac
EXPECTED_SHA256="${ORT_SHA256:-$DEFAULT_SHA256}"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    return 1
  fi
}

extract_archive() {
  local archive="$1" dest="$2"
  case "$archive" in
    *.zip)
      if command -v unzip >/dev/null 2>&1; then
        unzip -q -o "$archive" -d "$dest"
      elif command -v python3 >/dev/null 2>&1; then
        python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" \
          "$archive" "$dest"
      else
        echo "需要 unzip 或 python3 才能解压 ${archive}" >&2
        exit 1
      fi
      ;;
    *)
      tar -xzf "$archive" -C "$dest"
      ;;
  esac
}

URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ARCHIVE}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "downloading ${URL}"
curl -sSL --retry 3 --retry-delay 2 --max-time 600 -o "${TMP_DIR}/${ARCHIVE}" "${URL}"

# 完整性校验：防止上游资产被替换 / 下载被篡改
if [ -n "$EXPECTED_SHA256" ]; then
  if ! actual="$(sha256_of "${TMP_DIR}/${ARCHIVE}")"; then
    echo "找不到可用的 sha256 工具（sha256sum / shasum / openssl），无法校验下载" >&2
    exit 1
  fi
  if [ "$actual" != "$EXPECTED_SHA256" ]; then
    echo "SHA-256 校验失败: ${ARCHIVE}" >&2
    echo "  expected: ${EXPECTED_SHA256}" >&2
    echo "  actual:   ${actual}" >&2
    exit 1
  fi
  echo "  checksum ok"
else
  echo "warning: 未提供 ${PLATFORM}/${ORT_VERSION} 的 SHA-256，跳过完整性校验" >&2
fi

mkdir -p "$TMP_DIR/extract"
extract_archive "${TMP_DIR}/${ARCHIVE}" "$TMP_DIR/extract"
mkdir -p "$OUT_DIR"

if [ "$PLATFORM" = "win-x64" ]; then
  dll="$(find "$TMP_DIR/extract" -type f -name 'onnxruntime.dll' | head -1 || true)"
  if [ -z "$dll" ]; then
    echo "archive did not contain onnxruntime.dll" >&2
    exit 1
  fi
  cp -a "$dll" "${OUT_DIR}/"
  echo "  + $(basename "$dll")"
else
  copied=0
  for lib in "${TMP_DIR}"/extract/*/lib/libonnxruntime*; do
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
fi

echo "onnxruntime placed in ${OUT_DIR}:"
ls -lh "$OUT_DIR"
