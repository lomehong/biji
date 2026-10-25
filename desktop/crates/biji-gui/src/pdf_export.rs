//! 笔记导出 PDF（对位原客户端「导出 PDF」）。
//!
//! 零新依赖链路：ENML → 纯文本排版（ab_glyph 度量换行，复用标注画布的
//! 中文字体与栅格化）→ A4 白底位图分页 → JPEG（image crate）→
//! 手写最小 image-only PDF 容器（DCTDecode XObject + xref）。

use crate::annotate::{draw_text, load_cjk_font};
use ab_glyph::{Font, PxScale, ScaleFont};
use std::io::Write;
use std::path::Path;

/// A4 @150dpi。
const PAGE_W: u32 = 1240;
const PAGE_H: u32 = 1754;
/// 四边距（px）。
const MARGIN: f32 = 96.0;
const TITLE_PX: f32 = 34.0;
const BODY_PX: f32 = 20.0;
/// 行距倍数。
const LINE_H: f32 = 1.5;

/// 排版行（供单测）：按字宽测量贪心换行，返回 (行文本, 行宽)。
fn wrap_line(line: &str, font_px: f32, max_w: f32) -> Vec<String> {
    let Some(font) = load_cjk_font() else {
        return vec![line.to_string()];
    };
    let scaled = font.into_scaled(PxScale::from(font_px));
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0.0;
    for ch in line.chars() {
        let adv = scaled.h_advance(scaled.glyph_id(ch));
        if w + adv > max_w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0.0;
        }
        cur.push(ch);
        w += adv;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 导出笔记为 PDF：返回页数；Err 为可读错误（字体缺失/写盘失败）。
pub fn note_to_pdf(title: &str, enml: &str, out_path: &Path) -> Result<usize, String> {
    // ---- 文本：ENML → 纯文本（sanitize 容错：坏文按原样落纯文本）
    let body = biji_enml::Enml::parse(enml)
        .map(|mut d| {
            d.sanitize();
            d.to_plain_text()
        })
        .unwrap_or_else(|_| enml.to_string());

    // ---- 排版：标题 + 正文行（手动换行）
    let inner_w = PAGE_W as f32 - MARGIN * 2.0;
    let mut lines: Vec<(String, bool)> = Vec::new(); // (文本, 是否标题)
    for l in wrap_line(title, TITLE_PX, inner_w) {
        lines.push((l, true));
    }
    lines.push((String::new(), true)); // 标题与正文间隔
    for raw in body.lines() {
        if raw.trim().is_empty() {
            lines.push((String::new(), false));
        } else {
            for l in wrap_line(raw, BODY_PX, inner_w) {
                lines.push((l, false));
            }
        }
    }

    // ---- 分页 + 栅格化（复用标注画布的中文字体栅格化）
    let title_lh = TITLE_PX * LINE_H;
    let body_lh = BODY_PX * LINE_H;
    let usable_h = PAGE_H as f32 - MARGIN * 2.0;
    let mut pages: Vec<Vec<u8>> = Vec::new(); // JPEG 字节
    let mut y = 0.0f32;
    let mut img = image::RgbaImage::from_pixel(PAGE_W, PAGE_H, image::Rgba([255, 255, 255, 255]));
    let ink = [26u8, 26, 30];
    let flush = |img: image::RgbaImage, pages: &mut Vec<Vec<u8>>| -> Result<(), String> {
        let mut jpg = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 85);
        enc.encode_image(&img)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        pages.push(jpg);
        Ok(())
    };
    for (text, is_title) in lines {
        let lh = if is_title { title_lh } else { body_lh };
        if y + lh > usable_h {
            flush(std::mem::replace(
                &mut img,
                image::RgbaImage::from_pixel(PAGE_W, PAGE_H, image::Rgba([255, 255, 255, 255])),
            ), &mut pages)?;
            y = 0.0;
        }
        if !text.is_empty() {
            draw_text(&mut img, [MARGIN, MARGIN + y], &text, ink, if is_title { TITLE_PX } else { BODY_PX }, 0.0);
        }
        y += lh;
    }
    flush(img, &mut pages)?;
    if pages.is_empty() {
        return Err("没有可导出的内容".into());
    }

    // ---- 最小 PDF 容器（每页一个 DCTDecode XObject）
    // 点阵坐标（72dpi）：A4 = 595×842
    let pw = 595.0f32;
    let ph = 842.0f32;
    let n = pages.len();
    let mut buf: Vec<u8> = "%PDF-1.4\n".to_string().into_bytes();
    let mut offsets = Vec::with_capacity(3 + n * 3);
    let push = |buf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: String| {
        offsets.push(buf.len());
        buf.extend_from_slice(body.as_bytes());
    };
    // 对象编号：1=catalog 2=pages 3..(3+n)=page  (3+n)..(3+2n)=image (3+2n)..(3+3n)=content
    push(&mut buf, &mut offsets, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".into());
    push(&mut buf, &mut offsets, format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {n} >>\nendobj\n",
        (3..3 + n).map(|i| format!("{i} 0 R")).collect::<Vec<_>>().join(" ")));
    for (i, jpg) in pages.iter().enumerate() {
        let page_no = 3 + i;
        let img_no = 3 + n + i;
        let cont_no = 3 + 2 * n + i;
        push(&mut buf, &mut offsets, format!(
            "{page_no} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /XObject << /Im{i} {img_no} 0 R >> >> /Contents {cont_no} 0 R >>\nendobj\n"));
        push(&mut buf, &mut offsets, format!(
            "{img_no} 0 obj\n<< /Type /XObject /Subtype /Image /Width {PAGE_W} /Height {PAGE_H} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpg.len()));
        buf.extend_from_slice(jpg);
        buf.extend_from_slice(b"\nendstream\nendobj\n");
        let content = format!("q {pw} 0 0 {ph} 0 0 cm /Im{i} Do Q\n");
        push(&mut buf, &mut offsets, format!(
            "{cont_no} 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
            content.len()));
    }
    let xref_at = buf.len();
    let count = offsets.len() + 1; // +1 是 xref 自身的 0 号条目
    buf.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());

    let mut f = std::fs::File::create(out_path).map_err(|e| format!("创建文件失败: {e}"))?;
    f.write_all(&buf).map_err(|e| format!("写入失败: {e}"))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 产物结构核验：%PDF 头、页对象数、xref 指向正确、%%EOF 结尾。
    #[test]
    fn pdf_structure_and_pagination() {
        let long_text = "这是一段用于分页验证的正文。".repeat(400);
        let enml = format!("<en-note><p>{long_text}</p></en-note>");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("导出.pdf");
        let pages = note_to_pdf("导出标题", &enml, &out).unwrap();
        assert!(pages >= 2, "长文本应分多页（实际 {pages}）");
        let bytes = std::fs::read(&out).unwrap();
        let head = String::from_utf8_lossy(&bytes[..8]).to_string();
        assert!(head.starts_with("%PDF-1.4"), "应为 PDF 头: {head}");
        let text = String::from_utf8_lossy(&bytes);
        let page_count = text.matches("/Type /Page ").count();
        assert_eq!(page_count, pages, "页对象数应与返回一致");
        assert!(text.ends_with("%%EOF\n"));
        // startxref 指向 xref 表首字母 x
        let sx = text.rfind("startxref\n").unwrap();
        let num: usize = text[sx + 10..]
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(&bytes[num..num + 1], b"x", "startxref 应指向 xref 表");
        // 对象 1 的 xref 偏移确实指向 "1 0 obj"：定位 xref 表，
        // 跳过表头 / 计数行 / 0 号自由条目三行
        let first_obj: usize = text.find("1 0 obj").unwrap();
        let xref_start = text.find("xref\n0 ").unwrap();
        let after = &text[xref_start..];
        let mut pos = 0usize;
        for _ in 0..3 {
            pos = after[pos..].find('\n').unwrap() + pos + 1;
        }
        let entry_line = &after[pos..];
        let off: usize = entry_line[..10].parse().unwrap();
        assert_eq!(off, first_obj, "对象 1 的 xref 偏移应指向其 obj 头");
    }
}
