@echo off
setlocal enabledelayedexpansion

REM LiveTranslate 快速启动脚本 (Windows)

set BINARY=target\release\livetranslate.exe
set CONFIG=config\default.toml

echo ================================
echo LiveTranslate 快速启动
echo ================================

REM 检查二进制文件
if not exist "%BINARY%" (
    echo [ERROR] 二进制文件不存在: %BINARY%
    echo 正在编译...
    cargo build --release
)

echo [OK] 二进制文件: %BINARY%

REM 检查配置文件
if not exist "%CONFIG%" (
    echo [ERROR] 配置文件不存在: %CONFIG%
    echo 请创建配置文件后再运行
    exit /b 1
)

echo [OK] 配置文件: %CONFIG%

REM 列出设备
echo.
echo 可用的音频设备:
"%BINARY%" --list-devices

echo.
echo ================================
echo 启动应用... (Ctrl+C 停止)
echo ================================
echo.

REM 启动应用
"%BINARY%" --log-level info

pause
