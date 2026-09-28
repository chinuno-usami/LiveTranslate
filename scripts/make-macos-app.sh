#!/usr/bin/env bash
#
# 组装 macOS .app bundle（不依赖 tauri-cli）
#
# 用法:
#   scripts/make-macos-app.sh <binary-path> <out-dir> [label]
#
# 例:
#   scripts/make-macos-app.sh target/universal/release/livetranslate dist macos-universal
#
# 产出:
#   <out-dir>/LiveTranslate.app
#   <out-dir>/LiveTranslate-<label>.zip
#
set -euo pipefail

BIN_PATH="${1:?usage: make-macos-app.sh <binary-path> <out-dir> [label]}"
OUT_DIR="${2:?usage: make-macos-app.sh <binary-path> <out-dir> [label]}"
LABEL="${3:-macos}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

APP_NAME="LiveTranslate"
EXEC_NAME="livetranslate"
BUNDLE_ID="com.chinuno.livetranslate"
APP_VERSION="$(grep -m1 '^version' "$ROOT_DIR/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"

if [ ! -f "$BIN_PATH" ]; then
  echo "binary not found: $BIN_PATH" >&2
  exit 1
fi

APP_DIR="$OUT_DIR/$APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

# 可执行文件
cp "$BIN_PATH" "$APP_DIR/Contents/MacOS/$EXEC_NAME"
chmod +x "$APP_DIR/Contents/MacOS/$EXEC_NAME"

# 图标
if [ -f "$ROOT_DIR/icons/icon.icns" ]; then
  cp "$ROOT_DIR/icons/icon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
fi

# Info.plist
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$EXEC_NAME</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>$APP_VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$APP_VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSUIElement</key>
    <false/>
    <key>NSMicrophoneUsageDescription</key>
    <string>需要访问麦克风，用于实时语音识别与翻译。</string>
</dict>
</plist>
PLIST

# ad-hoc 签名（有 codesign 时执行；未签名时 macOS 首次运行需放行）
if command -v codesign >/dev/null 2>&1; then
  codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 \
    && echo "ad-hoc signed: $APP_DIR" \
    || echo "warning: ad-hoc codesign failed (ignored)"
fi

# 打包为 zip
ZIP_PATH="$OUT_DIR/${APP_NAME}-${LABEL}.zip"
rm -f "$ZIP_PATH"
if command -v ditto >/dev/null 2>&1; then
  ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP_PATH"
else
  ( cd "$OUT_DIR" && zip -qry "$(basename "$ZIP_PATH")" "$APP_NAME.app" )
fi

echo "created app: $APP_DIR"
echo "created zip: $ZIP_PATH"
