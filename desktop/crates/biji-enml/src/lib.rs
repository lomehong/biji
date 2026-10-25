//! ENML 2.0 笔记正文标记语言（公开规范）的解析、校验、清洗与序列化。
//!
//! 对应分析文档 §6.2（ENML DTD）与 §9.1（正文格式）：笔记正文的
//! 标准格式是 ENML——受 DTD 约束的 XHTML 子集，根元素 `en-note`，
//! 扩展元素 `en-media`（附件占位）、`en-crypt`（加密段）、`en-todo`（待办）、
//! `en-codeblock`（代码块）。
//!
//! 真实笔记常见 HTML 命名实体（`&nbsp;` 等）不是合法 XML，解析前统一
//! 预处理为数字引用。

use std::fmt;

use thiserror::Error;

pub const ENML2_DTD: &str = "http://xml.evernote.com/pub/enml2.dtd";
pub const EN_NOTE: &str = "en-note";

/// ENML2 DTD 允许的元素全集（en-* 扩展单列）。
const ALLOWED_TAGS: &[&str] = &[
    "a", "abbr", "acronym", "address", "area", "article", "aside", "b", "bdi", "bdo",
    "blockquote", "br", "button", "canvas", "caption", "center", "cite", "code", "col",
    "colgroup", "dd", "del", "details", "dfn", "div", "dl", "dt", "em", "fieldset",
    "figcaption", "figure", "font", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6",
    "header", "hr", "i", "img", "input", "ins", "kbd", "label", "legend", "li", "map",
    "mark", "menu", "nav", "ol", "p", "pre", "q", "s", "samp", "small", "span", "strike",
    "strong", "sub", "summary", "sup", "table", "tbody", "td", "textarea", "tfoot", "th",
    "thead", "time", "tr", "tt", "u", "ul", "var", "en-note", "en-crypt", "en-media",
    "en-todo", "en-codeblock",
];

/// 出现即整棵剪除（连同子树）的元素。
const STRIP_WITH_CONTENT: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "applet", "frame", "frameset",
    "noscript", "title", "head", "html", "body", "base", "link", "meta", "svg", "math",
    "xml",
];

/// 无子元素的 void 元素（序列化为自闭合）。
const VOID_TAGS: &[&str] = &["br", "hr", "img", "input", "area", "col", "en-media"];

/// `href`/`src` 允许的 URL scheme 前缀（ENML2 协议白名单的常用子集）。
const ALLOWED_SCHEMES: &[&str] = &[
    "http://", "https://", "evernote://", "mailto:", "ftp://", "tel:", "callto:",
];

#[derive(Error, Debug)]
pub enum EnmlError {
    #[error("XML 解析失败: {0}")]
    Parse(String),
    #[error("缺少 en-note 根元素（可用 parse_loose 包裹后重试）")]
    NoEnNoteRoot,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        if let Some(slot) = self
            .attrs
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            slot.1 = value.to_string();
        } else {
            self.attrs.push((name.to_string(), value.to_string()));
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Element(Element),
}

#[derive(Debug, Clone)]
pub struct Enml {
    pub root: Element,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub kind: ViolationKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// DTD 不允许的标签（清洗时会展开为子内容）。
    DisallowedTag,
    /// 连同内容剪除的标签。
    StripWithContent,
    /// href/src 的 scheme 不在白名单内。
    DisallowedScheme,
    /// on* 事件属性。
    EventAttribute,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

/// 真实笔记中高频出现的 HTML 命名实体 → 数字引用。
fn normalize_entities(input: &str) -> String {
    const NAMED: &[(&str, u32)] = &[
        ("nbsp", 160), ("copy", 169), ("reg", 174), ("trade", 8482),
        ("hellip", 8230), ("mdash", 8212), ("ndash", 8211), ("ldquo", 8220),
        ("rdquo", 8221), ("lsquo", 8216), ("rsquo", 8217), ("bull", 8226),
        ("deg", 176), ("middot", 183), ("laquo", 171), ("raquo", 187),
        ("times", 215), ("divide", 247), ("euro", 8364), ("pound", 163),
        ("yen", 165), ("cent", 162), ("sect", 167), ("para", 182),
        ("larr", 8592), ("uarr", 8593), ("rarr", 8594), ("darr", 8595),
        ("harr", 8596), ("minus", 8722), ("frac12", 189), ("sup2", 178),
        ("sup3", 179), ("micro", 181), ("plusmn", 177),
    ];
    let mut out = input.to_string();
    for (name, code) in NAMED {
        let pat = format!("&{name};");
        let rep = format!("&#{code};");
        if out.contains(&pat) {
            out = out.replace(&pat, &rep);
        }
    }
    out
}

fn is_void(name: &str) -> bool {
    VOID_TAGS.contains(&name)
}

fn is_allowed(name: &str) -> bool {
    ALLOWED_TAGS.contains(&name)
}

/// ENML 文档必带 DOCTYPE，roxmltree 默认拒绝，需显式放行。
fn parse_xml(xml: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..roxmltree::ParsingOptions::default()
        },
    )
    .ok()
}

/// 剥掉 XML 声明与 DOCTYPE（碎片包裹前清理）。
fn strip_declarations(input: &str) -> String {
    let mut s = input.trim().to_string();
    loop {
        if s.starts_with("<?xml") {
            if let Some(end) = s.find("?>") {
                s = s[end + 2..].trim_start().to_string();
                continue;
            }
        }
        if s.starts_with("<!DOCTYPE") {
            if let Some(end) = s.find('>') {
                s = s[end + 1..].trim_start().to_string();
                continue;
            }
        }
        break;
    }
    s
}

fn should_strip(name: &str) -> bool {
    STRIP_WITH_CONTENT.contains(&name)
}

fn convert_node(src: roxmltree::Node) -> Option<Node> {
    if src.is_text() {
        let text = src.text()?.to_string();
        if text.is_empty() {
            None
        } else {
            Some(Node::Text(text))
        }
    } else if src.is_element() {
        let name = src.tag_name().name().to_ascii_lowercase();
        let attrs = src
            .attributes()
            .map(|a: roxmltree::Attribute| {
                (
                    a.name().to_ascii_lowercase(),
                    a.value().to_string(),
                )
            })
            .collect();
        let children = src.children().filter_map(convert_node).collect();
        Some(Node::Element(Element {
            name,
            attrs,
            children,
        }))
    } else {
        None
    }
}

impl Enml {
    /// 严格解析：要求存在 `en-note` 根元素。
    pub fn parse(input: &str) -> Result<Self, EnmlError> {
        let normalized = normalize_entities(input);
        let doc = parse_xml(&normalized).ok_or(EnmlError::Parse("无法解析为 XML 文档".into()))?;
        if doc.root_element().tag_name().name().eq_ignore_ascii_case(EN_NOTE) {
            let root = convert_node(doc.root_element())
                .expect("en-note 根元素必然转换为 Element");
            Ok(Enml {
                root: into_element(root),
            })
        } else {
            Err(EnmlError::NoEnNoteRoot)
        }
    }

    /// 宽松解析：无 `en-note` 根时把顶层内容（或 html/body 的内容）包进新的
    /// `en-note`；碎片 HTML 先补壳再解析；完全无法解析时退化为纯文本转义。
    /// 对应真实剪藏/粘贴场景。
    pub fn parse_loose(input: &str) -> Result<Self, EnmlError> {
        if let Ok(enml) = Self::parse(input) {
            return Ok(enml);
        }
        let normalized = normalize_entities(input);
        if let Some(doc) = parse_xml(&normalized) {
            let top = doc.root_element();
            // html 文档取 body 的孩子；其他情况取顶层元素的孩子。
            let source = if top.tag_name().name().eq_ignore_ascii_case("html") {
                top.children()
                    .find(|n| {
                        n.is_element() && n.tag_name().name().eq_ignore_ascii_case("body")
                    })
                    .unwrap_or(top)
            } else {
                top
            };
            let children: Vec<Node> =
                source.children().filter_map(convert_node).collect();
            return Ok(Enml {
                root: Element {
                    name: EN_NOTE.to_string(),
                    attrs: vec![],
                    children,
                },
            });
        }
        // 碎片（多个顶层元素等）：剥掉声明后整体包进 en-note。
        let stripped = strip_declarations(&normalized);
        if let Some(doc) = parse_xml(&format!("<en-note>{stripped}</en-note>")) {
            let root =
                convert_node(doc.root_element()).expect("en-note 包裹后根元素必然存在");
            return Ok(Enml {
                root: into_element(root),
            });
        }
        Ok(Self::from_plain_text(input))
    }

    /// 纯文本快速包装：转义后按空行分段包进 `<en-note>`。
    pub fn from_plain_text(text: &str) -> Self {
        let paras: Vec<String> = text
            .split("\n\n")
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let escaped = escape_text(p.trim());
                format!("<p>{}</p>", escaped.replace('\n', "<br/>"))
            })
            .collect();
        let xml = if paras.is_empty() {
            "<en-note></en-note>".to_string()
        } else {
            format!("<en-note>{}</en-note>", paras.concat())
        };
        Self::parse(&xml).expect("from_plain_text 产出的 XML 必然合法")
    }

    /// 带 XML 声明与 DOCTYPE 的完整 ENML 文本。
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<!DOCTYPE en-note SYSTEM \"{ENML2_DTD}\">\n{}",
            serialize(&Node::Element(self.root.clone()))
        )
    }

    /// 去掉 `en-note` 壳的正文片段（用于 HTML 预览）。
    pub fn to_html_fragment(&self) -> String {
        self.root
            .children
            .iter()
            .map(serialize)
            .collect::<Vec<_>>()
            .join("")
    }

    /// 清洗：剪除禁用子树、展开未知标签为子内容、去掉 on* 事件属性。
    pub fn sanitize(&mut self) {
        sanitize_element(&mut self.root);
    }

    /// 校验：返回全部违例（不修改内容）。
    pub fn validate(&self) -> Vec<Violation> {
        let mut out = Vec::new();
        validate_element(&self.root, &mut out);
        out
    }

    /// 提取纯文本（块级元素换行）。
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        plain_text_node(&Node::Element(self.root.clone()), &mut out);
        let trimmed = out.trim_matches('\n').to_string();
        // 连续 3 个以上换行收敛为两个（段间一个空行）。
        let mut collapsed = String::with_capacity(trimmed.len());
        let mut run = 0usize;
        for ch in trimmed.chars() {
            if ch == '\n' {
                run += 1;
                if run <= 2 {
                    collapsed.push(ch);
                }
            } else {
                run = 0;
                collapsed.push(ch);
            }
        }
        collapsed
    }
}

fn into_element(node: Node) -> Element {
    match node {
        Node::Element(e) => e,
        Node::Text(_) => Element {
            name: EN_NOTE.to_string(),
            attrs: vec![],
            children: vec![node],
        },
    }
}

/// 序列化节点子树为 HTML 片段（格式保留型行内块编辑的回写前置）。
pub fn nodes_html(nodes: &[Node]) -> String {
    nodes.iter().map(serialize).collect()
}

fn serialize(node: &Node) -> String {
    match node {
        Node::Text(t) => escape_text(t),
        Node::Element(e) => {
            let attrs = e
                .attrs
                .iter()
                .map(|(k, v)| format!(" {}=\"{}\"", k, escape_attr(v)))
                .collect::<String>();
            if e.children.is_empty() {
                if is_void(&e.name) || matches!(e.name.as_str(), "en-todo") {
                    format!("<{0}{attrs}/>", e.name)
                } else {
                    format!("<{0}{attrs}></{0}>", e.name)
                }
            } else {
                let inner: String = e.children.iter().map(serialize).collect();
                format!("<{0}{attrs}>{inner}</{0}>", e.name)
            }
        }
    }
}

pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sanitize_element(el: &mut Element) {
    el.attrs.retain(|(k, _)| !k.to_ascii_lowercase().starts_with("on"));
    let mut new_children = Vec::with_capacity(el.children.len());
    for child in el.children.drain(..) {
        match child {
            Node::Text(_) => new_children.push(child),
            Node::Element(mut sub) => {
                if should_strip(&sub.name) {
                    continue; // 整棵剪除
                }
                sanitize_element(&mut sub);
                if is_allowed(&sub.name) {
                    new_children.push(Node::Element(sub));
                } else {
                    // 未知标签：展开为子内容保留文本。
                    new_children.extend(sub.children);
                }
            }
        }
    }
    el.children = new_children;
}

fn validate_element(el: &Element, out: &mut Vec<Violation>) {
    if !is_allowed(&el.name) {
        let kind = if should_strip(&el.name) {
            ViolationKind::StripWithContent
        } else {
            ViolationKind::DisallowedTag
        };
        out.push(Violation {
            kind,
            detail: format!("<{}>", el.name),
        });
    }
    for (k, v) in &el.attrs {
        if k.to_ascii_lowercase().starts_with("on") {
            out.push(Violation {
                kind: ViolationKind::EventAttribute,
                detail: format!("<{} {k}>", el.name),
            });
        }
        if (k.eq_ignore_ascii_case("href") || k.eq_ignore_ascii_case("src"))
            && !ALLOWED_SCHEMES.iter().any(|s| v.to_ascii_lowercase().starts_with(s))
        {
            // 形如 "#anchor"、相对路径按违例记（与 DTD 白名单一致）。
            out.push(Violation {
                kind: ViolationKind::DisallowedScheme,
                detail: format!("<{} {k}=\"{v}\">", el.name),
            });
        }
    }
    for child in &el.children {
        if let Node::Element(sub) = child {
            validate_element(sub, out);
        }
    }
}

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "li", "tr", "ul", "ol", "dl", "table", "blockquote", "pre", "h1", "h2",
    "h3", "h4", "h5", "h6", "address", "article", "aside", "details", "figure",
    "figcaption", "footer", "header", "summary", "en-codeblock",
];

fn plain_text_node(node: &Node, out: &mut String) {
    match node {
        Node::Text(t) => out.push_str(t),
        Node::Element(e) => {
            let block = BLOCK_TAGS.contains(&e.name.as_str());
            if e.name == "br" {
                out.push('\n');
                return;
            }
            if e.name == "img" {
                if let Some(alt) = e.attr("alt") {
                    out.push_str(alt);
                }
                return;
            }
            if block {
                out.push('\n');
            }
            for child in &e.children {
                plain_text_node(child, out);
            }
            if e.name == "en-todo" {
                let checked = e.attr("checked").map(|v| v == "true").unwrap_or(false);
                out.push_str(if checked { "[x] " } else { "[ ] " });
            }
            if block {
                out.push('\n');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note><h1>标题</h1><p>第一段&nbsp;文字</p><en-todo checked="true"/>完成<br/><en-media hash="abc123" type="image/png"/></en-note>"#;

    #[test]
    fn parse_strict_roundtrip() {
        let enml = Enml::parse(SAMPLE).unwrap();
        assert_eq!(enml.root.name, "en-note");
        // h1、p、en-todo、文本"完成"、br、en-media
        assert_eq!(enml.root.children.len(), 6);
        let again = Enml::parse(&enml.to_xml()).unwrap();
        assert_eq!(again.root, enml.root);
    }

    #[test]
    fn entity_nbsp_normalized() {
        let enml = Enml::parse(SAMPLE).unwrap();
        let text = enml.to_plain_text();
        assert!(text.contains("第一段\u{a0}文字"), "实际: {text}");
    }

    #[test]
    fn plain_text_structure() {
        let enml = Enml::parse(SAMPLE).unwrap();
        let text = enml.to_plain_text();
        assert!(text.starts_with("标题"));
        assert!(text.contains("完成"));
    }

    #[test]
    fn sanitize_strips_script_keeps_text() {
        let mut enml = Enml::parse_loose(
            "<p>前</p><script>alert('x')</script><style>.x{}</style><p>后</p>",
        )
        .unwrap();
        enml.sanitize();
        let text = enml.to_plain_text();
        assert!(text.contains("前") && text.contains("后"));
        assert!(!text.contains("alert"));
        assert!(enml.validate().is_empty());
    }

    #[test]
    fn sanitize_unwraps_unknown_tags() {
        let mut enml = Enml::parse_loose("<p><custom-tag>内容</custom-tag></p>").unwrap();
        enml.sanitize();
        assert!(enml.to_plain_text().contains("内容"));
        assert!(enml.validate().is_empty());
    }

    #[test]
    fn validate_reports_disallowed() {
        let enml = Enml::parse_loose(
            r#"<p><a href="javascript:alert(1)">坏链接</a><b onclick="x()">加粗</b></p>"#,
        )
        .unwrap();
        let violations = enml.validate();
        assert!(violations
            .iter()
            .any(|v| v.kind == ViolationKind::DisallowedScheme));
        assert!(violations
            .iter()
            .any(|v| v.kind == ViolationKind::EventAttribute));
    }

    #[test]
    fn parse_loose_wraps_fragment() {
        let enml = Enml::parse_loose("<p>正文</p>").unwrap();
        assert_eq!(enml.root.name, "en-note");
        assert_eq!(enml.root.children.len(), 1);
    }

    #[test]
    fn parse_loose_takes_html_body() {
        let enml =
            Enml::parse_loose("<html><head><title>t</title></head><body><p>正文</p></body></html>")
                .unwrap();
        assert_eq!(enml.root.name, "en-note");
        let text = enml.to_plain_text();
        assert!(text.contains("正文"));
        assert!(!text.contains("<title>"));
    }

    #[test]
    fn from_plain_text_roundtrip() {
        let enml = Enml::from_plain_text("第一段\n第二行\n\n第二段");
        assert!(enml.to_xml().contains("<p>第一段<br/>第二行</p>"));
        assert!(enml.to_plain_text().contains("第二段"));
    }

    #[test]
    fn media_and_todo_attrs_preserved() {
        let enml = Enml::parse(SAMPLE).unwrap();
        let xml = enml.to_xml();
        assert!(xml.contains(r#"<en-media hash="abc123" type="image/png"/>"#));
        assert!(xml.contains(r#"<en-todo checked="true"/>"#));
    }
}
