//! PDF 分页渲染（文件产物版）：DesktopService `renderPage`/`renderPages` 的
//! 渲染接缝（分析文档 §5.2「分页渲染（PDF）」）。
//!
//! 与 GUI 标注画布的内存渲染（`biji-gui::pdf`）共享同一外部渲染器约定：
//! 优先环境变量 `BIJI_PDF_RENDER_CMD`（参数按 `{pdf} {page} {out}` 替换），
//! 否则探测 PATH 上的 poppler `pdftoppm`。本模块产出 **PNG 文件路径**
//! （服务层语义：编辑器按 URL 引用渲染产物），不做内存解码。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 页面渲染到文件的接缝（`renderPage` 的可注入实现）。
pub trait PdfPageFileRenderer {
    /// 渲染 `pdf` 的第 `page` 页（1 起）到 `out_png`；`scale_to` > 0 时
    /// 按长边缩放（对位 pdftoppm `-scale-to`）。
    fn render_to_file(&self, pdf: &Path, page: u32, scale_to: i32, out_png: &Path)
        -> Result<(), String>;
    fn describe(&self) -> String;
}

/// poppler `pdftoppm` 文件渲染器。
pub struct PopplerFileRenderer {
    pub program: PathBuf,
}

impl PdfPageFileRenderer for PopplerFileRenderer {
    fn render_to_file(
        &self,
        pdf: &Path,
        page: u32,
        scale_to: i32,
        out_png: &Path,
    ) -> Result<(), String> {
        let out_dir = out_png.parent().ok_or("输出路径缺父目录")?;
        let prefix = out_png.with_extension(""); // pdftoppm 自行追加页码后缀
        let mut cmd = Command::new(&self.program);
        cmd.arg("-png").arg("-r").arg("150");
        if scale_to > 0 {
            cmd.arg("-scale-to").arg(scale_to.to_string());
        }
        cmd.arg("-f").arg(page.to_string());
        cmd.arg("-l").arg(page.to_string());
        cmd.arg(pdf.as_os_str()).arg(prefix.as_os_str());
        let status = cmd
            .output()
            .map_err(|e| format!("执行 {:?} 失败: {e}", self.program))?;
        if !status.status.success() {
            return Err(format!("pdftoppm 退出码 {:?}", status.status.code()));
        }
        // pdftoppm 产出 `<prefix>-<page>.png`（页码位数随文档页数变化，宽松匹配）
        let prefix_name = prefix
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let suffix = format!("-{page}.png");
        let mut found: Option<PathBuf> = None;
        if let Ok(entries) = std::fs::read_dir(out_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix_name) && name.ends_with(&suffix) {
                    found = Some(e.path());
                }
            }
        }
        let src = found.ok_or("pdftoppm 未产出页面图片")?;
        if src != out_png {
            std::fs::rename(&src, out_png).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!("poppler ({})", self.program.display())
    }
}

/// 环境变量指定的自定义渲染命令：`BIJI_PDF_RENDER_CMD`，
/// 参数 `{pdf} {page} {out}` 按占位符替换，产出 PNG 写到 `{out}`。
pub struct CommandTemplateFileRenderer {
    pub template: String,
}

impl PdfPageFileRenderer for CommandTemplateFileRenderer {
    fn render_to_file(
        &self,
        pdf: &Path,
        page: u32,
        _scale_to: i32,
        out_png: &Path,
    ) -> Result<(), String> {
        let cmd = self
            .template
            .replace("{pdf}", &pdf.to_string_lossy())
            .replace("{page}", &page.to_string())
            .replace("{out}", &out_png.to_string_lossy());
        let status = Command::new("cmd")
            .args(["/C", &cmd])
            .output()
            .map_err(|e| e.to_string())?;
        if !status.status.success() {
            return Err(format!("自定义渲染命令退出码 {:?}", status.status.code()));
        }
        if !out_png.exists() {
            return Err("渲染命令未产出 {out} 文件".into());
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!("custom ({})", self.template)
    }
}

/// 依次探测：`BIJI_PDF_RENDER_CMD` → PATH 上的 `pdftoppm`。
pub fn detect_file_renderer() -> Option<Box<dyn PdfPageFileRenderer>> {
    if let Ok(template) = std::env::var("BIJI_PDF_RENDER_CMD") {
        if !template.trim().is_empty() {
            return Some(Box::new(CommandTemplateFileRenderer { template }));
        }
    }
    for name in ["pdftoppm.exe", "pdftoppm"] {
        let ok = Command::new(name)
            .arg("-v")
            .output()
            .map(|o| o.status.success() || !o.stderr.is_empty())
            .unwrap_or(false);
        if ok {
            return Some(Box::new(PopplerFileRenderer {
                program: PathBuf::from(name),
            }));
        }
    }
    None
}

pub const RENDERER_GUIDANCE: &str =
    "PDF 渲染器未找到：安装 poppler（pdftoppm 入 PATH）或设 BIJI_PDF_RENDER_CMD（参数 {pdf} {page} {out}）";

#[cfg(test)]
mod tests {
    use super::*;

    /// 用 cmd copy 当最简渲染器：把 fixture PNG 复制到 {out}。
    #[test]
    fn command_template_renderer_produces_file() {
        let dir = std::env::temp_dir().join("biji-svc-pdf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture.png");
        std::fs::write(&fixture, b"\x89PNG-fake-bytes").unwrap();
        let pdf = dir.join("fake.pdf");
        std::fs::write(&pdf, b"%PDF-fake").unwrap();
        let out = dir.join("page-1.png");
        let renderer = CommandTemplateFileRenderer {
            template: format!("copy /Y {} {{out}}", fixture.display()),
        };
        renderer.render_to_file(&pdf, 1, 0, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG-fake-bytes");
    }

    #[test]
    fn missing_output_is_error() {
        let dir = std::env::temp_dir().join("biji-svc-pdf-test2");
        std::fs::create_dir_all(&dir).unwrap();
        let renderer = CommandTemplateFileRenderer {
            template: "exit /B 0".into(), // 成功退出但不产出 {out}
        };
        let out = dir.join("never.png");
        let err = renderer
            .render_to_file(&dir.join("x.pdf"), 1, 0, &out)
            .unwrap_err();
        assert!(err.contains("未产出"), "got: {err}");
    }
}
