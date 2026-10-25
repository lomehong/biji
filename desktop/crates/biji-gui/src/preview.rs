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

/// 选区映射片段：块内一段连续纯文本 ↔ 其源文本节点。
/// `path` 为从文档根到该文本节点的子节点下标路径（首元素 = 顶层块下标；
/// 文本节点是叶子，一个文本节点恒贡献一个片段，不分裂）。
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    /// 该片段在块纯文本域中的起始 char 下标。
    pub char_start: usize,
    pub char_len: usize,
    /// 从根到文本节点的路径（`root.children` 为第 0 层）。
    pub path: Vec<usize>,
}

struct InlineCtx {
    runs: Vec<Run>,
    pieces: Vec<Piece>,
    char_total: usize,
    /// 链接区间（char 域）与目标 href：点击命中即内部导航。
    links: Vec<(usize, usize, String)>,
}

impl InlineCtx {
    fn new() -> InlineCtx {
        InlineCtx {
            runs: vec![],
            pieces: vec![],
            char_total: 0,
            links: vec![],
        }
    }

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

/// 收集行内样式段。`path` 为当前容器（children 列表）的路径：文本片段
/// 的路径 = `path ++ [子节点下标]`；元素递归携带 `path ++ [子节点下标]`。
fn collect_inline(children: &[Node], style: &Run, out: &mut InlineCtx, path: &[usize]) {
    for (i, node) in children.iter().enumerate() {
        let mut node_path = path.to_vec();
        node_path.push(i);
        match node {
            Node::Text(t) => {
                let char_start = out.char_total;
                let char_len = t.chars().count();
                out.push_text(t, style);
                out.char_total += char_len;
                if char_len > 0 {
                    out.pieces.push(Piece {
                        char_start,
                        char_len,
                        path: node_path,
                    });
                }
            }
            Node::Element(el) => collect_inline_element(el, style, out, &node_path),
        }
    }
}

fn collect_inline_element(el: &Element, style: &Run, out: &mut InlineCtx, path: &[usize]) {
    let mut s = style.clone_fields();
    s.bold = style.bold || matches!(el.name.as_str(), "b" | "strong");
    s.italic = style.italic || matches!(el.name.as_str(), "i" | "em" | "cite" | "var" | "dfn");
    s.underline = style.underline || el.name == "u" || el.name == "ins";
    s.strike = style.strike || matches!(el.name.as_str(), "s" | "strike" | "del");
    s.mono = style.mono || matches!(el.name.as_str(), "code" | "kbd" | "tt" | "samp");
    s.link = style.link || el.name == "a";
    let link_start = if el.name == "a" {
        Some(out.char_total)
    } else {
        None
    };
    if el.name == "font" {
        if let Some(v) = el.attr("size") {
            if let Ok(n) = v.parse::<f32>() {
                let factor: f32 = (1.0 + (n - 3.0) * 0.15).clamp(0.6, 1.8);
                s.size = style.size * factor;
            }
        }
    }
    collect_inline(&el.children, &s, out, path);
    if let Some(start) = link_start {
        if let Some(href) = el.attr("href") {
            let len = out.char_total.saturating_sub(start);
            if len > 0 {
                out.links.push((start, len, href.to_string()));
            }
        }
    }
}

/// 空白段不裁剪的 job 构建（渲染态选区用）：所有 run 逐段 append，
/// 保证 LayoutJob 的 char 域与片段表（`pieces`）的 char 域一一对应
/// ——裁剪空白段会造成两域错位、光标命中映射失真。
fn runs_to_job_exact(ui: &egui::Ui, runs: &[Run]) -> Option<egui::text::LayoutJob> {
    let mut job = egui::text::LayoutJob::default();
    let mut any = false;
    for run in runs {
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
            format.underline = egui::Stroke::new(1.0_f32, color);
        }
        if run.strike {
            format.strikethrough = egui::Stroke::new(1.0_f32, color);
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
            format.underline = egui::Stroke::new(1.0_f32, color);
        }
        if run.strike {
            format.strikethrough = egui::Stroke::new(1.0_f32, color);
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
    let mut ctx = InlineCtx::new();
    collect_inline(&el.children, &base_run(size), &mut ctx, &[]);
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
            let mut ctx = InlineCtx::new();
            collect_inline(&el.children, &base_run(size), &mut ctx, &[]);
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
                            let mut ctx = InlineCtx::new();
                            collect_inline(&li.children, &base_run(16.0), &mut ctx, &[]);
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
            // 行来源两种：thead/tbody/tfoot 分节包裹，或 tr 直接挂在 table 下
            // （ENML 常见裸行写法，编辑器插入的骨架即此形态）。
            let mut rows: Vec<&Element> = Vec::new();
            for child in &el.children {
                match child {
                    Node::Element(section)
                        if matches!(section.name.as_str(), "thead" | "tbody" | "tfoot") =>
                    {
                        for row in &section.children {
                            if let Node::Element(tr) = row {
                                if tr.name == "tr" {
                                    rows.push(tr);
                                }
                            }
                        }
                    }
                    Node::Element(tr) if tr.name == "tr" => rows.push(tr),
                    _ => {}
                }
            }
            for tr in rows {
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

/// 预览态行内块编辑会话 + 渲染态选区（所见即所得）。
#[derive(Debug, Default)]
pub struct PreviewSession {
    /// 正在行内编辑的顶层块下标。
    pub editing: Option<usize>,
    /// 编辑中的片段缓冲（块内 HTML）。
    pub buffer: String,
    /// 完成编辑时产出：(块下标, 新片段)。由调用方写回 ENML 树。
    pub request_commit: Option<(usize, String)>,
    /// 渲染态选区：(顶层块下标, 锚点 char, 焦点 char)——char 域为该块
    /// 片段表（`pieces`）的纯文本域，与 LayoutJob char 域一致。
    pub sel: Option<(usize, usize, usize)>,
    /// 选区所在块的片段表（该块最近一次渲染时捕获，供工具栏点击时映射）。
    pub sel_pieces: Vec<Piece>,
    /// 渲染态格式施加请求：选区 → 拆分文本节点插入标记元素。
    pub request_format: Option<FormatApply>,
    /// 点击了 biji://note/N 链接：请求打开目标笔记（调用方同帧消费）。
    pub open_note: Option<i64>,
}

/// 渲染态格式施加：对若干文本节点的 [start,end) char 区间包裹标记元素。
#[derive(Debug, Clone, PartialEq)]
pub struct FormatApply {
    /// (文本节点路径, 节点内 [start,end) char 区间)——按文档序。
    pub edits: Vec<(Vec<usize>, usize, usize)>,
    /// 包裹元素名（b/i/u/s/code/a…）。
    pub tag: String,
    /// 包裹元素属性（如链接的 href）。
    pub attrs: Vec<(String, String)>,
}

/// 就地片段编辑器的稳定 id（选区状态读写锚点）。
fn frag_editor_id() -> egui::Id {
    egui::Id::new("preview-frag-edit")
}

/// 按路径下钻到指定层级的 children 列表（root.children 为第 0 层）。
fn children_at<'a>(mut list: &'a mut Vec<Node>, path: &[usize]) -> Option<&'a mut Vec<Node>> {
    for &step in path {
        let node = list.get_mut(step)?;
        match node {
            Node::Element(el) => list = &mut el.children,
            Node::Text(_) => return None,
        }
    }
    Some(list)
}

/// 把格式施加落到 ENML 树：文本节点不能直接含标记（序列化会转义），
/// 故将命中节点拆为三个兄弟——`前缀文本` + `标记元素{选中文本}` + `后缀文本`。
/// 同一父节点内多处编辑按文档序**逆序**应用（拆分会右移后续兄弟下标）。
/// 返回成功落地的编辑数。
pub fn apply_format_edits(doc: &mut Enml, apply: &FormatApply) -> usize {
    let mut done = 0usize;
    for (path, start, end) in apply.edits.iter().rev() {
        let Some(&idx) = path.last() else { continue };
        let parent = &path[..path.len() - 1];
        // 逐层下钻到父容器（root.children 为第 0 层）
        let Some(list) = children_at(&mut doc.root.children, parent) else {
            continue;
        };
        let Some(node) = list.get_mut(idx) else { continue };
        let Node::Text(full) = node else { continue };
        let chars: Vec<char> = full.chars().collect();
        let s = (*start).min(chars.len());
        let e = (*end).max(s).min(chars.len());
        if s == e {
            continue; // 空区间不产生标记
        }
        let mut before = String::new();
        before.extend(chars[..s].iter());
        let mut selected = String::new();
        selected.extend(chars[s..e].iter());
        let mut after = String::new();
        after.extend(chars[e..].iter());
        let mut repl: Vec<Node> = Vec::with_capacity(3);
        if !before.is_empty() {
            repl.push(Node::Text(before));
        }
        repl.push(Node::Element(Element {
            name: apply.tag.clone(),
            attrs: apply.attrs.clone(),
            children: vec![Node::Text(selected)],
        }));
        if !after.is_empty() {
            repl.push(Node::Text(after));
        }
        list.splice(idx..idx + 1, repl);
        done += 1;
    }
    done
}

/// 选区 → 格式编辑列表：对每个与 [sel_start, sel_end) 相交的片段，
/// 求其节点内 char 区间（文本节点不分裂：节点内起点恒为 0）。
pub fn selection_edits(
    pieces: &[Piece],
    block_plain_len: usize,
    sel_start: usize,
    sel_end: usize,
    tag: &str,
    attrs: Vec<(String, String)>,
) -> FormatApply {
    let sel_start = sel_start.min(block_plain_len);
    let sel_end = sel_end.max(sel_start).min(block_plain_len);
    let mut edits = Vec::new();
    for p in pieces {
        let p_end = p.char_start + p.char_len;
        let s = sel_start.max(p.char_start);
        let e = sel_end.min(p_end);
        if s < e {
            edits.push((p.path.clone(), s - p.char_start, e - p.char_start));
        }
    }
    FormatApply {
        edits,
        tag: tag.to_string(),
        attrs,
    }
}

/// 格式工具栏：作用于片段缓冲当前选区（所见即所得切片——渲染态点 ✎
/// 后直接对所见文本施加标记，无需理解 HTML 源码）。
fn frag_format_toolbar(ui: &mut egui::Ui, ses: &mut PreviewSession) {
    const FORMATS: &[(&str, &str, &str)] = &[
        ("B", "<b>", "</b>"),
        ("I", "<i>", "</i>"),
        ("U", "<u>", "</u>"),
        ("S", "<s>", "</s>"),
        ("代码", "<code>", "</code>"),
        ("链接", "<a href=\"\">", "</a>"),
    ];
    let selection = egui::TextEdit::load_state(ui.ctx(), frag_editor_id())
        .as_ref()
        .and_then(|st| st.cursor.char_range());
    ui.horizontal(|ui| {
        for (label, open, close) in FORMATS {
            if ui.small_button(*label).clicked() {
                let (newc, newr) =
                    crate::util::wrap_enml_around_selection(&ses.buffer, selection, open, close);
                ses.buffer = newc;
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), frag_editor_id()) {
                    st.cursor.set_char_range(Some(newr));
                    egui::TextEdit::store_state(ui.ctx(), frag_editor_id(), st);
                }
            }
        }
        ui.label(egui::RichText::new("格式作用于选区").weak().small());
    });
}

/// 可选中富文本块（渲染态直操核心组件）：自绘 galley + 拖拽选区。
/// 光标命中经 `Galley::cursor_from_pos`（CCursor char 域与片段表一致），
/// 选区高亮按 char 逐个 `pos_from_ccursor` 取矩形填充；
/// 状态机：drag_started 置锚点 → dragged 动焦点 → clicked 清除。
/// 该块持有选区时片段表回写会话（工具栏点击时映射选区 → 文本节点编辑）。
/// 解析 biji://note/N 内部链接（对位原客户端 evernote:///view 链接）。
fn parse_note_link(href: &str) -> Option<i64> {
    let rest = href.strip_prefix("biji://note/")?;
    rest.parse().ok()
}

fn selectable_block(
    ui: &mut egui::Ui,
    block_idx: usize,
    el: &Element,
    size: f32,
    force_bold: bool,
    ses: &mut PreviewSession,
) {
    let mut ctx = InlineCtx::new();
    collect_inline(&el.children, &base_run(size), &mut ctx, &[block_idx]);
    if force_bold {
        for run in &mut ctx.runs {
            run.bold = true;
        }
    }
    let Some(job) = runs_to_job_exact(ui, &ctx.runs) else {
        ui.add_space(2.0);
        ui.add_space(6.0);
        return;
    };
    let plain_len = ctx.char_total;
    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::click_and_drag());
    // 指针 → 块内 char 下标（galley 局部坐标；越界由命中测试钳制）
    let hit = |pos: egui::Pos2| -> usize {
        galley
            .cursor_from_pos(pos - rect.min)
            .ccursor
            .index
            .min(plain_len)
    };
    let holds_sel = ses.sel.map(|(b, _, _)| b) == Some(block_idx);
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let c = hit(pos);
            ses.sel = Some((block_idx, c, c));
        }
    } else if response.dragged() && holds_sel {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some((b, anchor, _)) = ses.sel {
                ses.sel = Some((b, anchor, hit(pos)));
            }
        }
    } else if response.clicked() {
        // 单击：先看是否命中链接（biji://note/N → 内部导航，对位原
        // 客户端 evernote:// 跳转），否则清除选区
        let mut hit_link = false;
        if let Some(pos) = response.interact_pointer_pos() {
            // 链接命中按屏幕矩形（char 光标对单字链接会给出越界边界，
            // 矩形判定对长短链接都精确）
            for (start, len, href) in &ctx.links {
                let a = galley.pos_from_ccursor(egui::text::CCursor::new(*start));
                let b = galley.pos_from_ccursor(egui::text::CCursor::new(start + len));
                let r = egui::Rect::from_min_max(a.min, b.max).translate(rect.min.to_vec2());
                if r.contains(pos) {
                    if let Some(uid) = parse_note_link(href) {
                        ses.open_note = Some(uid);
                        hit_link = true;
                    }
                    break;
                }
            }
        }
        if !hit_link && holds_sel {
            ses.sel = None; // 单击清除选区
        }
    }
    // 链接悬停手型（屏幕矩形判定）
    if !ctx.links.is_empty() {
        if let Some(pos) = response.hover_pos() {
            let over_link = ctx.links.iter().any(|(st, len, _)| {
                let a = galley.pos_from_ccursor(egui::text::CCursor::new(*st));
                let b = galley.pos_from_ccursor(egui::text::CCursor::new(st + len));
                egui::Rect::from_min_max(a.min, b.max)
                    .translate(rect.min.to_vec2())
                    .contains(pos)
            });
            if over_link {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    }
    let holds_sel = ses.sel.map(|(b, _, _)| b) == Some(block_idx);
    if holds_sel {
        // 选区高亮先铺底，再叠 galley 文本
        let (a, h) = ses.sel.map(|(_, a, h)| (a, h)).unwrap();
        let (lo, hi) = (a.min(h), a.max(h));
        let painter = ui.painter().clone();
        for i in lo..hi {
            let r = galley.pos_from_ccursor(egui::text::CCursor::new(i));
            painter.rect_filled(
                r.expand(1.0),
                0.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.6),
            );
        }
        ses.sel_pieces = ctx.pieces.clone();
    }
    ui.painter()
        .galley(rect.min, galley, ui.visuals().text_color());
    ui.add_space(6.0);
}

/// 交互式渲染：可选中块（段落/标题等行内富文本）支持拖拽选区 + 选区
/// 格式工具栏（B/I/U/S/代码/链接直接作用于所见文本，写回 ENML）；
/// 每个顶层块旁带「✎」编辑按钮（块内 HTML 片段编辑，格式保留）。
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
            frag_format_toolbar(ui, ses);
            ui.add(
                egui::TextEdit::multiline(&mut ses.buffer)
                    .id(frag_editor_id())
                    .desired_width(ui.available_width()),
            );
            ui.add_space(6.0);
            continue;
        }
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                if let Node::Element(el) = child {
                    match el.name.as_str() {
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            ui.add_space(4.0);
                            selectable_block(
                                ui,
                                i,
                                el,
                                heading_size(&el.name),
                                true,
                                ses,
                            );
                        }
                        // 行内富文本块走可选中组件（渲染态直操）；
                        // 其余块级元素（列表/引用/代码块/表格/附件占位…）
                        // 保持既有渲染路径
                        "p" | "div" | "span" | "font" | "center" | "article" | "section"
                        | "header" | "footer" | "aside" | "figure" | "figcaption" | "details"
                        | "summary" | "nav" | "address" | "main" | "time" | "abbr" | "acronym"
                        | "bdo" | "bdi" | "cite" | "dfn" | "var" | "samp" | "kbd" | "tt"
                        | "label" | "legend" | "caption" | "tr" | "a" | "b" | "strong" | "i"
                        | "em" | "u" | "s" | "strike" | "del" | "ins" | "code" | "mark"
                        | "sub" | "sup" | "small" | "big" => {
                            selectable_block(ui, i, el, 16.0, false, ses);
                        }
                        _ => render_block(ui, el),
                    }
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
                    ses.sel = None; // 进编辑态清选区
                    ses.buffer = match child {
                        Node::Element(el) => element_inner_html(el),
                        Node::Text(t) => t.trim().to_string(),
                    };
                }
            });
        });
        ui.add_space(2.0);
    }
    // 渲染态选区格式工具栏：选区非空时出现（所见文本直接施加标记）
    if let Some((b, a, h)) = ses.sel {
        if a != h && ses.sel_pieces.iter().any(|p| p.path.first() == Some(&b)) {
            let lo = a.min(h);
            let hi = a.max(h);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("选区格式：")
                        .weak()
                        .small(),
                );
                const TAGS: &[(&str, &str, &str)] = &[
                    ("B", "b", ""),
                    ("I", "i", ""),
                    ("U", "u", ""),
                    ("S", "s", ""),
                    ("代码", "code", ""),
                    ("链接", "a", "href=\"\""),
                ];
                for (label, tag, attr) in TAGS {
                    if ui.small_button(*label).clicked() {
                        let attrs: Vec<(String, String)> = if attr.is_empty() {
                            Vec::new()
                        } else {
                            let (k, v) = attr.split_once('=').unwrap_or(("href", "\"\""));
                            vec![(k.to_string(), v.trim_matches('"').to_string())]
                        };
                        ses.request_format = Some(selection_edits(
                            &ses.sel_pieces,
                            ses.sel_pieces
                                .last()
                                .map(|p| p.char_start + p.char_len)
                                .unwrap_or(0),
                            lo,
                            hi,
                            tag,
                            attrs,
                        ));
                        ses.sel = None;
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 裸行表格（tr 直挂 table，编辑器「表格」按钮插入的骨架形态）应渲染出
    /// 内容行，hr 应渲染分隔线——修复前裸行被静默丢弃。
    #[test]
    fn renders_bare_tr_table_and_hr() {
        let doc = biji_enml::Enml::parse(
            "<en-note><table><tr><td>甲</td><td>乙</td></tr></table><hr/></en-note>",
        )
        .unwrap();
        let (table, hr) = match (&doc.root.children[0], &doc.root.children[1]) {
            (Node::Element(a), Node::Element(b)) => (a.clone(), b.clone()),
            _ => panic!("结构不符"),
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = ui.cursor().min.y;
                render_block(ui, &table);
                let after = ui.cursor().min.y;
                assert!(
                    after - before > 10.0,
                    "裸行表格应渲染出内容行（光标前进 {:.1}px）",
                    after - before
                );
                let b2 = ui.cursor().min.y;
                render_block(ui, &hr);
                assert!(
                    ui.cursor().min.y - b2 >= 4.0,
                    "hr 应渲染分隔线（光标前进 {:.1}px）",
                    ui.cursor().min.y - b2
                );
            });
        });
    }

    #[test]
    fn inline_styles_merge_runs() {
        let doc =
            biji_enml::Enml::parse("<en-note><p>普通<b>加粗</b>之后<i>斜体</i></p></en-note>")
                .unwrap();
        let mut ctx = InlineCtx { runs: vec![], pieces: vec![], char_total: 0, links: vec![] };
        collect_inline(&doc.root.children, &base_run(16.0), &mut ctx, &[]);
        // 普通 / 加粗 / 之后 / 斜体 —— 样式切换处切段。
        assert_eq!(ctx.runs.len(), 4);
        assert!(ctx.runs[1].bold && !ctx.runs[0].bold);
        assert!(ctx.runs[3].italic);
    }

    /// 片段表（渲染态直操的映射基座）：嵌套标记下的每个文本节点
    /// 各贡献一个片段，char 域连续、路径精确到文本节点。
    #[test]
    fn pieces_map_nested_text_nodes() {
        let doc = biji_enml::Enml::parse(
            "<en-note><p>普通<b>加粗<i>嵌套</i>尾</b>后</p></en-note>",
        )
        .unwrap();
        // 块 0 = <p>；片段收集以块为前缀路径
        let p = match &doc.root.children[0] {
            Node::Element(el) => el.clone(),
            _ => panic!(),
        };
        let mut ctx = InlineCtx::new();
        collect_inline(&p.children, &base_run(16.0), &mut ctx, &[0]);
        let got: Vec<(usize, usize, Vec<usize>)> = ctx
            .pieces
            .iter()
            .map(|pc| (pc.char_start, pc.char_len, pc.path.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 2, vec![0, 0]),   // 普通（p.children[0]）
                (2, 2, vec![0, 1, 0]), // 加粗（b.children[0]）
                (4, 2, vec![0, 1, 1, 0]), // 嵌套（i.children[0]）
                (6, 1, vec![0, 1, 2]), // 尾（b.children[2]）
                (7, 1, vec![0, 2]),   // 后（p.children[2]）
            ]
        );
        assert_eq!(ctx.char_total, 8, "块纯文本域长度");
    }

    /// 选区跨两个片段 → 每片段各产生一条节点内区间。
    #[test]
    fn selection_edits_spanning_pieces() {
        let pieces = vec![
            Piece { char_start: 0, char_len: 2, path: vec![0, 0] },
            Piece { char_start: 2, char_len: 2, path: vec![0, 1, 0] },
        ];
        let apply = selection_edits(&pieces, 4, 1, 3, "b", vec![]);
        assert_eq!(
            apply.edits,
            vec![(vec![0, 0], 1, 2), (vec![0, 1, 0], 0, 1)]
        );
    }

    /// 单文本节点中部包裹（CJK char 安全）：拆为 前 + <b>选</b> + 后。
    #[test]
    fn apply_bold_within_single_text_node() {
        let mut doc = biji_enml::Enml::parse("<en-note><p>你好世界</p></en-note>").unwrap();
        let apply = FormatApply {
            edits: vec![(vec![0, 0], 1, 3)], // 「好世」
            tag: "b".into(),
            attrs: vec![],
        };
        assert_eq!(apply_format_edits(&mut doc, &apply), 1);
        let xml = doc.to_xml();
        assert!(
            xml.contains("你<b>好世</b>界"),
            "拆分包裹失真：{xml}"
        );
    }

    /// 同一父节点内两处编辑（选区横跨兄弟文本节点）：逆序应用保证
    /// 第二处（更靠前）路径不被第一处拆分右移失真。
    #[test]
    fn apply_selection_spanning_sibling_nodes() {
        let mut doc =
            biji_enml::Enml::parse("<en-note><p>一二<b>三四</b>五六</p></en-note>").unwrap();
        // 选区 [1..5)：覆盖 一二的后半 + 三四的前半 → 两条编辑
        let pieces = vec![
            Piece { char_start: 0, char_len: 2, path: vec![0, 0] },  // 一二
            Piece { char_start: 2, char_len: 2, path: vec![0, 1, 0] }, // 三四
        ];
        let apply = selection_edits(&pieces, 4, 1, 3, "b", vec![]);
        assert_eq!(apply.edits.len(), 2);
        assert_eq!(apply_format_edits(&mut doc, &apply), 2);
        let xml = doc.to_xml();
        assert!(
            xml.contains("一<b>二</b>") && xml.contains("<b>三</b>四"),
            "跨节点包裹失真：{xml}"
        );
    }

    /// 链接标记带属性（href 空串占位）。
    #[test]
    fn apply_link_tag_with_attrs() {
        let mut doc = biji_enml::Enml::parse("<en-note><p>链接文本</p></en-note>").unwrap();
        let apply = FormatApply {
            edits: vec![(vec![0, 0], 0, 2)],
            tag: "a".into(),
            attrs: vec![("href".into(), String::new())],
        };
        assert_eq!(apply_format_edits(&mut doc, &apply), 1);
        let xml = doc.to_xml();
        assert!(xml.contains("<a href=\"\">链接</a>"), "链接包裹失真：{xml}");
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
