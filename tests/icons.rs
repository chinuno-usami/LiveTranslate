//! 图标资源校验测试
//!
//! Windows 构建时 `tauri::generate_context!` 会解析 `icons/icon.ico`，
//! 若该文件缺失/为空/格式非法，只会在 Windows 上暴露错误。
//! 这里做跨平台校验，让 CI 在任意平台都能提前发现问题。

use std::fs::File;
use std::io::BufReader;

fn manifest_path(rel: &str) -> String {
    format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel)
}

fn read_png_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((w, h))
}

#[test]
fn ico_is_valid() {
    let path = manifest_path("icons/icon.ico");
    let file =
        File::open(&path).unwrap_or_else(|e| panic!("无法打开 {path}: {e}（Windows 构建必需）"));
    let dir = ico::IconDir::read(BufReader::new(file))
        .unwrap_or_else(|e| panic!("{path} 不是合法的 ICO: {e}"));

    assert!(
        !dir.entries().is_empty(),
        "icons/icon.ico 必须包含至少一张图像"
    );

    let mut has_large = false;
    for entry in dir.entries() {
        let (w, h) = (entry.width(), entry.height());
        assert!(w > 0 && h > 0, "ICO 条目尺寸为 0");

        let img = entry
            .decode()
            .unwrap_or_else(|e| panic!("ICO 条目 {w}x{h} 无法解码: {e}"));
        assert_eq!(img.width(), w, "ICO 条目宽度不一致");
        assert_eq!(img.height(), h, "ICO 条目高度不一致");
        assert!(!img.rgba_data().is_empty(), "ICO 条目像素数据为空");

        if w >= 256 {
            has_large = true;
        }
    }

    assert!(has_large, "icons/icon.ico 应包含 256x256 图像");
}

#[test]
fn png_icons_are_valid() {
    for (name, expected) in [
        ("icons/32x32.png", 32u32),
        ("icons/128x128.png", 128),
        ("icons/128x128@2x.png", 256),
        ("icons/icon.png", 256),
    ] {
        let path = manifest_path(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("无法读取 {name}: {e}"));
        let (w, h) = read_png_dims(&bytes).unwrap_or_else(|| panic!("{name} 不是合法的 PNG"));
        assert_eq!((w, h), (expected, expected), "{name} 尺寸不符合预期");
    }
}

#[test]
fn icns_is_valid() {
    let path = manifest_path("icons/icon.icns");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("无法读取 icons/icon.icns: {e}"));

    assert!(bytes.len() > 8, "icons/icon.icns 过小");
    assert_eq!(&bytes[..4], b"icns", "ICNS magic 不正确");

    let total = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(total, bytes.len(), "ICNS 声明大小与文件大小不一致");

    let mut pos = 8;
    let mut chunks = 0;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        assert!(len >= 8, "ICNS chunk 长度非法");
        assert!(pos + len <= bytes.len(), "ICNS chunk 越界");
        chunks += 1;
        pos += len;
    }

    assert_eq!(pos, bytes.len(), "ICNS chunk 未正好填满文件");
    assert!(chunks >= 1, "ICNS 必须包含至少一张图像");
}
