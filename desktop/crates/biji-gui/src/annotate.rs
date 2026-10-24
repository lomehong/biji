//! 图注标注画布（分析文档 P2：原 NW.js `markup.min.js` 的复刻）。
//!
//! 标注以**图片像素坐标**存储（窗口缩放无关），两套渲染器共享同一模型：
//! - `paint`：egui painter 实时叠加（编辑态）；
//! - `rasterize`：软件栅格化到像素缓冲（保存态），经 PNG 编码后由
//!   `NoteService::update_resource` 写回资源（原 `updateResource` 语义）。
//!
//! 文字栅格化用 `ab_glyph` 加载与界面同源的系统中文字体。

use std::collections::HashMap;
use std::sync::OnceLock;

use ab_glyph::{Font, FontArc, FontVec, PxScale, ScaleFont};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use image::RgbaImage;

// ---------- 标注模型 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Arrow,
    Rect,
    Text,
    /// 自由画笔：折线（拖拽轨迹采样）。
    Pen,
    /// 选择/移动已有标注（不产生新标注）。
    Select,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub tool: Tool,
    /// RGB。
    pub color: [u8; 3],
    /// 线宽（图片像素）。
    pub stroke: f32,
    /// 起点（图片像素）。
    pub start: [f32; 2],
    /// 终点（图片像素）；Text 工具下为文本左上角。
    pub end: [f32; 2],
    /// Text 工具的文本内容。
    pub text: Option<String>,
    /// Text 工具的字号（图片像素）。
    pub font_px: f32,
    /// Pen 折线中间点（图片像素）；其他工具为空。
    pub mids: Vec<[f32; 2]>,
    /// 旋转角（度，绕标注自身包围盒中心；paint/rasterize 应用该变换）。
    pub angle: f32,
}

impl Annotation {
    fn color32(&self) -> Color32 {
        Color32::from_rgb(self.color[0], self.color[1], self.color[2])
    }

    /// 编辑态：叠加到 egui 画布。`origin` 为图片显示区左上角，`scale` 为
    /// 图片像素 → 屏幕像素的缩放。
    pub fn paint(&self, painter: &egui::Painter, origin: Pos2, scale: f32) {
        let pt = |p: [f32; 2]| img_to_screen(origin, scale, p);
        let stroke = Stroke::new((self.stroke * scale).max(1.0), self.color32());
        let (r_start, r_end, r_mids) = self.rotated_points();
        let rpt = |p: [f32; 2]| img_to_screen(origin, scale, p);
        match self.tool {
            Tool::Arrow => {
                let a = pt(r_start);
                let b = pt(r_end);
                painter.arrow(a, b - a, stroke);
            }
            Tool::Rect => {
                // 旋转支持：四角折线描边
                let c0 = pt(r_start);
                let c2 = pt(r_end);
                let c1 = Pos2::new(c2.x, c0.y);
                let c3 = Pos2::new(c0.x, c2.y);
                let mut prev = c0;
                for next in [c1, c2, c3, c0] {
                    painter.line_segment([prev, next], stroke);
                    prev = next;
                }
            }
            Tool::Text => {
                // 限制：egui 0.29 TextFormat 不支持字形旋转，paint 侧仅锚点
                // 随 angle 旋转、字形保持水平；权威旋转渲染在 rasterize 保存路径。
                let text = self.text.clone().unwrap_or_default();
                let size = (self.font_px * scale).clamp(10.0, 96.0);
                painter.text(
                    rpt(r_start),
                    egui::Align2::LEFT_TOP,
                    text,
                    FontId::proportional(size),
                    self.color32(),
                );
            }
            Tool::Pen => {
                let mut prev = pt(r_start);
                for p in r_mids.iter().copied().chain(std::iter::once(r_end)) {
                    let cur = pt(p);
                    painter.line_segment([prev, cur], stroke);
                    prev = cur;
                }
            }
            Tool::Select => {}
        }
    }

    /// 保存态：软件栅格化到像素缓冲。
    pub fn rasterize(&self, img: &mut RgbaImage) {
        let color = self.color;
        let (r_start, r_end, r_mids) = self.rotated_points();
        match self.tool {
            Tool::Arrow => {
                draw_line(img, r_start, r_end, self.stroke, color);
                let head = arrow_head(r_start, r_end, (self.stroke * 4.0).max(10.0));
                draw_line(img, r_end, head.0, self.stroke, color);
                draw_line(img, r_end, head.1, self.stroke, color);
            }
            Tool::Rect => {
                let c0 = r_start;
                let c2 = r_end;
                let c1 = [c2[0], c0[1]];
                let c3 = [c0[0], c2[1]];
                draw_line(img, c0, c1, self.stroke, color);
                draw_line(img, c1, c2, self.stroke, color);
                draw_line(img, c2, c3, self.stroke, color);
                draw_line(img, c3, c0, self.stroke, color);
            }
            Tool::Text => {
                draw_text(
                    img,
                    r_start,
                    &self.text.clone().unwrap_or_default(),
                    color,
                    self.font_px,
                    self.angle,
                );
            }
            Tool::Pen => {
                let mut prev = r_start;
                for p in r_mids.iter().copied().chain(std::iter::once(r_end)) {
                    draw_line(img, prev, p, self.stroke, color);
                    prev = p;
                }
            }
            Tool::Select => {}
        }
    }
}

impl Annotation {
    /// 平移标注全部坐标点（选择/移动用）。
    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.start[0] += dx;
        self.start[1] += dy;
        self.end[0] += dx;
        self.end[1] += dy;
        for p in &mut self.mids {
            p[0] += dx;
            p[1] += dy;
        }
    }

    /// 包围盒（min, max），含线宽外扩。
    pub fn bbox(&self) -> ([f32; 2], [f32; 2]) {
        match self.tool {
            Tool::Text => {
                let w = self
                    .text
                    .as_deref()
                    .map(|t| t.chars().count() as f32 * self.font_px * 0.62)
                    .unwrap_or(self.font_px)
                    .max(self.font_px * 1.2);
                let h = self.font_px.max(14.0);
                (
                    [self.start[0] - self.stroke, self.start[1] - self.stroke],
                    [
                        self.start[0] + w + self.stroke,
                        self.start[1] + h + self.stroke,
                    ],
                )
            }
            _ => {
                let mut min = self.start;
                let mut max = self.end;
                for p in &self.mids {
                    min[0] = min[0].min(p[0]);
                    min[1] = min[1].min(p[1]);
                    max[0] = max[0].max(p[0]);
                    max[1] = max[1].max(p[1]);
                }
                if min[0] > max[0] {
                    std::mem::swap(&mut min[0], &mut max[0]);
                }
                if min[1] > max[1] {
                    std::mem::swap(&mut min[1], &mut max[1]);
                }
                let pad = self.stroke;
                ([min[0] - pad, min[1] - pad], [max[0] + pad, max[1] + pad])
            }
        }
    }

    /// 命中测试：点是否落在包围盒内。
    pub fn hit(&self, p: [f32; 2]) -> bool {
        let (min, max) = self.bbox();
        p[0] >= min[0] && p[0] <= max[0] && p[1] >= min[1] && p[1] <= max[1]
    }

    /// 绕外部点（如画布中心）的轨道位移：坐标经 rotate_point 变换，
    /// 自身 angle 刚体跟随（整体绕转，不改变标注内部几何）。
    pub fn orbit_around(&mut self, center: [f32; 2], delta_deg: f32) {
        self.start = rotate_point(self.start, center, delta_deg);
        self.end = rotate_point(self.end, center, delta_deg);
        for p in &mut self.mids {
            *p = rotate_point(*p, center, delta_deg);
        }
        self.angle = normalize_angle(self.angle + delta_deg);
    }

    /// 以 `anchor` 为锚点按 `factor` 缩放，返回缩放后的副本。
    pub fn scaled_about(&self, anchor: [f32; 2], factor: f32) -> Annotation {
        let mut a = self.clone();
        scale_point(&mut a.start, anchor, factor);
        scale_point(&mut a.end, anchor, factor);
        for p in &mut a.mids {
            scale_point(p, anchor, factor);
        }
        a.stroke = (a.stroke * factor).max(1.0);
        if a.font_px > 0.0 {
            a.font_px = (a.font_px * factor).max(8.0);
        }
        a
    }

    /// 渲染用：绕标注自身包围盒中心应用 angle 后的坐标。
    fn rotation_center(&self) -> [f32; 2] {
        let (mn, mx) = self.bbox();
        [(mn[0] + mx[0]) / 2.0, (mn[1] + mx[1]) / 2.0]
    }

    fn rotated_points(&self) -> ([f32; 2], [f32; 2], Vec<[f32; 2]>) {
        if self.angle == 0.0 {
            return (self.start, self.end, self.mids.clone());
        }
        let c = self.rotation_center();
        (
            rotate_point(self.start, c, self.angle),
            rotate_point(self.end, c, self.angle),
            self.mids
                .iter()
                .map(|p| rotate_point(*p, c, self.angle))
                .collect(),
        )
    }

    /// 单轴缩放：仅 x（Axis::X）或 y（Axis::Y）绕锚点缩放。
    pub fn scaled_about_axis(&self, anchor: [f32; 2], fx: f32, fy: f32) -> Annotation {
        let mut a = self.clone();
        a.start[0] = anchor[0] + (a.start[0] - anchor[0]) * fx;
        a.start[1] = anchor[1] + (a.start[1] - anchor[1]) * fy;
        a.end[0] = anchor[0] + (a.end[0] - anchor[0]) * fx;
        a.end[1] = anchor[1] + (a.end[1] - anchor[1]) * fy;
        for p in &mut a.mids {
            p[0] = anchor[0] + (p[0] - anchor[0]) * fx;
            p[1] = anchor[1] + (p[1] - anchor[1]) * fy;
        }
        a
    }
}

/// 箭头头部两翼端点（与线方向成 ±30°）。
fn arrow_head(start: [f32; 2], end: [f32; 2], len: f32) -> ([f32; 2], [f32; 2]) {
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    let ang = dy.atan2(dx);
    let a1 = ang + std::f32::consts::PI * 0.85;
    let a2 = ang - std::f32::consts::PI * 0.85;
    (
        [end[0] + len * a1.cos(), end[1] + len * a1.sin()],
        [end[0] + len * a2.cos(), end[1] + len * a2.sin()],
    )
}

// ---------- 坐标换算 ----------

pub fn img_to_screen(origin: Pos2, scale: f32, p: [f32; 2]) -> Pos2 {
    Pos2::new(origin.x + p[0] * scale, origin.y + p[1] * scale)
}

pub fn screen_to_img(origin: Pos2, scale: f32, p: Pos2) -> [f32; 2] {
    [(p.x - origin.x) / scale, (p.y - origin.y) / scale]
}

/// 依据可用空间与图片尺寸计算显示区域（保持纵横比）。
pub fn display_rect(origin: Pos2, avail: Vec2, img_w: f32, img_h: f32) -> (Rect, f32) {
    let scale = (avail.x / img_w).min(avail.y / img_h).clamp(0.05, 4.0);
    let size = Vec2::new(img_w * scale, img_h * scale);
    (Rect::from_min_size(origin, size), scale)
}

/// 框选命中：标注包围盒与框选矩形相交即选中。
pub fn select_by_rubber(
    annotations: &[Annotation],
    r_start: [f32; 2],
    r_end: [f32; 2],
) -> Vec<usize> {
    let mut min = [r_start[0].min(r_end[0]), r_start[1].min(r_end[1])];
    let mut max = [r_start[0].max(r_end[0]), r_start[1].max(r_end[1])];
    let mut out = Vec::new();
    for (i, a) in annotations.iter().enumerate() {
        let (mn, mx) = a.bbox();
        let overlap = mn[0] <= max[0] && mx[0] >= min[0] && mn[1] <= max[1] && mx[1] >= min[1];
        if overlap {
            out.push(i);
        }
    }
    let _ = (&mut min, &mut max);
    out
}

// ---------- 旋转（标注把手 + 模型变换） ----------

/// 绕 center 旋转 p 点 angle_deg 度（顺时针为正，屏幕坐标系）。
pub fn rotate_point(p: [f32; 2], center: [f32; 2], angle_deg: f32) -> [f32; 2] {
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let (dx, dy) = (p[0] - center[0], p[1] - center[1]);
    [
        center[0] + dx * cos - dy * sin,
        center[1] + dx * sin + dy * cos,
    ]
}

/// center → p 的方位角（度，-180..=180，0° 指向 +x）。
pub fn angle_of(center: [f32; 2], p: [f32; 2]) -> f32 {
    (p[1] - center[1]).atan2(p[0] - center[0]).to_degrees()
}

/// 角度归一到 -180..=180。
pub fn normalize_angle(deg: f32) -> f32 {
    let mut d = deg % 360.0;
    if d > 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// 轨道位移的角增量：指针绕 center 从 last 转到 now 的角度差（归一化）。
pub fn orbit_delta(center: [f32; 2], last: [f32; 2], now: [f32; 2]) -> f32 {
    normalize_angle(angle_of(center, now) - angle_of(center, last))
}

/// 单轴缩放系数（边中点把手）：锚点与起止点在该轴上的投影比；
/// 位移过小返回 None。
pub fn drag_axis_factor_1d(anchor_1d: f32, start_1d: f32, now_1d: f32) -> Option<f32> {
    let base = start_1d - anchor_1d;
    if base.abs() <= 1.0 {
        return None;
    }
    Some(((now_1d - anchor_1d) / base).clamp(0.05, 20.0))
}

// ---------- 控制点把手（角点锚定缩放） ----------

/// 包围盒 4 个角点把手：0 左上 / 1 右上 / 2 右下 / 3 左下。
pub fn handle_positions(min: [f32; 2], max: [f32; 2]) -> [[f32; 2]; 4] {
    [
        [min[0], min[1]],
        [max[0], min[1]],
        [max[0], max[1]],
        [min[0], max[1]],
    ]
}

/// 4 个边中点把手：0 上 / 1 右 / 2 下 / 3 左（单轴缩放）。
pub fn edge_mid_positions(min: [f32; 2], max: [f32; 2]) -> [[f32; 2]; 4] {
    let (cx, cy) = ((min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0);
    [[cx, min[1]], [max[0], cy], [cx, max[1]], [min[0], cy]]
}

/// 旋转把手位置：顶边中点向外偏移 `offset` 像素。
pub fn rotate_handle_position(min: [f32; 2], max: [f32; 2], offset: f32) -> [f32; 2] {
    let mids = edge_mid_positions(min, max);
    [mids[0][0], mids[0][1] - offset]
}

/// 把手命中：屏幕点到某角点距离 ≤ tol。返回角点下标。
pub fn handle_hit(p: Pos2, corners: &[Pos2], tol: f32) -> Option<usize> {
    corners
        .iter()
        .position(|c| (c.x - p.x).powi(2) + (c.y - p.y).powi(2) <= tol * tol)
}

/// 围绕锚点缩放单点。
pub fn scale_point(p: &mut [f32; 2], anchor: [f32; 2], factor: f32) {
    p[0] = anchor[0] + (p[0] - anchor[0]) * factor;
    p[1] = anchor[1] + (p[1] - anchor[1]) * factor;
}

/// 拖拽缩放系数：由把手（锚点的对角）拖拽前后位置推算，
/// 按轴可用性归一；位移过小或退化返回 None（本帧不缩放）。
pub fn drag_scale_factor(anchor: [f32; 2], start: [f32; 2], now: [f32; 2]) -> Option<f32> {
    let sx = start[0] - anchor[0];
    let sy = start[1] - anchor[1];
    let nx = now[0] - anchor[0];
    let ny = now[1] - anchor[1];
    let fx = if sx.abs() > 1.0 { Some(nx / sx) } else { None };
    let fy = if sy.abs() > 1.0 { Some(ny / sy) } else { None };
    match (fx, fy) {
        (Some(a), Some(b)) => Some(((a + b) / 2.0).clamp(0.05, 20.0)),
        (Some(a), None) => Some(a.clamp(0.05, 20.0)),
        (None, Some(b)) => Some(b.clamp(0.05, 20.0)),
        (None, None) => None,
    }
}

// ---------- 软件栅格化 ----------

fn blend_pixel(img: &mut RgbaImage, x: i64, y: i64, c: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return;
    }
    let a = alpha.clamp(0.0, 1.0);
    let px = img.get_pixel_mut(x as u32, y as u32);
    px.0[0] = (c[0] as f32 * a + px.0[0] as f32 * (1.0 - a)) as u8;
    px.0[1] = (c[1] as f32 * a + px.0[1] as f32 * (1.0 - a)) as u8;
    px.0[2] = (c[2] as f32 * a + px.0[2] as f32 * (1.0 - a)) as u8;
    px.0[3] = 255;
}

fn fill_disc(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, c: [u8; 3]) {
    let r = r.max(0.5);
    let steps = (r * 2.0).ceil() as i64;
    for dy in -steps..=steps {
        for dx in -steps..=steps {
            let (fx, fy) = (dx as f32, dy as f32);
            if fx * fx + fy * fy <= r * r {
                blend_pixel(img, cx as i64 + dx, cy as i64 + dy, c, 1.0);
            }
        }
    }
}

fn draw_line(img: &mut RgbaImage, a: [f32; 2], b: [f32; 2], stroke: f32, c: [u8; 3]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let steps = (len * 2.0).ceil() as i64;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        fill_disc(img, a[0] + dx * t, a[1] + dy * t, stroke / 2.0, c);
    }
}

static CJK_FONT: OnceLock<Option<FontArc>> = OnceLock::new();

/// 与 `install_cjk_fonts` 同源的平台候选列表；供文字栅格化复用。
/// TTC 集合经 index 加载（ab_glyph `try_from_vec_and_index`）。
pub fn load_cjk_font() -> Option<&'static FontArc> {
    CJK_FONT
        .get_or_init(|| {
            #[cfg(target_os = "windows")]
            const CANDIDATES: &[(&str, u32)] = &[
                ("C:\\Windows\\Fonts\\simhei.ttf", 0),
                ("C:\\Windows\\Fonts\\msyh.ttc", 0),
                ("C:\\Windows\\Fonts\\simsun.ttc", 0),
                ("C:\\Windows\\Fonts\\Deng.ttf", 0),
            ];
            #[cfg(target_os = "macos")]
            const CANDIDATES: &[(&str, u32)] = &[
                ("/System/Library/Fonts/PingFang.ttc", 0),
                ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
                ("/Library/Fonts/Arial Unicode.ttf", 0),
                ("/System/Library/Fonts/STHeiti Light.ttc", 0),
            ];
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            const CANDIDATES: &[(&str, u32)] = &[
                ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
                ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ];
            for (path, index) in CANDIDATES {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(f) = FontVec::try_from_vec_and_index(bytes, *index) {
                        return Some(FontArc::new(f));
                    }
                }
            }
            None
        })
        .as_ref()
}

fn draw_text(
    img: &mut RgbaImage,
    pos: [f32; 2],
    text: &str,
    c: [u8; 3],
    font_px: f32,
    angle_deg: f32,
) {
    let Some(font) = load_cjk_font() else {
        return;
    };
    let scaled = font.into_scaled(PxScale::from(font_px));
    let ascent = scaled.ascent();
    let line_height = font_px * 1.35;
    // 基线方向随 angle 旋转；每个字形位图像素再绕文字锚点旋转同角度
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let mut pen = pos;
    for (li, line) in text.split('\n').enumerate() {
        let mut x = pen[0];
        let mut y = pen[1] + ascent + line_height * li as f32;
        for ch in line.chars() {
            let mut glyph = scaled.scaled_glyph(ch);
            glyph.position = ab_glyph::Point { x, y };
            let advance = scaled.h_advance(glyph.id);
            if let Some(outlined) = scaled.outline_glyph(glyph) {
                // ab_glyph draw 回调坐标相对 px_bounds().min，需加回偏移
                let bmin = outlined.px_bounds().min;
                outlined.draw(|gx, gy, alpha| {
                    let abs = [bmin.x + gx as f32, bmin.y + gy as f32];
                    let r = rotate_point(abs, pos, angle_deg);
                    blend_pixel(img, r[0] as i64, r[1] as i64, c, alpha);
                });
            }
            x += advance * cos;
            y += advance * sin;
        }
        // 换行沿基线法线下移
        pen[0] += -sin * line_height;
        pen[1] += cos * line_height;
    }
}

// ---------- 图片变换（RotateImage / ResizeImage 命令支持） ----------

/// 按 90° 倍数旋转；非 90° 倍数四舍五入到最近的 90°。
pub fn apply_rotation(img: RgbaImage, theta_deg: f64) -> RgbaImage {
    let turns = ((theta_deg / 90.0).round() as i64).rem_euclid(4);
    match turns {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        3 => image::imageops::rotate270(&img),
        _ => img,
    }
}

/// JPEG 编码（原格式写回用；RGBA 的 alpha 通道被丢弃）。
pub fn encode_jpeg(img: &RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgba8(img.clone())
        .write_with_encoder(encoder)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

// ---------- 画布状态与交互 ----------

/// 单轴缩放轴（边中点把手）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

/// 一次把手缩放拖拽的快照。
#[derive(Debug, Clone)]
pub struct ScaleDrag {
    /// 缩放锚点（角点缩放 = 对角；边中点缩放 = 对边中点，图片像素）。
    pub anchor: [f32; 2],
    /// 拖拽起始指针位置（图片像素）。
    pub start_pointer: [f32; 2],
    /// 拖拽开始时的选中标注副本（每帧据此重算，避免误差累积）。
    pub orig: Vec<Annotation>,
    /// None = 角点等比缩放；Some(X/Y) = 边中点单轴缩放。
    pub axis: Option<Axis>,
}

/// 一次旋转把手拖拽的快照。
#[derive(Debug, Clone)]
pub struct RotDrag {
    /// 旋转枢轴（可参数化：公共包围盒中心或画布中心等外部点，图片像素）。
    pub pivot: [f32; 2],
    /// 拖拽起始方位角（度）。
    pub start_angle: f32,
    /// 拖拽开始时的选中标注副本（angle 为原值）。
    pub orig: Vec<Annotation>,
}

pub struct AnnotateState {
    pub resource_uid: i64,
    pub note_uid: i64,
    pub title: String,
    pub filename: String,
    pub image: RgbaImage,
    pub texture: egui::TextureHandle,
    pub annotations: Vec<Annotation>,
    pub tool: Tool,
    pub color: [u8; 3],
    pub stroke: f32,
    pub drag_start: Option<[f32; 2]>,
    pub drag_now: Option<[f32; 2]>,
    /// Pen 拖拽的中间轨迹（图片像素）。
    pub drag_mids: Vec<[f32; 2]>,
    pub text_pending: Option<([f32; 2], String)>,
    /// Select 工具当前选中的标注下标集合（支持多选）。
    pub selected: Vec<usize>,
    /// 移动中的上次位置（增量平移基准）。
    pub move_last: Option<[f32; 2]>,
    /// 进行中的把手缩放：锚点 + 拖拽起点 + 原始标注副本。
    pub scaling: Option<ScaleDrag>,
    /// 进行中的旋转把手拖拽（枢轴可参数化）。
    pub rotating: Option<RotDrag>,
    /// 旋转枢轴切换：true = 绕画布中心（外部点），false = 绕选中公共包围盒中心。
    pub rotate_around_canvas: bool,
    /// 旋转文字的离屏栅格纹理缓存：标注下标 → (angle, 缓冲原点[img], 尺寸, 纹理)。
    pub text_tex_cache: HashMap<usize, (f32, [f32; 2], [f32; 2], egui::TextureHandle)>,
    /// 原资源 mime（JPEG 原格式写回依据）。
    pub original_mime: String,
    pub open: bool,
    pub status: String,
    /// Some(页码) = PDF 图注模式：保存产物为新资源而非替换原资源。
    pub pdf_mode: Option<u32>,
}

impl AnnotateState {
    pub fn new(
        resource_uid: i64,
        note_uid: i64,
        title: String,
        filename: String,
        original_mime: String,
        image: RgbaImage,
        texture: egui::TextureHandle,
    ) -> AnnotateState {
        AnnotateState {
            resource_uid,
            note_uid,
            title,
            filename,
            image,
            texture,
            annotations: Vec::new(),
            tool: Tool::Arrow,
            color: [230, 57, 70],
            stroke: 4.0,
            drag_start: None,
            drag_now: None,
            drag_mids: Vec::new(),
            selected: Vec::new(),
            scaling: None,
            rotating: None,
            rotate_around_canvas: false,
            text_tex_cache: HashMap::new(),
            move_last: None,
            original_mime,
            text_pending: None,
            open: true,
            status: String::new(),
            pdf_mode: None,
        }
    }

    /// 画布交互 + 绘制。返回是否产生了需要外部处理的提交（供状态栏提示）。
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let (w, h) = (self.image.width() as f32, self.image.height() as f32);
        let avail = ui.available_size() - Vec2::new(12.0, 42.0);
        let origin = ui.cursor().min;
        let (_, scale) = display_rect(origin, avail, w, h);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(w * scale, h * scale), Sense::click_and_drag());
        let origin = rect.min;
        let painter = ui.painter_at(rect);

        painter.image(
            self.texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // 旋转文字：离屏纹理 + 旋转 Mesh（角度非零的文字标注）
        let rotated_text_meshes = self.build_rotated_text_meshes(&ui.ctx().clone(), origin, scale);
        for m in rotated_text_meshes {
            painter.add(egui::Shape::mesh(m));
        }

        // 已提交标注（选中项带高亮框）
        for (i, a) in self.annotations.iter().enumerate() {
            a.paint(&painter, origin, scale);
            if self.selected.contains(&i) {
                let (mn, mx) = a.bbox();
                painter.rect_stroke(
                    Rect::from_two_pos(
                        img_to_screen(origin, scale, mn),
                        img_to_screen(origin, scale, mx),
                    ),
                    2.0,
                    Stroke::new(1.5_f32, Color32::from_rgb(255, 210, 0)),
                );
            }
        }
        // 框选矩形
        if let (Some(rs), Some(re)) = (self.drag_start, self.drag_now) {
            if self.tool == Tool::Select && self.scaling.is_none() {
                painter.rect_stroke(
                    Rect::from_two_pos(
                        img_to_screen(origin, scale, rs),
                        img_to_screen(origin, scale, re),
                    ),
                    0.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(120, 170, 255)),
                );
            }
        }
        // 控制把手：角点（黄，等比缩放）+ 边中点（青，单轴缩放）+ 旋转把手（紫）
        if self.tool == Tool::Select {
            if let Some((min, max)) = self.selection_common_bbox() {
                for c in handle_positions(min, max) {
                    let sp = img_to_screen(origin, scale, c);
                    painter.rect_filled(
                        Rect::from_center_size(sp, Vec2::splat(9.0)),
                        2.0,
                        Color32::from_rgb(255, 210, 0),
                    );
                }
                for c in edge_mid_positions(min, max) {
                    let sp = img_to_screen(origin, scale, c);
                    painter.rect_filled(
                        Rect::from_center_size(sp, Vec2::splat(8.0)),
                        2.0,
                        Color32::from_rgb(0, 200, 220),
                    );
                }
                let rh = rotate_handle_position(min, max, 24.0 * scale.max(0.5));
                let rsp = img_to_screen(origin, scale, rh);
                let tm = img_to_screen(origin, scale, edge_mid_positions(min, max)[0]);
                painter.line_segment(
                    [tm, rsp],
                    Stroke::new(1.0_f32, Color32::from_rgb(190, 120, 255)),
                );
                painter.circle_filled(rsp, 6.0, Color32::from_rgb(190, 120, 255));
            }
        }
        // 进行中预览
        if let (Some(start), Some(now)) = (self.drag_start, self.drag_now) {
            let preview = Annotation {
                tool: self.tool,
                color: self.color,
                stroke: self.stroke,
                start,
                end: now,
                text: self.text_pending.as_ref().map(|(_, t)| t.clone()),
                font_px: self.stroke * 6.0,
                mids: self.drag_mids.clone(),
                angle: 0.0,
            };
            preview.paint(&painter, origin, scale);
        }

        let pointer = response.interact_pointer_pos();
        let img_pt = pointer.map(|p| screen_to_img(origin, scale, p));

        // 拖拽/点击交互
        if response.drag_started() {
            if let Some(p) = img_pt {
                if self.tool == Tool::Select {
                    // 把手命中分级：旋转把手 > 边中点（单轴）> 角点（等比）
                    if let Some((min, max)) = self.selection_common_bbox() {
                        // 旋转把手（顶边中点外移）
                        let rh = rotate_handle_position(min, max, 24.0 * scale.max(0.5));
                        let rh_screen = Pos2::new(rh[0], rh[1]);
                        if handle_hit(pointer.unwrap_or(origin), &[rh_screen], 12.0).is_some() {
                            let pivot = if self.rotate_around_canvas {
                                let (w, h) =
                                    (self.image.width() as f32, self.image.height() as f32);
                                [w / 2.0, h / 2.0]
                            } else {
                                [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0]
                            };
                            self.start_rotation(pivot, pointer.unwrap_or(origin).into());
                            self.status = if self.rotate_around_canvas {
                                "旋转中（绕画布中心）".into()
                            } else {
                                "旋转中（绕公共中心）".into()
                            };
                            return;
                        }
                        // 边中点：单轴缩放
                        let edge_mids: [[f32; 2]; 4] = edge_mid_positions(min, max);
                        let edge_screen: [Pos2; 4] =
                            edge_mids.map(|c| img_to_screen(origin, scale, c));
                        if let Some(k) = handle_hit(pointer.unwrap_or(origin), &edge_screen, 10.0) {
                            let (anchor, axis) = match k {
                                0 => ([(min[0] + max[0]) / 2.0, max[1]], Some(Axis::Y)),
                                1 => ([min[0], (min[1] + max[1]) / 2.0], Some(Axis::X)),
                                2 => ([(min[0] + max[0]) / 2.0, min[1]], Some(Axis::Y)),
                                _ => ([max[0], (min[1] + max[1]) / 2.0], Some(Axis::X)),
                            };
                            let orig = self
                                .selected
                                .iter()
                                .filter_map(|&i| self.annotations.get(i).cloned())
                                .collect();
                            self.scaling = Some(ScaleDrag {
                                anchor,
                                start_pointer: p,
                                orig,
                                axis,
                            });
                            self.status =
                                format!("单轴缩放中（锚点 {:.0},{:.0}）", anchor[0], anchor[1]);
                            return;
                        }
                        // 角点：等比缩放（对角为锚）
                        let corners_img = handle_positions(min, max);
                        let corners_screen: [Pos2; 4] =
                            corners_img.map(|c| img_to_screen(origin, scale, c));
                        if let Some(k) =
                            handle_hit(pointer.unwrap_or(origin), &corners_screen, 10.0)
                        {
                            let anchor = corners_img[(k + 2) % 4]; // 对角为锚
                            let orig = self
                                .selected
                                .iter()
                                .filter_map(|&i| self.annotations.get(i).cloned())
                                .collect();
                            self.scaling = Some(ScaleDrag {
                                anchor,
                                start_pointer: p,
                                orig,
                                axis: None,
                            });
                            self.status =
                                format!("把手缩放中（锚点 {:.0},{:.0}）", anchor[0], anchor[1]);
                            return;
                        }
                    }
                    let shift = ui.input(|i| i.modifiers.shift);
                    match self.annotations.iter().rposition(|a| a.hit(p)) {
                        Some(idx) => {
                            if shift {
                                // Shift+点击：切换多选成员
                                match self.selected.iter().position(|&i| i == idx) {
                                    Some(pos) => {
                                        self.selected.remove(pos);
                                    }
                                    None => self.selected.push(idx),
                                }
                            } else if !self.selected.contains(&idx) {
                                self.selected = vec![idx];
                            }
                            self.move_last = Some(p);
                            self.status = format!("已选中 {} 条标注", self.selected.len());
                        }
                        None => {
                            // 空白处：开始框选
                            self.selected.clear();
                            self.move_last = None;
                            self.drag_start = Some(p);
                            self.drag_now = Some(p);
                            self.status = "框选标注…".into();
                        }
                    }
                } else {
                    self.drag_start = Some(p);
                    self.drag_now = Some(p);
                }
            }
        }
        if response.dragged() {
            if let Some(p) = img_pt {
                if self.tool == Tool::Select {
                    if let Some(rd) = self.rotating.as_ref() {
                        // 旋转：绕公共中心的角度增量
                        self.drag_now = Some(p);
                        let now_angle = angle_of(rd.pivot, p);
                        let delta = normalize_angle(now_angle - rd.start_angle);
                        self.apply_rotate_drag(delta);
                    } else if let Some(sd) = self.scaling.as_ref() {
                        // 把手缩放：从原始副本按当前系数重算
                        let (anchor, start_pointer, axis) = (sd.anchor, sd.start_pointer, sd.axis);
                        self.drag_now = Some(p);
                        match axis {
                            Some(Axis::X) => {
                                let f = drag_axis_factor_1d(anchor[0], start_pointer[0], p[0])
                                    .unwrap_or(1.0);
                                self.apply_scale_drag_axis(f, 1.0);
                            }
                            Some(Axis::Y) => {
                                let f = drag_axis_factor_1d(anchor[1], start_pointer[1], p[1])
                                    .unwrap_or(1.0);
                                self.apply_scale_drag_axis(1.0, f);
                            }
                            None => {
                                if let Some(f) = drag_scale_factor(anchor, start_pointer, p) {
                                    self.apply_scale_drag(f);
                                }
                            }
                        }
                    } else if let Some(last) = self.move_last {
                        if self.rotate_around_canvas {
                            // 绕外部点（画布中心）的轨道位移：坐标经 rotate_point
                            // 绕画布中心变换，自身 angle 刚体跟随
                            let c = [
                                self.image.width() as f32 / 2.0,
                                self.image.height() as f32 / 2.0,
                            ];
                            let delta = orbit_delta(c, last, p);
                            for &idx in &self.selected {
                                if let Some(a) = self.annotations.get_mut(idx) {
                                    a.orbit_around(c, delta);
                                }
                            }
                        } else {
                            // 常规平移
                            let (dx, dy) = (p[0] - last[0], p[1] - last[1]);
                            for &idx in &self.selected {
                                if let Some(a) = self.annotations.get_mut(idx) {
                                    a.translate(dx, dy);
                                }
                            }
                        }
                        self.move_last = Some(p);
                    } else if self.drag_start.is_some() {
                        // 框选拖拽
                        self.drag_now = Some(p);
                    }
                } else {
                    if self.tool == Tool::Pen {
                        let far = self
                            .drag_mids
                            .last()
                            .or(self.drag_start.as_ref())
                            .map(|last| (p[0] - last[0]).powi(2) + (p[1] - last[1]).powi(2) > 4.0)
                            .unwrap_or(true);
                        if far {
                            self.drag_mids.push(p);
                        }
                    }
                    self.drag_now = Some(p);
                }
            }
        }
        if response.drag_stopped() {
            if self.tool == Tool::Select {
                if self.rotating.take().is_some() {
                    self.drag_start = None;
                    self.drag_now = None;
                    self.status = "旋转完成".into();
                    return;
                }
                if self.scaling.take().is_some() {
                    self.drag_start = None;
                    self.drag_now = None;
                    self.status = "把手缩放完成".into();
                    return;
                }
                // 框选收尾：选中所有相交标注
                if self.move_last.is_none() {
                    if let (Some(rs), Some(re)) = (self.drag_start, self.drag_now) {
                        self.selected = select_by_rubber(&self.annotations, rs, re);
                        self.status = format!("框选 {} 条标注", self.selected.len());
                    }
                }
                self.drag_start = None;
                self.drag_now = None;
                self.move_last = None;
                return;
            }
            if let (Some(start), Some(now)) = (self.drag_start, self.drag_now) {
                let dist = ((now[0] - start[0]).powi(2) + (now[1] - start[1]).powi(2)).sqrt();
                if self.tool == Tool::Text {
                    // 单击进入文字放置
                    self.text_pending = Some((start, String::new()));
                } else if self.tool == Tool::Pen {
                    // 画笔：有足够轨迹即提交
                    if dist >= 3.0 || !self.drag_mids.is_empty() {
                        self.annotations.push(Annotation {
                            tool: Tool::Pen,
                            color: self.color,
                            stroke: self.stroke,
                            start,
                            end: now,
                            text: None,
                            font_px: 0.0,
                            mids: std::mem::take(&mut self.drag_mids),
                            angle: 0.0,
                        });
                    }
                } else if dist >= 3.0 {
                    self.annotations.push(Annotation {
                        tool: self.tool,
                        color: self.color,
                        stroke: self.stroke,
                        start,
                        end: now,
                        text: None,
                        font_px: self.stroke * 6.0,
                        mids: Vec::new(),
                        angle: 0.0,
                    });
                }
            }
            self.drag_start = None;
            self.drag_now = None;
            self.drag_mids.clear();
        }

        // 画布下方状态行
        ui.add_space(4.0);
        let zoom = format!("图片 {}×{}", self.image.width(), self.image.height());
        let hint = match self.tool {
            Tool::Text => "点击画布放置文字",
            Tool::Select => "点击选中（Shift 加选）/空白处框选，拖拽移动",
            _ => "在画布上拖拽绘制",
        };
        ui.label(
            egui::RichText::new(format!(
                "{zoom} · {hint} · 标注 {} 条 · 选中 {}",
                self.annotations.len(),
                self.selected.len()
            ))
            .weak()
            .small(),
        );
    }

    /// 文字放置输入行。返回 true 表示有新文字标注提交。
    pub fn draw_text_input(&mut self, ui: &mut egui::Ui) -> bool {
        let Some((pos, _)) = self.text_pending.clone() else {
            return false;
        };
        let mut committed = false;
        ui.horizontal(|ui| {
            ui.label(format!("在 ({:.0}, {:.0}) 放置文字:", pos[0], pos[1]));
            if let Some((_, text)) = self.text_pending.as_mut() {
                let resp = ui.text_edit_singleline(text);
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (resp.lost_focus() && enter) || ui.button("放置").clicked() {
                    let t = self.text_pending.as_ref().unwrap().1.clone();
                    if !t.trim().is_empty() {
                        self.annotations.push(Annotation {
                            tool: Tool::Text,
                            color: self.color,
                            stroke: self.stroke,
                            start: pos,
                            end: pos,
                            text: Some(t),
                            font_px: (self.stroke * 6.0).max(16.0),
                            mids: Vec::new(),
                            angle: 0.0,
                        });
                        committed = true;
                    }
                    self.text_pending = None;
                }
                if ui.button("取消").clicked() {
                    self.text_pending = None;
                }
            }
        });
        committed
    }

    /// 替换工作图（旋转/重置）并重建纹理。
    pub fn set_image(&mut self, ctx: &egui::Context, img: RgbaImage) {
        let size = [img.width() as usize, img.height() as usize];
        self.texture = ctx.load_texture(
            "annotate",
            egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        self.image = img;
    }

    /// 以指定枢轴开始一次旋转拖拽（绕外部点旋转的参数化入口）。
    pub fn start_rotation(&mut self, pivot: [f32; 2], pointer: [f32; 2]) {
        let start_angle = angle_of(pivot, pointer);
        let orig = self
            .selected
            .iter()
            .filter_map(|&i| self.annotations.get(i).cloned())
            .collect();
        self.rotating = Some(RotDrag {
            pivot,
            start_angle,
            orig,
        });
    }

    /// 旋转文字的离屏纹理：按需栅格化并缓存（angle 或锚点变化时重建）。
    /// 返回 (纹理, 缓冲原点[img], 尺寸)。
    fn ensure_text_texture(
        &mut self,
        ctx: &egui::Context,
        idx: usize,
    ) -> (egui::TextureHandle, [f32; 2], [f32; 2]) {
        let a = self.annotations[idx].clone();
        let (r_start, _, _) = a.rotated_points();
        let font_px = a.font_px.max(10.0);
        let pad = 4.0f32;

        // 缓存命中（同角度同锚点）
        if let Some((cached_angle, cached_origin, size, tex)) = self.text_tex_cache.get(&idx) {
            if (*cached_angle - a.angle).abs() < 0.1
                && (cached_origin[0] - (r_start[0] - pad)).abs() < 0.5
                && (cached_origin[1] - (r_start[1] - pad)).abs() < 0.5
            {
                return (tex.clone(), *cached_origin, *size);
            }
        }

        // 量取文字块尺寸（基线左端锚点，逐字 advance 求宽）
        let text = a.text.as_deref().unwrap_or("");
        let font = load_cjk_font().expect("系统中文字体已在启动时加载");
        let scaled = font.into_scaled(PxScale::from(font_px));
        let mut text_w = 1.0f32;
        for line in text.split('\n') {
            let mut w = 0.0f32;
            for ch in line.chars() {
                let g = scaled.scaled_glyph(ch);
                w += scaled.h_advance(g.id);
            }
            text_w = text_w.max(w);
        }
        let n_lines = text.split('\n').count().max(1);
        let ascent = scaled.ascent();
        let line_height = font_px * 1.35;

        let size = [
            (text_w + pad * 2.0).ceil(),
            (ascent + line_height * n_lines as f32 + pad * 2.0).ceil(),
        ];
        let (bw, bh) = (size[0] as u32, size[1] as u32);
        let mut buf = RgbaImage::new(bw, bh);
        // 缓冲内锚点 = (pad, pad + ascent)，字形旋转由 draw_text 处理
        draw_text(
            &mut buf,
            [pad, pad + ascent],
            text,
            a.color,
            font_px,
            a.angle,
        );
        let tex = ctx.load_texture(
            format!("biji-text-{idx}"),
            egui::ColorImage::from_rgba_unmultiplied([bw as usize, bh as usize], buf.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        // 缓冲原点（img 坐标）：锚点减去缓冲内锚点偏移
        let origin = [r_start[0] - pad, r_start[1] - pad - ascent];
        self.text_tex_cache
            .insert(idx, (a.angle, origin, size, tex.clone()));
        (tex, origin, size)
    }

    /// 旋转文字（angle != 0）的旋转 Mesh 列表：离屏纹理贴到旋转后四角。
    fn build_rotated_text_meshes(
        &mut self,
        ctx: &egui::Context,
        origin: Pos2,
        scale: f32,
    ) -> Vec<egui::Mesh> {
        let mut out = Vec::new();
        let items: Vec<usize> = (0..self.annotations.len())
            .filter(|&i| self.annotations[i].tool == Tool::Text && self.annotations[i].angle != 0.0)
            .collect();
        for idx in items {
            let (tex, t_origin, t_size) = self.ensure_text_texture(ctx, idx);
            let a = &self.annotations[idx];
            let c0 = rotate_point([t_origin[0], t_origin[1]], a.start, a.angle);
            let c1 = rotate_point([t_origin[0] + t_size[0], t_origin[1]], a.start, a.angle);
            let c2 = rotate_point(
                [t_origin[0] + t_size[0], t_origin[1] + t_size[1]],
                a.start,
                a.angle,
            );
            let c3 = rotate_point([t_origin[0], t_origin[1] + t_size[1]], a.start, a.angle);
            let cs = [c0, c1, c2, c3].map(|p| img_to_screen(origin, scale, p));
            let uvs = [
                Pos2::new(0.0, 0.0),
                Pos2::new(1.0, 0.0),
                Pos2::new(1.0, 1.0),
                Pos2::new(0.0, 1.0),
            ];
            let mut mesh = egui::Mesh {
                texture_id: tex.id(),
                ..Default::default()
            };
            for (p, uv) in cs.iter().zip(uvs.iter()) {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: *p,
                    uv: *uv,
                    color: Color32::WHITE,
                });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            out.push(mesh);
        }
        out
    }

    /// 选中标注的公共包围盒（无选中返回 None）。
    pub fn selection_common_bbox(&self) -> Option<([f32; 2], [f32; 2])> {
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [-f32::MAX, -f32::MAX];
        for &i in &self.selected {
            if let Some(a) = self.annotations.get(i) {
                let (mn, mx) = a.bbox();
                min[0] = min[0].min(mn[0]);
                min[1] = min[1].min(mn[1]);
                max[0] = max[0].max(mx[0]);
                max[1] = max[1].max(mx[1]);
            }
        }
        if self.selected.is_empty() {
            None
        } else {
            Some((min, max))
        }
    }

    /// 以系数应用一次把手缩放（锚点与原始副本来自 `scaling` 快照，
    /// 每帧从原始副本重算，避免连续帧误差累积）。
    /// `axis: None` = 角点等比；`Some(X/Y)` = 边中点单轴。
    pub fn apply_scale_drag_axis(&mut self, fx: f32, fy: f32) {
        let Some(sd) = self.scaling.as_ref() else {
            return;
        };
        let (fx, fy) = match sd.axis {
            Some(Axis::X) => (fx, 1.0),
            Some(Axis::Y) => (1.0, fy),
            None => ((fx + fy) / 2.0, (fx + fy) / 2.0),
        };
        for (k, &idx) in self.selected.iter().enumerate() {
            if let (Some(orig), Some(slot)) = (sd.orig.get(k), self.annotations.get_mut(idx)) {
                *slot = orig.scaled_about_axis(sd.anchor, fx, fy);
            }
        }
    }

    /// 等比便捷封装。
    pub fn apply_scale_drag(&mut self, factor: f32) {
        let Some(sd) = self.scaling.as_ref() else {
            return;
        };
        for (k, &idx) in self.selected.iter().enumerate() {
            if let (Some(orig), Some(slot)) = (sd.orig.get(k), self.annotations.get_mut(idx)) {
                *slot = orig.scaled_about(sd.anchor, factor);
            }
        }
    }

    /// 应用一次旋转增量（角度快照重算模式，与缩放一致）。
    /// 增量相对 `RotDrag.pivot` 的起始方位角；绕外部点旋转即更换 pivot。
    pub fn apply_rotate_drag(&mut self, angle_delta: f32) {
        let Some(rd) = self.rotating.as_ref() else {
            return;
        };
        for (k, &idx) in self.selected.iter().enumerate() {
            if let (Some(orig), Some(slot)) = (rd.orig.get(k), self.annotations.get_mut(idx)) {
                slot.angle = normalize_angle(orig.angle + angle_delta);
            }
        }
    }

    pub fn undo(&mut self) {
        self.annotations.pop();
        self.selected.clear();
        self.scaling = None;
        self.rotating = None;
    }

    pub fn clear(&mut self) {
        self.annotations.clear();
        self.selected.clear();
        self.rotating = None;
    }

    /// 删除全部选中标注，返回删除数。
    pub fn delete_selected(&mut self) -> usize {
        let mut idxs = self.selected.clone();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        let mut n = 0;
        for i in idxs {
            if i < self.annotations.len() {
                self.annotations.remove(i);
                n += 1;
            }
        }
        self.selected.clear();
        n
    }

    /// 以选中标注的公共包围盒中心为基准等比缩放（含线宽与字号）。
    pub fn scale_selected(&mut self, factor: f32) {
        if self.selected.is_empty() {
            return;
        }
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [-f32::MAX, -f32::MAX];
        for &i in &self.selected {
            if let Some(a) = self.annotations.get(i) {
                let (mn, mx) = a.bbox();
                min[0] = min[0].min(mn[0]);
                min[1] = min[1].min(mn[1]);
                max[0] = max[0].max(mx[0]);
                max[1] = max[1].max(mx[1]);
            }
        }
        let (cx, cy) = ((min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0);
        let scale_pt = |p: &mut [f32; 2]| {
            p[0] = cx + (p[0] - cx) * factor;
            p[1] = cy + (p[1] - cy) * factor;
        };
        for &i in &self.selected {
            if let Some(a) = self.annotations.get_mut(i) {
                scale_pt(&mut a.start);
                scale_pt(&mut a.end);
                for p in &mut a.mids {
                    scale_pt(p);
                }
                a.stroke = (a.stroke * factor).max(1.0);
                if a.font_px > 0.0 {
                    a.font_px = (a.font_px * factor).max(8.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_img() -> RgbaImage {
        RgbaImage::from_pixel(100, 80, image::Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn coord_roundtrip() {
        let origin = Pos2::new(50.0, 30.0);
        let scale = 1.5;
        let img_pt = [20.0, 10.0];
        let screen = img_to_screen(origin, scale, img_pt);
        let back = screen_to_img(origin, scale, screen);
        assert!((back[0] - 20.0).abs() < 1e-4);
        assert!((back[1] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn display_rect_keeps_aspect() {
        let (rect, scale) = display_rect(Pos2::ZERO, Vec2::new(400.0, 200.0), 800.0, 600.0);
        // 受高度限制：scale = 200/600 ≈ 0.3333，宽度 800*scale ≈ 266.7 ≤ 400。
        assert!((scale - 200.0 / 600.0).abs() < 1e-4);
        assert!((rect.width() - 800.0 * scale).abs() < 1e-4);
        assert!((rect.height() - 200.0).abs() < 1e-4);
    }

    #[test]
    fn line_rasterize_colors_pixels() {
        let mut img = white_img();
        let a = Annotation {
            tool: Tool::Arrow,
            color: [255, 0, 0],
            stroke: 4.0,
            start: [10.0, 40.0],
            end: [80.0, 40.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        a.rasterize(&mut img);
        let mid = img.get_pixel(45, 40);
        assert_eq!(mid.0[0], 255, "线中点应为红色");
        assert_eq!(mid.0[1], 0);
        // 线外仍是白色
        let outside = img.get_pixel(45, 10);
        assert_eq!(outside.0[0], 255);
    }

    #[test]
    fn rect_rasterize_strokes_border_only() {
        let mut img = white_img();
        let a = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 255],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [60.0, 50.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        a.rasterize(&mut img);
        let border = img.get_pixel(10, 30);
        assert_eq!(border.0[2], 255, "左边框应为蓝色");
        let inside = img.get_pixel(35, 30);
        assert_eq!(inside.0[0], 255, "内部应保持白色");
    }

    #[test]
    fn pen_rasterize_follows_polyline() {
        let mut img = white_img();
        let a = Annotation {
            tool: Tool::Pen,
            color: [255, 140, 0],
            stroke: 4.0,
            start: [10.0, 10.0],
            end: [70.0, 70.0],
            text: None,
            font_px: 0.0,
            mids: vec![[40.0, 10.0], [40.0, 70.0]],
            angle: 0.0,
        };
        a.rasterize(&mut img);
        // 折线三段中段的采样点应为橙色（非白）
        let p1 = img.get_pixel(40, 10);
        assert!(p1.0[0] > 200 && p1.0[2] < 120, "横段起点应被涂色");
        let p2 = img.get_pixel(40, 70);
        assert!(p2.0[0] > 200 && p2.0[2] < 120, "竖段终点应被涂色");
        let p3 = img.get_pixel(70, 70);
        assert!(p3.0[0] > 200 && p3.0[2] < 120, "末段终点应被涂色");
        // 远离折线的角落仍是白色
        let far = img.get_pixel(95, 5);
        assert_eq!(far.0[0], 255);
    }

    #[test]
    fn text_rasterize_and_png_roundtrip() {
        let mut img = white_img();
        let a = Annotation {
            tool: Tool::Text,
            color: [0, 128, 0],
            stroke: 4.0,
            start: [5.0, 5.0],
            end: [5.0, 5.0],
            text: Some("测试中文".into()),
            font_px: 24.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        a.rasterize(&mut img);
        // 中文栅格化必然改动若干像素（有字体时）
        if load_cjk_font().is_some() {
            let mut changed = 0;
            for y in 0..40u32 {
                for x in 0..90u32 {
                    if img.get_pixel(x, y).0[0] != 255 {
                        changed += 1;
                    }
                }
            }
            assert!(changed > 20, "文字应覆盖一批像素，实际 {changed}");
        }

        // PNG 编解码回环：标注像素保持
        let png = encode_png(&img).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (100, 80));
        assert_eq!(decoded.get_pixel(45, 40).0[0], 255);
    }

    #[test]
    fn jpeg_roundtrip_keeps_annotation_pixels() {
        // 原格式：先造一张 JPEG 源图，标注后按 JPEG 编码，解码验证像素被涂色
        let src = white_img();
        let jpeg_bytes = encode_jpeg(&src, 90).unwrap();
        let decoded = image::load_from_memory(&jpeg_bytes).unwrap().to_rgba8();
        let mut img = decoded;
        let a = Annotation {
            tool: Tool::Arrow,
            color: [255, 0, 0],
            stroke: 6.0,
            start: [20.0, 40.0],
            end: [70.0, 40.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        a.rasterize(&mut img);
        let out = encode_jpeg(&img, 95).unwrap();
        let check = image::load_from_memory(&out).unwrap().to_rgba8();
        // JPEG 有损：容忍少量色偏，检查红色通道显著高于绿/蓝
        let px = check.get_pixel(45, 40);
        assert!(
            px.0[0] > 150 && px.0[0] > px.0[1] + 40 && px.0[0] > px.0[2] + 40,
            "JPEG 写回后箭头像素应仍偏红: {px:?}"
        );
    }

    #[test]
    fn rubber_selection_hits_intersecting() {
        let anns = vec![
            Annotation {
                tool: Tool::Rect,
                color: [0, 0, 0],
                stroke: 2.0,
                start: [5.0, 5.0],
                end: [30.0, 30.0],
                text: None,
                font_px: 0.0,
                mids: Vec::new(),
                angle: 0.0,
            },
            Annotation {
                tool: Tool::Rect,
                color: [0, 0, 0],
                stroke: 2.0,
                start: [50.0, 50.0],
                end: [90.0, 90.0],
                text: None,
                font_px: 0.0,
                mids: Vec::new(),
                angle: 0.0,
            },
        ];
        // 框选只覆盖第一个
        let hit = select_by_rubber(&anns, [0.0, 0.0], [40.0, 40.0]);
        assert_eq!(hit, vec![0]);
        // 框选覆盖全部
        let hit = select_by_rubber(&anns, [0.0, 0.0], [200.0, 200.0]);
        assert_eq!(hit.len(), 2);
        // 框选空区
        assert!(select_by_rubber(&anns, [95.0, 0.0], [99.0, 40.0]).is_empty());
    }

    #[test]
    fn scale_selected_about_common_center() {
        let store_img = white_img();
        let texture = egui::Context::default().load_texture(
            "t",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );
        let mut st = AnnotateState::new(
            1,
            1,
            "t".into(),
            "f.png".into(),
            "image/png".into(),
            store_img,
            texture,
        );
        st.annotations = vec![
            Annotation {
                tool: Tool::Rect,
                color: [0, 0, 0],
                stroke: 2.0,
                start: [10.0, 10.0],
                end: [30.0, 30.0],
                text: None,
                font_px: 0.0,
                mids: Vec::new(),
                angle: 0.0,
            },
            Annotation {
                tool: Tool::Rect,
                color: [0, 0, 0],
                stroke: 2.0,
                start: [70.0, 10.0],
                end: [90.0, 30.0],
                text: None,
                font_px: 0.0,
                mids: Vec::new(),
                angle: 0.0,
            },
        ];
        st.selected = vec![0, 1];
        // 公共中心 = (50, 20)；放大 2x 后第一块 start.x 应远离中心
        let before = st.annotations[0].start[0];
        st.scale_selected(2.0);
        let after = st.annotations[0].start[0];
        assert!(
            after < before,
            "远离中心的点放大后应更远: before={before} after={after}"
        );
        assert!(
            (st.annotations[0].stroke - 4.0).abs() < 1e-4,
            "线宽同步缩放"
        );
        // 不在选中集合里的标注不动（此处全部选中，跳过）
    }

    #[test]
    fn translate_moves_all_points() {
        let mut a = Annotation {
            tool: Tool::Pen,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [0.0, 0.0],
            end: [10.0, 10.0],
            text: None,
            font_px: 0.0,
            mids: vec![[5.0, 0.0]],
            angle: 0.0,
        };
        a.translate(10.0, 5.0);
        assert_eq!(a.start, [10.0, 5.0]);
        assert_eq!(a.mids[0], [15.0, 5.0]);
        assert_eq!(a.end, [20.0, 15.0]);
    }

    #[test]
    fn handle_positions_are_four_corners() {
        let corners = handle_positions([10.0, 20.0], [30.0, 60.0]);
        assert_eq!(corners[0], [10.0, 20.0]); // 左上
        assert_eq!(corners[1], [30.0, 20.0]); // 右上
        assert_eq!(corners[2], [30.0, 60.0]); // 右下
        assert_eq!(corners[3], [10.0, 60.0]); // 左下
    }

    #[test]
    fn handle_hit_within_tolerance() {
        let corners = handle_positions([0.0, 0.0], [100.0, 100.0]);
        let corners_screen = corners.map(|c| Pos2::new(c[0], c[1]));
        assert_eq!(
            handle_hit(Pos2::new(2.0, 1.0), &corners_screen, 5.0),
            Some(0)
        );
        assert_eq!(
            handle_hit(Pos2::new(99.0, 102.0), &corners_screen, 5.0),
            Some(2)
        );
        assert_eq!(
            handle_hit(Pos2::new(50.0, 50.0), &corners_screen, 5.0),
            None
        );
    }

    #[test]
    fn drag_scale_factor_axes() {
        let anchor = [0.0, 0.0];
        // 两轴均可用：取平均
        assert!(
            (drag_scale_factor(anchor, [10.0, 10.0], [20.0, 30.0]).unwrap() - 2.5).abs() < 1e-4
        );
        // 单轴退化（水平把手）：只用可用轴
        assert!((drag_scale_factor(anchor, [10.0, 0.0], [30.0, 50.0]).unwrap() - 3.0).abs() < 1e-4);
        // 位移过小：None
        assert!(drag_scale_factor(anchor, [1.0, 1.0], [1.0, 1.0]).is_none());
    }

    #[test]
    fn scaled_about_anchor() {
        let a = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [30.0, 30.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        // 锚点 (0,0)，2 倍：所有点远离锚点一倍
        let s = a.scaled_about([0.0, 0.0], 2.0);
        assert_eq!(s.start, [20.0, 20.0]);
        assert_eq!(s.end, [60.0, 60.0]);
        assert!((s.stroke - 4.0).abs() < 1e-4);
        // 锚点本身缩放系数 1 不变
        let s1 = a.scaled_about([10.0, 10.0], 1.0);
        assert_eq!(s1.start, a.start);
    }

    #[test]
    fn apply_scale_drag_from_snapshot() {
        let store_img = white_img();
        let texture = egui::Context::default().load_texture(
            "t",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );
        let orig = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [30.0, 30.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        let mut st = AnnotateState::new(
            1,
            1,
            "t".into(),
            "f.png".into(),
            "image/png".into(),
            store_img,
            texture,
        );
        st.annotations.push(orig.clone());
        st.selected = vec![0];
        st.scaling = Some(ScaleDrag {
            anchor: [0.0, 0.0],
            start_pointer: [20.0, 20.0],
            orig: vec![orig.clone()],
            axis: None,
        });
        // 把手从 (20,20) 拖到 (40,40)：系数 2
        st.apply_scale_drag(2.0);
        assert_eq!(st.annotations[0].start, [20.0, 20.0]);
        assert_eq!(st.annotations[0].end, [60.0, 60.0]);
        // 原始快照未被污染
        assert_eq!(st.scaling.as_ref().unwrap().orig[0].start, [10.0, 10.0]);
    }

    #[test]
    fn rotate_point_quadrants() {
        let c = [0.0, 0.0];
        // 屏幕坐标系 y 向下，90° 顺时针：+x → +y
        let p = rotate_point([10.0, 0.0], c, 90.0);
        assert!((p[0] - 0.0).abs() < 1e-4 && (p[1] - 10.0).abs() < 1e-4);
        // 180°：+x → -x
        let p = rotate_point([10.0, 0.0], c, 180.0);
        assert!((p[0] + 10.0).abs() < 1e-4 && p[1].abs() < 1e-4);
        // 绕非原点中心
        let p = rotate_point([20.0, 10.0], [10.0, 10.0], 90.0);
        assert!((p[0] - 10.0).abs() < 1e-4 && (p[1] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn angle_of_and_normalize() {
        let c = [0.0, 0.0];
        assert!(angle_of(c, [10.0, 0.0]).abs() < 1e-4);
        assert!((angle_of(c, [0.0, 10.0]) - 90.0).abs() < 1e-4);
        assert!((angle_of(c, [-10.0, 0.0]).abs() - 180.0).abs() < 1e-4);
        assert_eq!(normalize_angle(270.0), -90.0);
        assert_eq!(normalize_angle(-270.0), 90.0);
        assert_eq!(normalize_angle(45.0), 45.0);
    }

    #[test]
    fn axis_factor_1d_normal_and_degenerate() {
        // 锚点 0，从 10 拖到 30 → 3 倍
        assert!((drag_axis_factor_1d(0.0, 10.0, 30.0).unwrap() - 3.0).abs() < 1e-4);
        // 锚点即起点：退化
        assert!(drag_axis_factor_1d(5.0, 5.0, 30.0).is_none());
    }

    #[test]
    fn scaled_about_axis_only_x() {
        let a = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [30.0, 30.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        // 锚点 (0,0)，仅 X 缩放 2 倍：x 翻倍、y 不变
        let s = a.scaled_about_axis([0.0, 0.0], 2.0, 1.0);
        assert_eq!(s.start, [20.0, 10.0]);
        assert_eq!(s.end, [60.0, 30.0]);
    }

    #[test]
    fn draw_text_rotates_glyph_pixels_around_anchor() {
        // 90° 旋转（绕锚点）：无旋转时字形在锚点右下方（x>=ax, y 接近 ay）；
        // 旋转后应移到锚点左侧（x < ax），证明字形像素被旋转。
        let mut img_unrot = white_img();
        draw_text(&mut img_unrot, [40.0, 40.0], "III", [255, 0, 0], 24.0, 0.0);
        let mut img_rot = white_img();
        draw_text(&mut img_rot, [40.0, 40.0], "III", [255, 0, 0], 24.0, 90.0);

        // 统计锚点左侧区域（x 0..38，y 全图）的红像素数
        let count_left = |img: &RgbaImage| -> u32 {
            let mut n = 0;
            for y in 0..img.height() {
                for x in 0..38u32 {
                    let px = img.get_pixel(x, y);
                    if px.0[0] > 180 && px.0[1] < 100 {
                        n += 1;
                    }
                }
            }
            n
        };
        {
            let mut pts = Vec::new();
            for y in 0..img_unrot.height() {
                for x in 0..60u32 {
                    let px = img_unrot.get_pixel(x, y);
                    if px.0[0] > 180 && px.0[1] < 100 {
                        pts.push((x, y));
                    }
                }
            }
            eprintln!("DBG unrot red pts (x<60): {pts:?}");
        }
        assert_eq!(count_left(&img_unrot), 0, "无旋转时字形不应出现在锚点左侧");
        assert!(count_left(&img_rot) >= 3, "90° 旋转后字形应移到锚点左侧");
    }

    #[test]
    fn orbit_delta_quadrants_and_wrap() {
        let c = [0.0, 0.0];
        // 同点：零增量
        assert!(orbit_delta(c, [10.0, 0.0], [10.0, 0.0]).abs() < 1e-4);
        // 右 → 下（屏幕 y 向下）：顺时针 90°
        assert!((orbit_delta(c, [10.0, 0.0], [0.0, 10.0]) - 90.0).abs() < 1e-4);
        // 跨 ±180° 边界归一：170° → -170° 的增量应为 +20°（经 180 折返）
        let rad = |deg: f32| deg.to_radians();
        let last = [10.0 * rad(170.0).cos(), 10.0 * rad(170.0).sin()];
        let now = [10.0 * rad(-170.0).cos(), 10.0 * rad(-170.0).sin()];
        let d = orbit_delta(c, last, now);
        assert!((d - 20.0).abs() < 1.0, "跨边界增量应归一: {d}");
    }

    #[test]
    fn orbit_around_rigid_motion() {
        let mut a = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [45.0, 45.0],
            end: [55.0, 55.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        // 绕画布中心 (50,50) 转 180°：对角点应镜像
        a.orbit_around([50.0, 50.0], 180.0);
        assert!((a.start[0] - 55.0).abs() < 1e-4 && (a.start[1] - 55.0).abs() < 1e-4);
        assert!((a.end[0] - 45.0).abs() < 1e-4 && (a.end[1] - 45.0).abs() < 1e-4);
        // 刚体跟随：自身 angle 同步 +180
        assert!((normalize_angle(a.angle) - 180.0).abs() < 1e-4);
    }

    #[test]
    fn rotation_pivot_parameterization() {
        // 绕外部点旋转：pivot 可配置，同指针方位角随 pivot 变化
        let store_img = white_img();
        let texture = egui::Context::default().load_texture(
            "t",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );
        let mut st = AnnotateState::new(
            1,
            1,
            "t".into(),
            "f.png".into(),
            "image/png".into(),
            store_img,
            texture,
        );
        st.annotations.push(Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [30.0, 30.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        });
        st.selected = vec![0];

        // 枢轴 A：指针在枢轴右侧 → 起始角 0
        st.start_rotation([20.0, 20.0], [30.0, 20.0]);
        let rd = st.rotating.as_ref().unwrap();
        assert_eq!(rd.pivot, [20.0, 20.0]);
        assert!(rd.start_angle.abs() < 1e-4);

        // 枢轴 B（外部点）：同指针方位角不同
        st.start_rotation([0.0, 0.0], [30.0, 20.0]);
        let rd = st.rotating.as_ref().unwrap();
        assert_eq!(rd.pivot, [0.0, 0.0]);
        assert!((rd.start_angle - 33.69).abs() < 0.1, "atan2(20,30)≈33.7°");
    }

    #[test]
    fn rotation_rasterize_draws_rotated_edge() {
        // 100×100 白图，矩形 (10,10)-(90,50) 旋转 90°（绕自身中心 (50,30)）
        let mut img = white_img();
        let a = Annotation {
            tool: Tool::Rect,
            color: [255, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [90.0, 50.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 90.0,
        };
        a.rasterize(&mut img);
        // 旋转后矩形覆盖 (30,-10)-(70,70)：可见区左边缘 x≈30 应有红色
        let mut red_on_rotated_left = 0;
        for y in 0..img.height() {
            let px = img.get_pixel(30, y);
            if px.0[0] > 180 && px.0[1] < 100 {
                red_on_rotated_left += 1;
            }
        }
        assert!(red_on_rotated_left >= 2, "旋转后左边缘应有红色描边");
    }

    #[test]
    fn apply_rotate_drag_from_snapshot() {
        let store_img = white_img();
        let texture = egui::Context::default().load_texture(
            "t",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );
        let orig = Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [10.0, 10.0],
            end: [30.0, 30.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        };
        let mut st = AnnotateState::new(
            1,
            1,
            "t".into(),
            "f.png".into(),
            "image/png".into(),
            store_img,
            texture,
        );
        st.annotations.push(orig.clone());
        st.selected = vec![0];
        st.rotating = Some(RotDrag {
            pivot: [20.0, 20.0],
            start_angle: 0.0,
            orig: vec![orig.clone()],
        });
        // 拖出 90° 增量（快照重算：angle = orig.angle + delta）
        st.apply_rotate_drag(90.0);
        assert!((st.annotations[0].angle - 90.0).abs() < 1e-4);
        // 快照未污染
        assert_eq!(st.rotating.as_ref().unwrap().orig[0].angle, 0.0);
        // 同 delta 重复应用幂等（对应 GUI 每帧重算）
        st.apply_rotate_drag(90.0);
        assert!((st.annotations[0].angle - 90.0).abs() < 1e-4);
        // 指针继续转动：delta=180 → 自 orig 重算为 180
        st.apply_rotate_drag(180.0);
        assert!((st.annotations[0].angle - 180.0).abs() < 1e-4);
    }

    #[test]
    fn rotation_turns() {
        let img = white_img();
        // 100×80 源图：90°/450°/-270° 等价（宽高互换），180° 不互换。
        assert_eq!(apply_rotation(img.clone(), 90.0).dimensions(), (80, 100));
        assert_eq!(apply_rotation(img.clone(), 450.0).dimensions(), (80, 100));
        assert_eq!(apply_rotation(img.clone(), -270.0).dimensions(), (80, 100));
        assert_eq!(apply_rotation(img.clone(), 180.0).dimensions(), (100, 80));
        assert_eq!(apply_rotation(img.clone(), 0.0).dimensions(), (100, 80));
    }

    #[test]
    fn undo_and_clear() {
        let store_img = white_img();
        let texture = egui::Context::default().load_texture(
            "test",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );
        let mut st = AnnotateState::new(
            1,
            1,
            "t".into(),
            "f.png".into(),
            "image/png".into(),
            store_img,
            texture,
        );
        st.annotations.push(Annotation {
            tool: Tool::Rect,
            color: [0, 0, 0],
            stroke: 2.0,
            start: [0.0, 0.0],
            end: [9.0, 9.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        });
        st.undo();
        assert!(st.annotations.is_empty());
        st.annotations.push(Annotation {
            tool: Tool::Arrow,
            color: [1, 2, 3],
            stroke: 1.0,
            start: [0.0; 2],
            end: [1.0, 1.0],
            text: None,
            font_px: 0.0,
            mids: Vec::new(),
            angle: 0.0,
        });
        st.clear();
        assert!(st.annotations.is_empty());
    }
}
