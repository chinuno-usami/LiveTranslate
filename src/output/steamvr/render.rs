//! 文本 → RGBA 纹理。使用系统字体 + cosmic-text 排版，支持 CJK fallback。
#![cfg_attr(not(all(feature = "steamvr", target_os = "windows")), allow(dead_code))]

use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight,
};

pub const CANVAS_WIDTH: usize = 1024;
/// OpenVR `SetOverlayRaw` 通过共享内存 IPC 传输整幅 RGBA 缓冲区，超过 ~4 MiB 时容易返回
/// `RequestFailed`；高度限制在 1024（1024×1024×4 = 4 MiB），超出时自动丢弃最旧行。
const MAX_CANVAS_HEIGHT: usize = 1024;
/// 单条字幕参与排版的字符上限（超出部分用 … 截断）
const MAX_LINE_CHARS: usize = 400;
const PADDING: f32 = 28.0;
const RADIUS: f32 = 24.0;
/// 原文行与译文行之间的间距
const SOURCE_ROW_GAP: f32 = 4.0;

pub struct RenderedFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// 解析 `#RGB` / `#RRGGBB` / `#RRGGBBAA`
pub fn parse_color(value: &str) -> [u8; 4] {
    let hex = value.trim().trim_start_matches('#');
    if !matches!(hex.len(), 3 | 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return [255, 255, 255, 255];
    }
    let expand = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0);
    match hex.len() {
        3 => {
            let r = expand(&hex[0..1].repeat(2));
            let g = expand(&hex[1..2].repeat(2));
            let b = expand(&hex[2..3].repeat(2));
            [r, g, b, 255]
        }
        6 => [
            expand(&hex[0..2]),
            expand(&hex[2..4]),
            expand(&hex[4..6]),
            255,
        ],
        8 => [
            expand(&hex[0..2]),
            expand(&hex[2..4]),
            expand(&hex[4..6]),
            expand(&hex[6..8]),
        ],
        _ => [255, 255, 255, 255],
    }
}

fn with_alpha(color: [u8; 4], alpha: f32) -> Color {
    let a = (color[3] as f32 * alpha.clamp(0.0, 1.0)).round() as u8;
    Color::rgba(color[0], color[1], color[2], a)
}

struct Block {
    source: Option<Buffer>,
    translated: Option<Buffer>,
    source_height: f32,
    translated_height: f32,
}

/// 画布装不下时，需要从最旧的多少行开始丢弃（至少保留一行）
fn rows_to_drop(heights: &[f32], gap: f32, max_height: f32) -> usize {
    let total = |from: usize| -> f32 {
        heights[from..].iter().sum::<f32>()
            + gap * (heights.len() - from).saturating_sub(1) as f32
            + PADDING * 2.0
    };
    let mut dropped = 0;
    while heights.len() - dropped > 1 && total(dropped) > max_height {
        dropped += 1;
    }
    dropped
}

impl Block {
    fn height(&self) -> f32 {
        let source = if self.source.is_some() {
            self.source_height.max(0.0) + SOURCE_ROW_GAP
        } else {
            0.0
        };
        source + self.translated_height
    }
}

/// 渲染器：缓存系统字体扫描结果，避免每帧重新扫描字体（首次扫描可能接近 1 秒）
pub struct Renderer {
    font_system: FontSystem,
    cache: SwashCache,
    loaded_font_path: String,
    family: Option<String>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
            loaded_font_path: String::new(),
            family: None,
        }
    }

    fn ensure_font(&mut self, path: &str) {
        let path = path.trim();
        if self.loaded_font_path == path {
            return;
        }
        self.loaded_font_path = path.to_string();
        self.family = resolve_family(&mut self.font_system, path);
    }

    /// 把字幕快照渲染成 RGBA 纹理（调用方负责上传到 Overlay）
    pub fn render(&mut self, lines: &[DisplayLine], config: &SteamVrConfig) -> RenderedFrame {
        self.ensure_font(&config.font_path);
        let family = self.family.clone();
        let font_system = &mut self.font_system;
        let cache = &mut self.cache;
        let max_lines = config.max_lines.clamp(1, 8);
        let selected: Vec<&DisplayLine> = lines.iter().rev().take(max_lines).rev().collect();

        let translated_size = config.font_size_px.clamp(12, 96) as f32;
        let source_size = (translated_size * 0.5).max(12.0);
        let block_gap = (translated_size * 0.5).max(10.0);
        let content_width = CANVAS_WIDTH as f32 - PADDING * 2.0;

        let background = parse_color(&config.background);
        let text_color = parse_color(&config.text_color);
        let source_color = parse_color(&config.source_color);
        let label_color = parse_color(&config.label_color);

        let mut blocks: Vec<Block> = Vec::new();
        let count = selected.len();

        for (index, line) in selected.iter().enumerate() {
            // 最新一条（数组末尾）用 line_alphas[0]
            let position = count.saturating_sub(1).saturating_sub(index);
            let alpha = config
                .line_alphas
                .get(position.min(2))
                .copied()
                .unwrap_or(0.48)
                .clamp(0.05, 1.0);

            let body = if line.translated.trim().is_empty() {
                &line.source
            } else {
                &line.translated
            };
            if body.trim().is_empty() {
                continue;
            }
            let body = clip_chars(body, MAX_LINE_CHARS);
            let source_text = clip_chars(&line.source, MAX_LINE_CHARS);

            let source_buffer = if config.show_source
                && !line.source.trim().is_empty()
                && !line.translated.trim().is_empty()
            {
                let mut buffer =
                    Buffer::new(font_system, Metrics::new(source_size, source_size * 1.35));
                buffer.set_size(Some(content_width), None);
                let attrs = build_attrs(&family, with_alpha(source_color, alpha * 0.85));
                buffer.set_text(&source_text, &attrs, Shaping::Advanced, None);
                buffer.shape_until_scroll(font_system, false);
                Some(buffer)
            } else {
                None
            };

            let mut buffer = Buffer::new(
                font_system,
                Metrics::new(translated_size, translated_size * 1.28),
            );
            buffer.set_size(Some(content_width), None);

            let body_attrs = build_attrs(&family, with_alpha(text_color, alpha));
            if let Some(label) = line.label.as_deref().filter(|l| !l.trim().is_empty()) {
                let label_attrs = build_attrs(&family, with_alpha(label_color, alpha));
                let prefix = format!("[{label}] ");
                buffer.set_rich_text(
                    [
                        (prefix.as_str(), label_attrs),
                        (body.as_str(), body_attrs.clone()),
                    ],
                    &body_attrs,
                    Shaping::Advanced,
                    None,
                );
            } else {
                buffer.set_text(&body, &body_attrs, Shaping::Advanced, None);
            }
            buffer.shape_until_scroll(font_system, false);

            let source_height = source_buffer.as_ref().map(buffer_height).unwrap_or(0.0);
            let translated_height = buffer_height(&buffer);

            blocks.push(Block {
                source: source_buffer,
                translated: Some(buffer),
                source_height,
                translated_height,
            });
        }

        // 画布高度有上限，而绘制是从最旧到最新自上而下：
        // 超限时被裁掉的是**最新**内容，正好是字幕最不该丢的。
        // 因此从最旧一行开始丢，直到装得下。
        let heights: Vec<f32> = blocks.iter().map(Block::height).collect();
        let dropped = rows_to_drop(&heights, block_gap, MAX_CANVAS_HEIGHT as f32);
        for _ in 0..dropped {
            blocks.remove(0);
        }
        let content_height = heights[dropped..].iter().sum::<f32>()
            + block_gap * (heights.len() - dropped).saturating_sub(1) as f32;
        let total_height = (content_height + PADDING * 2.0).max(PADDING * 2.0 + translated_size);
        let height = (total_height.ceil() as usize).clamp(1, MAX_CANVAS_HEIGHT);
        let mut canvas = vec![0u8; CANVAS_WIDTH * height * 4];

        // 背景面板（圆角 + 1px 内描边）
        draw_rounded_rect(
            &mut canvas,
            CANVAS_WIDTH,
            height,
            0.0,
            0.0,
            CANVAS_WIDTH as f32,
            height as f32,
            RADIUS,
            background,
        );
        draw_rounded_rect_border(
            &mut canvas,
            CANVAS_WIDTH,
            height,
            RADIUS,
            [255, 255, 255, 20],
        );

        let mut y = PADDING;
        for block in &mut blocks {
            if let Some(source) = block.source.as_mut() {
                let x = (PADDING + translated_size * 0.1) as i32;
                draw_buffer(
                    source,
                    font_system,
                    cache,
                    &mut canvas,
                    CANVAS_WIDTH,
                    height,
                    x,
                    y as i32,
                );
                y += block.source_height.max(0.0) + SOURCE_ROW_GAP;
            }
            if let Some(translated) = block.translated.as_mut() {
                draw_buffer(
                    translated,
                    font_system,
                    cache,
                    &mut canvas,
                    CANVAS_WIDTH,
                    height,
                    PADDING as i32,
                    y as i32,
                );
                y += block.translated_height;
            }
            y += block_gap;
        }

        if config.swap_red_blue {
            for pixel in canvas.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }

        RenderedFrame {
            width: CANVAS_WIDTH,
            height,
            rgba: canvas,
        }
    }
}

/// 便捷入口（测试 / 一次性渲染）；热路径请复用 [`Renderer`]
pub fn render(lines: &[DisplayLine], config: &SteamVrConfig) -> RenderedFrame {
    Renderer::new().render(lines, config)
}

fn build_attrs<'a>(family: &'a Option<String>, color: Color) -> Attrs<'a> {
    // Windows/macOS 的系统 sans-serif 已有 CJK fallback；这里不需要额外处理
    Attrs::new()
        .color(color)
        .weight(Weight::NORMAL)
        .family(match family {
            Some(name) => Family::Name(name.as_str()),
            None => Family::SansSerif,
        })
}

/// 按字符截断，超长时保留头部并加 `…`（避免把单条字幕排成几十行）
fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn buffer_height(buffer: &Buffer) -> f32 {
    let mut bottom = 0.0f32;
    for run in buffer.layout_runs() {
        bottom = bottom.max(run.line_top + run.line_height);
    }
    bottom
}

fn resolve_family(font_system: &mut FontSystem, font_path: &str) -> Option<String> {
    let path = font_path.trim();
    if path.is_empty() {
        return None;
    }
    let db = font_system.db_mut();
    let before: std::collections::HashSet<_> = db.faces().map(|face| face.id).collect();
    if db.load_font_file(path).is_err() {
        return None;
    }
    db.faces()
        .find(|face| !before.contains(&face.id))
        .and_then(|face| face.families.first())
        .map(|(name, _)| name.clone())
}

#[allow(clippy::too_many_arguments)]
fn draw_buffer(
    buffer: &mut Buffer,
    font_system: &mut FontSystem,
    cache: &mut SwashCache,
    canvas: &mut [u8],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
) {
    buffer.shape_until_scroll(font_system, false);
    let default_color = Color::rgba(255, 255, 255, 255);
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let physical_glyph = glyph.physical((0.0, run.line_y), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(default_color);
            let base_rgba = glyph_color.as_rgba();
            let span_alpha = base_rgba[3] as f32 / 255.0;
            if span_alpha <= 0.0 {
                continue;
            }
            let Some(image) = cache.get_image(font_system, physical_glyph.cache_key) else {
                continue;
            };
            let gx0 = image.placement.left;
            let gy0 = -image.placement.top;
            let gw = image.placement.width as i32;
            let gh = image.placement.height as i32;
            match image.content {
                SwashContent::Mask => {
                    let mut idx = 0;
                    for off_y in 0..gh {
                        for off_x in 0..gw {
                            let cov = image.data[idx];
                            idx += 1;
                            if cov == 0 {
                                continue;
                            }
                            let alpha = (cov as f32 * span_alpha).round() as u8;
                            blend_rect(
                                canvas,
                                width,
                                height,
                                x + physical_glyph.x + gx0 + off_x,
                                y + physical_glyph.y + gy0 + off_y,
                                1,
                                1,
                                [base_rgba[0], base_rgba[1], base_rgba[2], alpha],
                            );
                        }
                    }
                }
                SwashContent::Color => {
                    // Windows COLR 矢量彩色字形（Source::ColorOutline）由 swash::blit 产出预乘 RGBA，
                    // 而位图彩色字形（SBIX/CBDT）为直通 RGBA；blend_pixel 期望直通 RGBA，需先还原避免暗边。
                    let premultiplied = format!("{:?}", image.source).starts_with("ColorOutline");
                    let mut idx = 0;
                    for off_y in 0..gh {
                        for off_x in 0..gw {
                            let mut rgba = [
                                image.data[idx],
                                image.data[idx + 1],
                                image.data[idx + 2],
                                image.data[idx + 3],
                            ];
                            idx += 4;
                            if rgba[3] == 0 {
                                continue;
                            }
                            if premultiplied && rgba[3] < 255 {
                                rgba = unpremultiply_rgba(rgba);
                            }
                            rgba[3] = (rgba[3] as f32 * span_alpha).round() as u8;
                            blend_rect(
                                canvas,
                                width,
                                height,
                                x + physical_glyph.x + gx0 + off_x,
                                y + physical_glyph.y + gy0 + off_y,
                                1,
                                1,
                                rgba,
                            );
                        }
                    }
                }
                SwashContent::SubpixelMask => {}
            }
        }
    }
}

fn unpremultiply_rgba(rgba: [u8; 4]) -> [u8; 4] {
    let a = rgba[3];
    if a == 0 || a == 255 {
        return rgba;
    }
    let scale = 255.0 / a as f32;
    [
        (rgba[0] as f32 * scale).round().clamp(0.0, 255.0) as u8,
        (rgba[1] as f32 * scale).round().clamp(0.0, 255.0) as u8,
        (rgba[2] as f32 * scale).round().clamp(0.0, 255.0) as u8,
        a,
    ]
}

#[allow(clippy::too_many_arguments)]
fn blend_rect(
    canvas: &mut [u8],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: [u8; 4],
) {
    if color[3] == 0 {
        return;
    }
    for py in y..y + h as i32 {
        if py < 0 || py >= height as i32 {
            continue;
        }
        for px in x..x + w as i32 {
            if px < 0 || px >= width as i32 {
                continue;
            }
            let offset = (py as usize * width + px as usize) * 4;
            blend_pixel(&mut canvas[offset..offset + 4], color);
        }
    }
}

fn blend_pixel(dst: &mut [u8], src: [u8; 4]) {
    let sa = src[3] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let value = (src[channel] as f32 * sa + dst[channel] as f32 * da * (1.0 - sa)) / out_a;
        dst[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn rounded_coverage(fx: f32, fy: f32, x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    if fx < x || fy < y || fx > x + w || fy > y + h {
        return 0.0;
    }
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let cx = fx.clamp(x + radius, x + w - radius);
    let cy = fy.clamp(y + radius, y + h - radius);
    let distance = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
    (radius + 0.5 - distance).clamp(0.0, 1.0)
}

#[allow(clippy::too_many_arguments)]
fn draw_rounded_rect(
    canvas: &mut [u8],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: [u8; 4],
) {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x + w).ceil() as i32;
    let y1 = (y + h).ceil() as i32;

    for py in y0..y1 {
        for px in x0..x1 {
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            let coverage = rounded_coverage(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, radius);
            if coverage <= 0.0 {
                continue;
            }
            let offset = (py as usize * width + px as usize) * 4;
            let mut src = color;
            src[3] = (color[3] as f32 * coverage).round() as u8;
            blend_pixel(&mut canvas[offset..offset + 4], src);
        }
    }
}

fn draw_rounded_rect_border(
    canvas: &mut [u8],
    width: usize,
    height: usize,
    radius: f32,
    color: [u8; 4],
) {
    let w = width as f32;
    let h = height as f32;
    for py in 0..height {
        for px in 0..width {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            let outer = rounded_coverage(fx, fy, 0.0, 0.0, w, h, radius);
            let inner = rounded_coverage(
                fx,
                fy,
                1.0,
                1.0,
                (w - 2.0).max(0.0),
                (h - 2.0).max(0.0),
                (radius - 1.0).max(0.0),
            );
            let coverage = (outer - inner).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let offset = (py * width + px) * 4;
            let mut src = color;
            src[3] = (color[3] as f32 * coverage).round() as u8;
            blend_pixel(&mut canvas[offset..offset + 4], src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(label: Option<&str>, source: &str, translated: &str) -> DisplayLine {
        DisplayLine {
            source_id: "mic".into(),
            label: label.map(str::to_string),
            source: source.into(),
            translated: translated.into(),
            lines: vec![],
            text: String::new(),
        }
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(parse_color("#FFF"), [255, 255, 255, 255]);
        assert_eq!(parse_color("#0B0F14CC"), [11, 15, 20, 204]);
        assert_eq!(parse_color("zzz"), [255, 255, 255, 255]);
    }

    #[test]
    fn renders_canvas_with_expected_dimensions() {
        let config = SteamVrConfig::default();
        let frame = render(
            &[
                line(Some("麦克风"), "hello", "你好"),
                line(None, "world", "世界"),
            ],
            &config,
        );
        assert_eq!(frame.width, CANVAS_WIDTH);
        assert!(frame.height > 100);
        assert_eq!(frame.rgba.len(), frame.width * frame.height * 4);
        // 左上角圆角外应完全透明
        assert_eq!(frame.rgba[3], 0);
        // 面板内部有不透明像素
        assert!(frame.rgba.iter().skip(3).step_by(4).any(|a| *a > 100));
    }

    /// 超长内容时要丢最旧的行，而不是让最新一行被画布边界裁掉
    #[test]
    fn drops_oldest_rows_when_canvas_overflows() {
        let heights = [1000.0, 1000.0, 1000.0];
        assert_eq!(rows_to_drop(&heights, 16.0, 2048.0), 2);
        // 装得下时一行都不丢
        assert_eq!(rows_to_drop(&[100.0, 100.0], 16.0, 2048.0), 0);
        // 单行就超高时也必须保留一行
        assert_eq!(rows_to_drop(&[5000.0], 16.0, 2048.0), 0);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let frame = render(&[], &SteamVrConfig::default());
        assert_eq!(frame.width, CANVAS_WIDTH);
        assert!(frame.height > 0);
    }

    #[test]
    fn line_alpha_fades_older_lines() {
        let config = SteamVrConfig {
            max_lines: 3,
            show_source: false,
            ..SteamVrConfig::default()
        };
        let frame = render(
            &[
                line(None, "old", "很旧"),
                line(None, "mid", "中间"),
                line(None, "new", "最新"),
            ],
            &config,
        );
        assert!(frame.rgba.iter().any(|b| *b < 255));

        // 真正验证淡出：最新一行（画布底部）的字形峰值不透明度必须高于最旧一行（顶部）。
        // 之前这里只断言「存在 <255 的像素」，而抗锯齿像素恒满足，测试是空过的。
        let peak_alpha = |from: usize, to: usize| -> u8 {
            frame.rgba[from * CANVAS_WIDTH * 4..to * CANVAS_WIDTH * 4]
                .chunks_exact(4)
                .map(|pixel| pixel[3])
                .max()
                .unwrap_or(0)
        };
        let third = frame.height / 3;
        let newest = peak_alpha(frame.height - third, frame.height);
        let oldest = peak_alpha(0, third);
        assert!(
            newest > oldest,
            "最新一行({newest})应比最旧一行({oldest})更不透明"
        );
    }

    #[test]
    fn swap_red_blue_is_applied() {
        let mut config = SteamVrConfig {
            background: "#FF0000FF".to_string(),
            ..SteamVrConfig::default()
        };
        let normal = render(&[line(None, "", "测试")], &config);
        config.swap_red_blue = true;
        let swapped = render(&[line(None, "", "测试")], &config);
        let normal_pixel = normal.rgba.chunks_exact(4).find(|p| p[3] == 255).unwrap();
        let swapped_pixel = swapped.rgba.chunks_exact(4).find(|p| p[3] == 255).unwrap();
        assert_eq!(normal_pixel[0], 255);
        assert_eq!(swapped_pixel[2], 255);
    }

    #[test]
    fn text_color_alpha_is_respected() {
        let config = SteamVrConfig {
            background: "#00000000".to_string(),
            text_color: "#FFFFFF80".to_string(),
            show_source: false,
            ..SteamVrConfig::default()
        };
        let frame = render(&[line(None, "", "半透明测试")], &config);
        // 去除 1px 描边（alpha <= 20），字形峰值 alpha 应受 #FFFFFF80 约束（约 128）
        let max_alpha = frame.rgba.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        assert!(
            (40..=140).contains(&max_alpha),
            "文字透明度未生效，max_alpha={max_alpha}"
        );
    }

    #[test]
    fn unpremultiply_restores_straight_rgb_without_darkening_edges() {
        // 半透明纯白（alpha=128，预乘后 RGB=128）还原后应回到 ~255，再经 blend_pixel 混合不发灰
        let straight = unpremultiply_rgba([128, 128, 128, 128]);
        assert_eq!(straight, [255, 255, 255, 128]);
        let mut dst = [0u8, 0, 0, 0];
        blend_pixel(&mut dst, straight);
        assert_eq!(dst, [255, 255, 255, 128]);
    }
}
