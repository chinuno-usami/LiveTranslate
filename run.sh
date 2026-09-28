#!/bin/bash

# LiveTranslate 快速启动脚本

set -e

BINARY="./target/release/livetranslate"
CONFIG="config/default.toml"

echo "================================"
echo "LiveTranslate 快速启动"
echo "================================"

# 检查二进制文件
if [ ! -f "$BINARY" ]; then
    echo "❌ 二进制文件不存在: $BINARY"
    echo "正在编译..."
    cargo build --release
fi

echo "✅ 二进制文件: $BINARY"

# 检查配置文件
if [ ! -f "$CONFIG" ]; then
    echo "❌ 配置文件不存在: $CONFIG"
    echo "请创建配置文件后再运行"
    exit 1
fi

echo "✅ 配置文件: $CONFIG"

# 列出设备
echo ""
echo "📋 可用的音频设备:"
$BINARY --list-devices

echo ""
echo "================================"
echo "启动应用... (Ctrl+C 停止)"
echo "================================"
echo ""

# 启动应用
$BINARY --log-level info
