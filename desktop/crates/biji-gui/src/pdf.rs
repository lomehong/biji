//! PDF 图注管线（分析文档 P2：原 `markupPdf` 的分页渲染接缝）。
//!
//! 原客户端由 `pdftools.exe`/CEF 渲染 PDF 页面为位图再进入图注编辑器。
//! 本实现定义 [`PdfPageRenderer`] 渲染接缝，默认探测 poppler 的
//! `pdftoppm`（也可用环境变量 `BIJI_PDF_RENDER_CMD` 指定，参数按
//! `{pdf} {page} {out}` 替换）；未安装渲染器时给出可操作的指引，
//! 不做静默降级。
//!
//! 写回语义：PDF 本体不被改写（对位原客户端"标注层另存"），标注结果
//! 作为新 PNG 资源挂到同一条笔记，文件名 `{pdf名}-p{页码}.png`。

use std::path::{Path, PathBuf};
use std::process::Command;

use image::RgbaImage;

pub trait PdfPageRenderer {
    /// 渲染第 `page` 页（1 起）为 RGBA 位图。
    fn render_page(&self, pdf: &Path, page: u32) -> Result<RgbaImage, String>;
    fn describe(&self) -> String;
}

/// poppler `pdftoppm` 渲染器。
pub struct PopplerRenderer {
    pub program: PathBuf,
}

impl PopplerRenderer {
    pub fn new(program: PathBuf) -> PopplerRenderer {
        PopplerRenderer { program }
    }
}

impl PdfPageRenderer for PopplerRenderer {
    fn render_page(&self, pdf: &Path, page: u32) -> Result<RgbaImage, String> {
        let out_dir = std::env::temp_dir().join("biji-pdf");
        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let prefix = out_dir.join(format!(
            "biji-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let status = Command::new(&self.program)
            .args([
                "-png".as_ref(),
                "-r".as_ref(),
                "150".as_ref(),
                "-f".as_ref(),
                page.to_string().as_ref(),
                "-l".as_ref(),
                page.to_string().as_ref(),
                pdf.as_os_str(),
                prefix.as_os_str(),
            ])
            .output()
            .map_err(|e| format!("执行 {:?} 失败: {e}", self.program))?;
        if !status.status.success() {
            return Err(format!("pdftoppm 退出码 {:?}", status.status.code()));
        }
        // pdftoppm 输出 `<prefix>-<page>.png`（页码位数随文档页数变化，做宽松匹配）。
        let suffix = format!("-{page}.png");
        let prefix_name = prefix
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut found = None;
        if let Ok(entries) = std::fs::read_dir(&out_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix_name) && name.ends_with(&suffix) {
                    found = Some(e.path());
                }
            }
        }
        let Some(path) = found else {
            return Err("pdftoppm 未产出页面图片".into());
        };
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&path);
        image::load_from_memory(&bytes)
            .map(|d| d.to_rgba8())
            .map_err(|e| e.to_string())
    }

    fn describe(&self) -> String {
        format!("poppler ({})", self.program.display())
    }
}

/// 环境变量指定的自定义渲染命令：`BIJI_PDF_RENDER_CMD`，参数
/// `{pdf} {page} {out}` 按占位符替换，输出 PNG。
pub struct CommandRenderer {
    pub template: String,
}

impl PdfPageRenderer for CommandRenderer {
    fn render_page(&self, pdf: &Path, page: u32) -> Result<RgbaImage, String> {
        let out = std::env::temp_dir().join(format!(
            "biji-pdf-page{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let cmd = self
            .template
            .replace("{pdf}", &pdf.to_string_lossy())
            .replace("{page}", &page.to_string())
            .replace("{out}", &out.to_string_lossy());
        let status = Command::new("cmd")
            .args(["/C", &cmd])
            .output()
            .map_err(|e| e.to_string())?;
        if !status.status.success() {
            return Err(format!("自定义渲染命令退出码 {:?}", status.status.code()));
        }
        let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&out);
        image::load_from_memory(&bytes)
            .map(|d| d.to_rgba8())
            .map_err(|e| e.to_string())
    }

    fn describe(&self) -> String {
        format!("custom ({})", self.template)
    }
}

/// 依次探测：`BIJI_PDF_RENDER_CMD` → PATH 上的 `pdftoppm`。
pub fn detect_renderer() -> Option<Box<dyn PdfPageRenderer>> {
    if let Ok(template) = std::env::var("BIJI_PDF_RENDER_CMD") {
        if !template.trim().is_empty() {
            return Some(Box::new(CommandRenderer { template }));
        }
    }
    for name in ["pdftoppm.exe", "pdftoppm"] {
        let ok = Command::new(name)
            .arg("-v")
            .output()
            .map(|o| o.status.success() || !o.stderr.is_empty())
            .unwrap_or(false);
        if ok {
            return Some(Box::new(PopplerRenderer::new(PathBuf::from(name))));
        }
    }
    None
}

pub const RENDERER_GUIDANCE: &str =
    "PDF 渲染器未找到：安装 poppler（pdftoppm 入 PATH）或设 BIJI_PDF_RENDER_CMD（参数 {pdf} {page} {out}）";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_renderer_runs_template() {
        // 用 cmd 的 copy 命令当最简渲染器：把 fixture PNG 复制到 {out}。
        let dir = std::env::temp_dir().join("biji-pdf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture.png");
        // 1×1 白色 PNG（用本项目 encode_png 生成，保证字节流合法）
        let png = crate::annotate::encode_png(&image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 255, 255, 255]),
        ))
        .unwrap();
        std::fs::write(&fixture, png).unwrap();
        let pdf = dir.join("fake.pdf");
        std::fs::write(&pdf, b"%PDF-fake").unwrap();
        let renderer = CommandRenderer {
            template: format!("copy /Y {} {{out}}", fixture.display()),
        };
        let img = renderer.render_page(&pdf, 1).unwrap();
        assert_eq!((img.width(), img.height()), (1, 1));
    }
}
