//! ENML → egui 富文本渲染（GUI 预览模式）。
//!
//! 行内样式（粗体/斜体/下划线/代码）通过 `LayoutJob` 多 section 混排；
//! 块级元素（标题/段落/列表/引用/代码块/表格）映射为 egui 控件。
//! 链接渲染为带色下划线（v1 不可点击，见 README 已知边界）。

use biji_enml::{Element, Enml, Node};

/// 行内样式段。
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    mono: bool,
    link: bool,
    size: f32,
}

struct InlineCtx {
    runs: Vec<Run>,
}

impl InlineCtx {
    fn push_text(&mut self, text: &str, base: &Run) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut() {
            if last.same_style(base) {
                last.text.push_str(text);
                return;
            }
        }
        self.runs.push(Run {
            text: text.to_string(),
            ..base.clone_fields()
        });
    }
}

impl Run {
    fn same_style(&self, other: &Run) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
            && self.strike == other.strike
            && self.mono == other.mono
            && self.link == other.link
            && self.size == other.size
    }

    fn clone_fields(&self) -> Run {
        Run {
            text: String::new(),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            strike: self.strike,
            mono: self.mono,
            link: self.link,
            size: self.size,
        }
    }
}

fn base_run(size: f32) -> Run {
    Run {
        text: String::new(),
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        mono: false,
        link: false,
        size,
    }
}

fn collect_inline(children: &[Node], style: &Run, out: &mut InlineCtx) {
    for node in children {
        match node {
            Node::Text(t) => out.push_text(t, style),
            Node::Element(el) => collect_inline_element(el, style, out),
        }
    }
}

fn collect_inline_element(el: &Element, style: &Run, out: &mut InlineCtx) {
    let mut s = style.clone_fields();
    s.bold = style.bold || matches!(el.name.as_str(), "b" | "strong");
    s.italic = style.italic || matches!(el.name.as_str(), "i" | "em" | "cite" | "var" | "dfn");
    s.underline = style.underline || el.name == "u" || el.name == "ins";
    s.strike = style.strike || matches!(el.name.as_str(), "s" | "strike" | "del");
    s.mono = style.mono || matches!(el.name.as_str(), "code" | "kbd" | "tt" | "samp");
    s.link = style.link || el.name == "a";
    if el.name == "font" {
        if let Some(v) = el.attr("size") {
            if let Ok(n) = v.parse::<f32>() {
                let factor: f32 = (1.0 + (n - 3.0) * 0.15).clamp(0.6, 1.8);
                s.size = style.size * factor;
            }
        }
    }
    collect_inline(&el.children, &s, out);
}

fn runs_to_job(ui: &egui::Ui, runs: &[Run]) -> Option<egui::text::LayoutJob> {
    let mut job = egui::text::LayoutJob::default();
    let mut any = false;
    for run in runs {
        if run.text.trim().is_empty() && !run.text.contains('\u{a0}') {
            continue;
        }
        any = true;
        let family = if run.mono {
            egui::FontFamily::Monospace
        } else {
            egui::FontFamily::Proportional
        };
        let color = if run.link {
            ui.visuals().hyperlink_color
        } else {
            ui.visuals().text_color()
        };
        let mut format = egui::TextFormat::simple(egui::FontId::new(run.size, family), color);
        if run.underline || run.link {
            format.underline = egui::Stroke::new(1.0, color);
        }
        if run.strike {
            format.strikethrough = egui::Stroke::new(1.0, color);
        }
        format.italics = run.italic;
        job.append(&run.text, 0.0, format);
    }
    if any {
        Some(job)
    } else {
        None
    }
}

fn heading_size(name: &str) -> f32 {
    match name {
        "h1" => 26.0,
        "h2" => 22.0,
        "h3" => 19.0,
        "h4" => 17.0,
        "h5" => 16.0,
        "h6" => 15.0,
        _ => 16.0,
    }
}

fn block_paragraph(ui: &mut egui::Ui, el: &Element, size: f32) {
    let mut ctx = InlineCtx { runs: vec![] };
    collect_inline(&el.children, &base_run(size), &mut ctx);
    if let Some(job) = runs_to_job(ui, &ctx.runs) {
        ui.label(job);
    } else {
        ui.add_space(2.0);
    }
    ui.add_space(6.0);
}

/// 文本转义（序列化用转义规则与 ENML 一致）。
/// 块内 HTML 片段（格式保留型行内编辑缓冲）。
pub fn element_inner_html(el: &Element) -> String {
    biji_enml::nodes_html(&el.children)
}

fn plain_text_of(children: &[Node]) -> String {
    let mut out = String::new();
    fn walk(nodes: &[Node], out: &mut String) {
        for n in nodes {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) => walk(&e.children, out),
            }
        }
    }
    walk(children, &mut out);
    out.trim().to_string()
}

fn render_block(ui: &mut egui::Ui, el: &Element) {
    match el.name.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let size = heading_size(&el.name);
            let mut ctx = InlineCtx { runs: vec![] };
            collect_inline(&el.children, &base_run(size), &mut ctx);
            for run in &mut ctx.runs {
                run.bold = true;
            }
            if let Some(job) = runs_to_job(ui, &ctx.runs) {
                ui.add_space(4.0);
                ui.label(job);
            }
        }
        "p" | "div" | "span" | "font" | "center" | "article" | "section" | "header" | "footer"
        | "aside" | "figure" | "figcaption" | "details" | "summary" | "nav" | "address"
        | "main" | "time" | "abbr" | "acronym" | "bdo" | "bdi" | "cite" | "dfn" | "var"
        | "samp" | "kbd" | "tt" | "label" | "legend" | "caption" => {
            block_paragraph(ui, el, 16.0);
        }
        "blockquote" => {
            egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                .show(ui, |ui| {
                    for child in &el.children {
                        if let Node::Element(sub) = child {
                            render_block(ui, sub);
                        }
                    }
                });
            ui.add_space(4.0);
        }
        "pre" | "en-codeblock" => {
            let text = plain_text_of(&el.children);
            egui::Frame::default()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(egui::RichText::new(text).monospace().size(13.5));
                });
            ui.add_space(4.0);
        }
        "ul" | "ol" => {
            let ordered = el.name == "ol";
            let mut index = 1usize;
            for child in &el.children {
                if let Node::Element(li) = child {
                    if li.name != "li" {
                        continue;
                    }
                    let marker = if ordered {
                        format!("{index}.")
                    } else {
                        "•".to_string()
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(marker));
                        // li 内容按段落渲染（嵌套列表自然递归缩进）。
                        let mut inner_has_block = false;
                        for sub in &li.children {
                            if let Node::Element(se) = sub {
                                if matches!(
                                    se.name.as_str(),
                                    "ul" | "ol"
                                        | "p"
                                        | "div"
                                        | "blockquote"
                                        | "pre"
                                        | "en-codeblock"
                                        | "table"
                                ) {
                                    inner_has_block = true;
                                }
                            }
                        }
                        if inner_has_block {
                            ui.vertical(|ui| {
                                for sub in &li.children {
                                    match sub {
                                        Node::Text(t) => {
                                            if !t.trim().is_empty() {
                                                ui.label(t.trim().to_string());
                                            }
                                        }
                                        Node::Element(se) => render_block(ui, se),
                                    }
                                }
                            });
                        } else {
                            let mut ctx = InlineCtx { runs: vec![] };
                            collect_inline(&li.children, &base_run(16.0), &mut ctx);
                            if let Some(job) = runs_to_job(ui, &ctx.runs) {
                                ui.label(job);
                            }
                        }
                    });
                    index += 1;
                }
            }
        }
        "table" => {
            for child in &el.children {
                if let Node::Element(section) = child {
                    if matches!(section.name.as_str(), "thead" | "tbody" | "tfoot") {
                        for row in &section.children {
                            if let Node::Element(tr) = row {
                                if tr.name == "tr" {
                                    let cells: Vec<String> = tr
                                        .children
                                        .iter()
                                        .filter_map(|c| match c {
                                            Node::Element(td) => Some(plain_text_of(&td.children)),
                                            _ => None,
                                        })
                                        .collect();
                                    ui.label(egui::RichText::new(cells.join("  |  ")));
                                }
                            }
                        }
                    }
                }
            }
            ui.add_space(4.0);
        }
        "tr" => {
            // 裸表格行（无 table 包裹）按段落处理。
            block_paragraph(ui, el, 16.0);
        }
        "hr" => {
            ui.separator();
        }
        "br" => {
            ui.label(" ");
        }
        "img" => {
            let alt = el.attr("alt").unwrap_or("图片");
            ui.label(egui::RichText::new(format!("🖼 {alt}")).weak());
        }
        "en-media" => {
            let mime = el.attr("type").unwrap_or("未知类型");
            egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(format!("📎 附件（{mime}）")).weak());
                });
        }
        "en-crypt" => {
            ui.label(egui::RichText::new("🔒 加密内容").weak());
        }
        "a" | "b" | "strong" | "i" | "em" | "u" | "s" | "strike" | "del" | "ins" | "code"
        | "mark" | "sub" | "sup" | "small" | "big" => {
            // 块级上下文中出现的行内标签：按段落渲染。
            block_paragraph(ui, el, 16.0);
        }
        other => {
            // 未知元素：递归渲染子内容。
            let _ = other;
            for child in &el.children {
                if let Node::Element(sub) = child {
                    render_block(ui, sub);
                }
            }
        }
    }
}

/// 预览态行内块编辑会话（所见即所得最小切片：
/// 点击块旁「✎」以纯文本编辑该块，完成后该块转为纯文本段落）。
#[derive(Debug, Default)]
pub struct PreviewSession {
    /// 正在行内编辑的顶层块下标。
    pub editing: Option<usize>,
    /// 编辑中的纯文本缓冲。
    pub buffer: String,
    /// 完成编辑时产出：(块下标, 新纯文本)。由调用方写回 ENML 树。
    pub request_commit: Option<(usize, String)>,
}

/// 交互式渲染：每个顶层块旁带「✎」编辑按钮；编辑中的块显示纯文本
/// 多行编辑器与 完成/取消 按钮。块内容变更经 `request_commit` 产出。
pub fn render_interactive(ui: &mut egui::Ui, doc: &Enml, ses: &mut PreviewSession) {
    let children = doc.root.children.clone();
    for (i, child) in children.iter().enumerate() {
        if ses.editing == Some(i) {
            ui.horizontal(|ui| {
                if ui.button("✓ 完成").clicked() {
                    ses.request_commit = Some((i, ses.buffer.clone()));
                    ses.editing = None;
                }
                if ui.button("✕ 取消").clicked() {
                    ses.editing = None;
                    ses.buffer.clear();
                }
                ui.label(
                    egui::RichText::new("行内编辑（块内 HTML，格式保留）")
                        .weak()
                        .small(),
                );
            });
            ui.add(egui::TextEdit::multiline(&mut ses.buffer).desired_width(ui.available_width()));
            ui.add_space(6.0);
            continue;
        }
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                if let Node::Element(el) = child {
                    render_block(ui, el);
                } else if let Node::Text(t) = child {
                    if !t.trim().is_empty() {
                        ui.label(t.trim().to_string());
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui
                    .small_button("✎")
                    .on_hover_text("编辑此块 HTML（格式保留）")
                    .clicked()
                {
                    ses.editing = Some(i);
                    ses.buffer = match child {
                        Node::Element(el) => element_inner_html(el),
                        Node::Text(t) => t.trim().to_string(),
                    };
                }
            });
        });
        ui.add_space(2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_styles_merge_runs() {
        let doc =
            biji_enml::Enml::parse("<en-note><p>普通<b>加粗</b>之后<i>斜体</i></p></en-note>")
                .unwrap();
        let mut ctx = InlineCtx { runs: vec![] };
        collect_inline(&doc.root.children, &base_run(16.0), &mut ctx);
        // 普通 / 加粗 / 之后 / 斜体 —— 样式切换处切段。
        assert_eq!(ctx.runs.len(), 4);
        assert!(ctx.runs[1].bold && !ctx.runs[0].bold);
        assert!(ctx.runs[3].italic);
    }

    #[test]
    fn plain_text_helper_handles_nesting() {
        let doc = biji_enml::Enml::parse(
            "<en-note><pre><code>let x = 1;\nlet y = 2;</code></pre></en-note>",
        )
        .unwrap();
        let pre = match &doc.root.children[0] {
            Node::Element(e) => e,
            _ => panic!(),
        };
        assert!(plain_text_of(&pre.children).contains("let x = 1;"));
    }
}
