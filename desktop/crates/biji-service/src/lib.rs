//! 服务层：把原客户端的双 Thrift 服务语义映射为 Rust 接口。
//!
//! 对应分析文档 §5（Thrift IPC 协议完整规范）：
//! - `DesktopService`（原 `\\.\pipe\yxmarkup`，编辑器→原生，28 方法）→
//!   [`NoteService`] 的固有方法；
//! - `NodeWebkitService`（原壳↔编辑器 NW 命名管道服务，原生→编辑器，15 方法）→
//!   [`NodeWebkitCommand`] 枚举 + [`MarkupFrontend`] trait（原生侧主动派发命令）；
//! - 原编辑器对壳的窗口状态回报 → [`DesktopEvent`] 事件流。
//!
//! 传输形态从"命名管道 + Thrift"收敛为进程内调用，但方法集与参数语义
//! 与原协议一一对应（URI scheme 自定义：`biji://note/<uid>`、
//! `biji://resource/<uid>`，对位原 `resourceUrl`/`noteLink`）。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::process::Command;

pub mod pdf;

use base64::Engine as _;
use biji_store::{
    ListNotes, NoteDraft, NoteFull, Store, StoreError,
};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ServiceError>;

#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("记录不存在: uid={0}")]
    NotFound(i64),
    #[error("URI 无法解析: {0}")]
    BadUri(String),
    #[error("暂不支持: {0}")]
    Unsupported(&'static str),
    #[error("笔记已被锁定: uid={0}")]
    LockHeld(i64),
    #[error("存储层错误: {0}")]
    Store(#[from] StoreError),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("ENML 错误: {0}")]
    Enml(String),
    #[error("PDF 渲染错误: {0}")]
    Render(String),
}

// ---------- 协议数据结构（对位原 Thrift struct） ----------

/// 原 `NwNote`。
#[derive(Debug, Clone, PartialEq)]
pub struct NoteView {
    pub id: i64,
    pub title: String,
    pub source: Option<String>,
    /// ENML 正文。
    pub html: String,
    pub content_class: Option<String>,
    pub source_url: Option<String>,
    pub author: Option<String>,
}

/// 原 `NwResource`。
#[derive(Debug, Clone)]
pub struct ResourceData {
    pub uid: i64,
    pub mime: String,
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl ResourceData {
    /// 原 `NwResourceBase64`。
    pub fn to_base64_variant(&self) -> Base64Resource {
        Base64Resource {
            uid: self.uid,
            mime: self.mime.clone(),
            filename: self.filename.clone(),
            base64: base64::engine::general_purpose::STANDARD.encode(&self.bytes),
        }
    }
}

/// 原 `NwResourceBase64`。
#[derive(Debug, Clone, PartialEq)]
pub struct Base64Resource {
    pub uid: i64,
    pub mime: String,
    pub filename: String,
    pub base64: String,
}

/// 原 `Display`（多显示器信息）。
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    pub id: i32,
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub dpi: i32,
}

/// 用户信息（原 `getCurrentUserInfo` 返回值）。
#[derive(Debug, Clone, PartialEq)]
pub struct UserInfo {
    pub username: String,
    pub service_host: String,
    pub email: Option<String>,
}

/// 业务开关（原 `getAnalyticsInfo` 返回值；默认值对齐自有后端部署）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticsInfo {
    pub service_host: String,
    pub marketing_url: String,
    pub support_url: String,
    pub enable_shared_notebooks: bool,
    pub enable_single_note_sharing: bool,
    pub enable_gift_subscriptions: bool,
    pub enable_business: bool,
}

impl Default for AnalyticsInfo {
    fn default() -> Self {
        AnalyticsInfo {
            service_host: "server.biji".into(),
            marketing_url: "https://github.com/lomehong/biji".into(),
            support_url: "https://github.com/lomehong/biji/issues".into(),
            enable_shared_notebooks: true,
            enable_single_note_sharing: false,
            enable_gift_subscriptions: false,
            enable_business: false,
        }
    }
}

// ---------- URI 约定 ----------

pub const NOTE_LINK_PREFIX: &str = "biji://note/";
pub const RESOURCE_URL_PREFIX: &str = "biji://resource/";

pub fn note_link(uid: i64) -> String {
    format!("{NOTE_LINK_PREFIX}{uid}")
}

pub fn parse_note_link(link: &str) -> Option<i64> {
    link.strip_prefix(NOTE_LINK_PREFIX)?.parse().ok()
}

pub fn resource_url(uid: i64) -> String {
    format!("{RESOURCE_URL_PREFIX}{uid}")
}

pub fn parse_resource_url(url: &str) -> Option<i64> {
    url.strip_prefix(RESOURCE_URL_PREFIX)?.parse().ok()
}

// ---------- NodeWebkitService：原生 → 编辑器命令（15 个，枚举化） ----------

#[derive(Debug, Clone, PartialEq)]
pub enum NodeWebkitCommand {
    Markup {
        resource_url: String,
        title: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        trigger: String,
    },
    MarkupPdf {
        pdf_url: String,
        manifest_url: String,
        page_num: i32,
        title: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        trigger: String,
    },
    FocusMarkupWindow,
    CloseMarkupWindow,
    CloseMarkupWindowWithMessage(String),
    RotateImage { resource_url: String, theta: f64 },
    ResizeImage { resource_url: String, width: i32, height: i32 },
    ClearImage { resource_url: String },
    Present {
        note_ids: Vec<i64>,
        index: usize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        trigger: String,
    },
    FocusPresentationWindow,
    ClosePresentationWindow,
    MinimizeWindow,
    RestoreWindow,
    UpdateDisplayInfo(Vec<DisplayInfo>),
    Quit,
}

/// 编辑器前端需实现的命令入口（对位原壳↔编辑器 NW 命名管道服务端）。
pub trait MarkupFrontend {
    fn dispatch(&mut self, cmd: NodeWebkitCommand);
}

/// CLI 与测试用前端：记录命令流。
#[derive(Debug, Default)]
pub struct LoggingFrontend {
    pub received: Vec<NodeWebkitCommand>,
}

impl MarkupFrontend for LoggingFrontend {
    fn dispatch(&mut self, cmd: NodeWebkitCommand) {
        self.received.push(cmd);
    }
}

// ---------- DesktopService：编辑器 → 原生（28 方法） ----------

/// 编辑器对壳的回报事件（原 DesktopService 中的窗口状态/握手类方法）。
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopEvent {
    ServerStarted(String),
    LogMessage(String),
    MarkupWindowClosed(String),
    MarkupWindowFocused(String),
    MarkupWindowActive(String),
    PresentationWindowClosed,
    ShowPremiumPage(String),
}

/// 用系统默认程序打开附件的接缝（测试注入记录器）。
pub type OpenTarget = Box<dyn Fn(&str) -> std::io::Result<()>>;

/// Windows 默认实现：落盘临时文件后调用 `start`。
pub fn system_opener(path: &str) -> std::io::Result<()> {
    Command::new("cmd").args(["/C", "start", "", path]).spawn()?.wait().map(|_| ())
}

pub struct NoteService {
    store: Store,
    frontend: Box<dyn MarkupFrontend>,
    locks: RefCell<HashSet<i64>>,
    events: RefCell<Vec<DesktopEvent>>,
    opener: OpenTarget,
    /// 分段写回的段装配缓冲（createNoteFromResource：调用方 resource uid → 已装配字节）。
    resource_assemblies: RefCell<HashMap<i64, Vec<u8>>>,
    /// createNoteFromResource 的幂等登记（resource uid → (note uid, resource uid)）。
    created_from_resource: RefCell<HashMap<i64, (i64, i64)>>,
    /// 注入的 PDF 渲染器（测试用）；None 时按环境约定探测。
    pdf_renderer: RefCell<Option<std::rc::Rc<dyn pdf::PdfPageFileRenderer>>>,
}

impl NoteService {
    pub fn new(store: Store, frontend: Box<dyn MarkupFrontend>) -> NoteService {
        NoteService {
            store,
            frontend,
            locks: RefCell::new(HashSet::new()),
            events: RefCell::new(Vec::new()),
            opener: Box::new(system_opener),
            resource_assemblies: RefCell::new(HashMap::new()),
            created_from_resource: RefCell::new(HashMap::new()),
            pdf_renderer: RefCell::new(None),
        }
    }

    pub fn with_opener(mut self, opener: OpenTarget) -> NoteService {
        self.opener = opener;
        self
    }

    /// 注入 PDF 渲染器（测试）；未注入时按环境约定探测。
    pub fn with_pdf_renderer(
        mut self,
        renderer: std::rc::Rc<dyn pdf::PdfPageFileRenderer>,
    ) -> NoteService {
        self.pdf_renderer = RefCell::new(Some(renderer));
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn take_events(&self) -> Vec<DesktopEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn push_event(&self, event: DesktopEvent) {
        self.events.borrow_mut().push(event);
    }

    fn note_full(&self, note_id: i64) -> Result<NoteFull> {
        self.store.get_note(note_id).map_err(|e| match e {
            StoreError::NotFound(id) => ServiceError::NotFound(id),
            other => ServiceError::Store(other),
        })
    }

    // ----- 生命周期 / 日志 -----

    pub fn server_started(&self, server_name: &str) {
        self.push_event(DesktopEvent::ServerStarted(server_name.to_string()));
    }

    pub fn log_message(&self, message: &str) {
        self.push_event(DesktopEvent::LogMessage(message.to_string()));
    }

    // ----- 用户 / 业务开关 -----

    pub fn get_current_user_info(&self) -> Result<UserInfo> {
        Ok(UserInfo {
            username: self
                .store
                .meta_get("user")
                .unwrap_or_else(|| "local".into()),
            service_host: self
                .store
                .meta_get("service_host")
                .unwrap_or_else(|| "server.biji".into()),
            email: self.store.meta_get("email"),
        })
    }

    /// 账号写入（复刻原 `user#service_host` 账号模型，分析文档 §6.1）。
    pub fn set_account(&self, username: &str, service_host: &str, email: Option<&str>) -> Result<()> {
        self.store.meta_set("user", username)?;
        self.store.meta_set("service_host", service_host)?;
        if let Some(mail) = email {
            self.store.meta_set("email", mail)?;
        }
        Ok(())
    }

    pub fn get_analytics_info(&self) -> Result<AnalyticsInfo> {
        Ok(AnalyticsInfo::default())
    }

    // ----- 附件资源 -----

    fn resolve_resource(&self, url: &str) -> Result<i64> {
        parse_resource_url(url).ok_or_else(|| ServiceError::BadUri(url.to_string()))
    }

    pub fn get_resource(&self, url: &str) -> Result<ResourceData> {
        let uid = self.resolve_resource(url)?;
        let (meta, bytes) = self.store.get_resource(uid)?;
        Ok(ResourceData {
            uid: meta.uid,
            mime: meta.mime,
            filename: meta.file_name,
            bytes,
        })
    }

    pub fn get_resources(&self, note_id: i64) -> Result<Vec<(String, ResourceData)>> {
        let mut out = Vec::new();
        for meta in self.store.list_resources(note_id)? {
            let (m, bytes) = self.store.get_resource(meta.uid)?;
            out.push((
                resource_url(m.uid),
                ResourceData {
                    uid: m.uid,
                    mime: m.mime,
                    filename: m.file_name,
                    bytes,
                },
            ));
        }
        Ok(out)
    }

    pub fn get_resource_as_base64(&self, url: &str) -> Result<Base64Resource> {
        self.get_resource(url).map(|r| r.to_base64_variant())
    }

    pub fn get_resources_as_base64(&self, note_id: i64) -> Result<Vec<(String, Base64Resource)>> {
        Ok(self
            .get_resources(note_id)?
            .into_iter()
            .map(|(url, r)| (url, r.to_base64_variant()))
            .collect())
    }

    /// 用系统默认程序打开附件（Windows 上经临时文件 + `start`）。
    pub fn open_resource(&self, url: &str) -> Result<()> {
        let resource = self.get_resource(url)?;
        let dir = tempfile_dir()?;
        let safe_name: String = resource
            .filename
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' })
            .collect();
        let path = dir.join(format!("biji-{safe_name}"));
        std::fs::write(&path, &resource.bytes)?;
        (self.opener)(&path.to_string_lossy())
            .map_err(|_| ServiceError::Unsupported("系统打开失败"))?;
        Ok(())
    }

    /// 写回附件。原协议的 `offset` 定位写入位置（分析文档 §5.2
    /// 「写回附件（offset 定位）」）：offset=0 为整体替换（v0 语义）；
    /// offset>0 为分段写——`resource.bytes` 拼接进现有内容的 `offset` 处
    /// （越界补零、末端自然延长）后整体落库。
    pub fn update_resource(&self, resource: ResourceData, offset: u64) -> Result<()> {
        let uid = self.resolve_resource(&resource_url(resource.uid))?;
        if offset == 0 {
            self.store.replace_resource_bytes(uid, &resource.bytes)?;
            return Ok(());
        }
        let (_, current) = self.store.get_resource(uid)?;
        let mut content = current;
        let start = offset as usize;
        let end = start + resource.bytes.len();
        if content.len() < end {
            content.resize(end, 0); // 段间空隙补零（offset 定位语义）
        }
        content[start..end].copy_from_slice(&resource.bytes);
        self.store.replace_resource_bytes(uid, &content)?;
        Ok(())
    }

    /// 由附件新建笔记：正文为指向该资源的 `en-media` 占位（对位原语义）。
    /// `offset>0` 为分段装配：段按 offset 拼进装配缓冲，每次调用后以当前
    /// 装配结果 upsert 笔记（同一 resource uid 幂等指向同一条笔记与资源）。
    pub fn create_note_from_resource(
        &self,
        resource: ResourceData,
        offset: u64,
    ) -> Result<i64> {
        // 段装配：offset 定位段在整体内容中的位置
        {
            let mut assemblies = self.resource_assemblies.borrow_mut();
            let buf = assemblies.entry(resource.uid).or_default();
            let start = offset as usize;
            let end = start + resource.bytes.len();
            if buf.len() < end {
                buf.resize(end, 0);
            }
            buf[start..end].copy_from_slice(&resource.bytes);
        }
        let assembled = self
            .resource_assemblies
            .borrow()
            .get(&resource.uid)
            .cloned()
            .unwrap_or_default();

        // 幂等 upsert：后续段只替换既有资源内容，不另建笔记
        if let Some(&(note_uid, res_uid)) = self.created_from_resource.borrow().get(&resource.uid)
        {
            self.store.replace_resource_bytes(res_uid, &assembled)?;
            return Ok(note_uid);
        }
        let note_uid = self.store.create_note(&NoteDraft {
            title: resource.filename.clone(),
            enml: format!(
                "<en-note><en-media hash=\"{}\" type=\"{}\"/></en-note>",
                short_hash(&assembled),
                escape_attr_min(&resource.mime),
            ),
            author: None,
            source: None,
            source_url: None,
            content_class: None,
            notebook_uid: None,
        })?;
        let res_uid = self.store.add_resource(
            note_uid,
            &resource.filename,
            &resource.mime,
            &assembled,
        )?;
        self.created_from_resource
            .borrow_mut()
            .insert(resource.uid, (note_uid, res_uid));
        Ok(note_uid)
    }

    /// PDF 分页渲染（文件产物版）：第 `page_num` 页渲染为 PNG 写入临时
    /// 目录，返回 PNG 文件路径（编辑器按 URL 引用渲染产物）。
    /// `width`/`height` 取长边作为渲染缩放目标；渲染器探测与 GUI 标注画布
    /// 共享同一约定（`BIJI_PDF_RENDER_CMD` → PATH 上的 pdftoppm）。
    pub fn render_page(
        &self,
        resource_url: &str,
        _offset: u64,
        width: i32,
        height: i32,
        page_num: i32,
    ) -> Result<String> {
        let uid = self.resolve_resource(resource_url)?;
        let (_, bytes) = self.store.get_resource(uid)?;
        let renderer: std::rc::Rc<dyn pdf::PdfPageFileRenderer> = self
            .pdf_renderer
            .borrow()
            .as_ref()
            .map(std::rc::Rc::clone)
            .or_else(|| pdf::detect_file_renderer().map(std::rc::Rc::from))
            .ok_or(ServiceError::Unsupported(pdf::RENDERER_GUIDANCE))?;
        let dir = tempfile_dir()?;
        let pdf_path = dir.join(format!("biji-render-{uid}.pdf"));
        std::fs::write(&pdf_path, &bytes)?;
        let out_png = dir.join(format!(
            "biji-render-{uid}-p{page_num}-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let scale_to = width.max(height);
        renderer
            .render_to_file(&pdf_path, page_num.max(1) as u32, scale_to, &out_png)
            .map_err(ServiceError::Render)?;
        Ok(out_png.to_string_lossy().into_owned())
    }

    /// 批量渲染：多个 PDF 附件各渲染首页，返回 PNG 路径列表（与单个
    /// 调用方约定的顺序一致）。
    pub fn render_pages(&self, urls: &[String]) -> Result<Vec<String>> {
        urls.iter()
            .map(|url| self.render_page(url, 0, 0, 0, 1))
            .collect()
    }

    // ----- 笔记 -----

    pub fn get_note_id(&self, note_link: &str) -> Result<i64> {
        parse_note_link(note_link).ok_or_else(|| ServiceError::BadUri(note_link.to_string()))
    }

    pub fn get_note_title(&self, note_id: i64) -> Result<String> {
        Ok(self.note_full(note_id)?.note.title)
    }

    pub fn is_note_locked(&self, note_id: i64) -> bool {
        self.locks.borrow().contains(&note_id)
    }

    pub fn request_note_lock(&self, note_id: i64) -> Result<bool> {
        let mut locks = self.locks.borrow_mut();
        if locks.contains(&note_id) {
            return Err(ServiceError::LockHeld(note_id));
        }
        locks.insert(note_id);
        Ok(true)
    }

    pub fn release_note_lock(&self, note_id: i64) {
        self.locks.borrow_mut().remove(&note_id);
    }

    pub fn get_note(&self, note_id: i64) -> Result<NoteView> {
        let full = self.note_full(note_id)?;
        Ok(NoteView {
            id: full.note.uid,
            title: full.note.title,
            source: full.note.source,
            html: full.enml,
            content_class: full.note.content_class,
            source_url: full.note.source_url,
            author: full.note.author,
        })
    }

    /// 写回编辑器产出的笔记（标题 + 正文）。
    pub fn update_note(&self, note: &NoteView) -> Result<()> {
        self.store.update_note_meta(note.id, Some(&note.title), note.author.as_deref(), note.source_url.as_deref())?;
        self.store.update_note_content(note.id, &note.html)?;
        Ok(())
    }

    pub fn create_note(&self, title: &str, enml: &str, notebook_uid: Option<i64>) -> Result<i64> {
        // 入库前清洗，保证库内正文始终是合法 ENML（对位原 RTE 约束）。
        let mut doc = biji_enml::Enml::parse_loose(enml)
            .map_err(|e| ServiceError::Enml(e.to_string()))?;
        if title.is_empty() {
            // 无标题时用纯文本首行兜底（对位原客户端行为）。
            let text = doc.to_plain_text();
            let first_line = text.lines().next().unwrap_or("无标题").to_string();
            let title = first_line.chars().take(50).collect::<String>();
            let uid = self.store.create_note(&NoteDraft {
                title,
                enml: doc.to_xml(),
                notebook_uid,
                ..Default::default()
            })?;
            return Ok(uid);
        }
        doc.sanitize();
        let uid = self.store.create_note(&NoteDraft {
            title: title.to_string(),
            enml: doc.to_xml(),
            notebook_uid,
            ..Default::default()
        })?;
        Ok(uid)
    }

    pub fn list_notes(&self) -> Result<Vec<NoteView>> {
        Ok(self
            .store
            .list_notes(ListNotes::All)?
            .into_iter()
            .map(|n| NoteView {
                id: n.uid,
                title: n.title,
                source: n.source,
                html: String::new(),
                content_class: n.content_class,
                source_url: n.source_url,
                author: n.author,
            })
            .collect())
    }

    pub fn search_notes(&self, query: &str) -> Result<Vec<NoteView>> {
        Ok(self
            .store
            .search_notes(query)?
            .into_iter()
            .map(|n| NoteView {
                id: n.uid,
                title: n.title,
                source: n.source,
                html: String::new(),
                content_class: n.content_class,
                source_url: n.source_url,
                author: n.author,
            })
            .collect())
    }

    pub fn trash_note(&self, note_id: i64) -> Result<()> {
        Ok(self.store.trash_note(note_id)?)
    }

    // ----- 窗口状态回报（原 DesktopService 回调方法） -----

    pub fn markup_window_closed(&self, resource_url: &str) {
        self.push_event(DesktopEvent::MarkupWindowClosed(resource_url.to_string()));
    }

    pub fn markup_window_focused(&self, resource_url: &str) {
        self.push_event(DesktopEvent::MarkupWindowFocused(resource_url.to_string()));
    }

    pub fn markup_window_active(&self, resource_url: &str) {
        self.push_event(DesktopEvent::MarkupWindowActive(resource_url.to_string()));
    }

    pub fn presentation_window_closed(&self) {
        self.push_event(DesktopEvent::PresentationWindowClosed);
    }

    // ----- 商业化钩子 -----

    pub fn pdf_trial_started(&self) -> Result<i64> {
        Ok(self.store.bump_trial("pdf")?)
    }

    pub fn pres_trial_started(&self) -> Result<i64> {
        Ok(self.store.bump_trial("pres")?)
    }

    pub fn show_premium_page(&self, offer_code: &str) {
        self.push_event(DesktopEvent::ShowPremiumPage(offer_code.to_string()));
    }

    // ----- NodeWebkitService：原生 → 编辑器派发 -----

    pub fn dispatch(&mut self, cmd: NodeWebkitCommand) {
        self.frontend.dispatch(cmd);
    }
}

fn escape_attr_min(s: &str) -> String {
    s.replace('"', "&quot;").replace('&', "&amp;")
}

fn short_hash(bytes: &[u8]) -> String {
    // 与 store 的 sha256 全哈希解耦：占位符只需稳定短标识。
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn tempfile_dir() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("biji-open");
    std::fs::create_dir_all(&dir).map_err(|_| ServiceError::Unsupported("临时目录创建失败"))?;
    Ok(dir)
}

impl std::fmt::Display for NodeWebkitCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeWebkitCommand::Markup { resource_url, title, .. } => {
                write!(f, "Markup {resource_url}「{title}」")
            }
            NodeWebkitCommand::MarkupPdf { pdf_url, page_num, title, .. } => {
                write!(f, "MarkupPdf {pdf_url} p{page_num}「{title}」")
            }
            NodeWebkitCommand::FocusMarkupWindow => write!(f, "FocusMarkupWindow"),
            NodeWebkitCommand::CloseMarkupWindow => write!(f, "CloseMarkupWindow"),
            NodeWebkitCommand::CloseMarkupWindowWithMessage(m) => {
                write!(f, "CloseMarkupWindowWithMessage「{m}」")
            }
            NodeWebkitCommand::RotateImage { resource_url, theta } => {
                write!(f, "RotateImage {resource_url} {theta}°")
            }
            NodeWebkitCommand::ResizeImage { resource_url, width, height } => {
                write!(f, "ResizeImage {resource_url} {width}x{height}")
            }
            NodeWebkitCommand::ClearImage { resource_url } => {
                write!(f, "ClearImage {resource_url}")
            }
            NodeWebkitCommand::Present { note_ids, index, .. } => {
                write!(f, "Present {note_ids:?} from #{index}")
            }
            NodeWebkitCommand::FocusPresentationWindow => write!(f, "FocusPresentationWindow"),
            NodeWebkitCommand::ClosePresentationWindow => write!(f, "ClosePresentationWindow"),
            NodeWebkitCommand::MinimizeWindow => write!(f, "MinimizeWindow"),
            NodeWebkitCommand::RestoreWindow => write!(f, "RestoreWindow"),
            NodeWebkitCommand::UpdateDisplayInfo(d) => write!(f, "UpdateDisplayInfo {}块", d.len()),
            NodeWebkitCommand::Quit => write!(f, "Quit"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> NoteService {
        NoteService::new(Store::open_memory().unwrap(), Box::new(LoggingFrontend::default()))
    }

    fn recording_service() -> (NoteService, std::rc::Rc<std::cell::RefCell<Vec<String>>>) {
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = opened.clone();
        let svc = NoteService::new(Store::open_memory().unwrap(), Box::new(LoggingFrontend::default()))
            .with_opener(Box::new(move |target| {
                captured.borrow_mut().push(target.to_string());
                Ok(())
            }));
        (svc, opened)
    }

    #[test]
    fn note_lifecycle_through_service() {
        let svc = service();
        let uid = svc.create_note("标题", "<en-note><p>正文</p></en-note>", None).unwrap();
        let view = svc.get_note(uid).unwrap();
        assert_eq!(view.title, "标题");
        assert!(view.html.contains("正文"));
        assert_eq!(svc.get_note_title(uid).unwrap(), "标题");

        svc.update_note(&NoteView {
            id: uid,
            title: "新标题".into(),
            source: None,
            html: "<en-note><p>新正文</p></en-note>".into(),
            content_class: None,
            source_url: None,
            author: None,
        })
        .unwrap();
        let view = svc.get_note(uid).unwrap();
        assert_eq!(view.title, "新标题");
        assert!(view.html.contains("新正文"));
    }

    #[test]
    fn create_note_sanitizes_enml() {
        let svc = service();
        let uid = svc
            .create_note("x", "<en-note><script>alert(1)</script><p>ok</p></en-note>", None)
            .unwrap();
        let view = svc.get_note(uid).unwrap();
        assert!(!view.html.contains("script"));
        assert!(view.html.contains("ok"));
    }

    #[test]
    fn note_link_roundtrip() {
        let svc = service();
        let uid = svc.create_note("t", "<en-note/>", None).unwrap();
        assert_eq!(svc.get_note_id(&note_link(uid)).unwrap(), uid);
        assert!(svc.get_note_id("biji://other/1").is_err());
        assert!(svc.get_note_id("not a link").is_err());
    }

    #[test]
    fn note_locks_exclusive() {
        let svc = service();
        let uid = svc.create_note("t", "<en-note/>", None).unwrap();
        assert!(svc.request_note_lock(uid).unwrap());
        assert!(svc.is_note_locked(uid));
        assert!(matches!(svc.request_note_lock(uid), Err(ServiceError::LockHeld(_))));
        svc.release_note_lock(uid);
        assert!(!svc.is_note_locked(uid));
        assert!(svc.request_note_lock(uid).unwrap());
    }

    #[test]
    fn resource_base64_and_replace() {
        let svc = service();
        let note = svc.create_note("n", "<en-note/>", None).unwrap();
        let ruid = svc.store().add_resource(note, "pic.png", "image/png", b"\x89PNG").unwrap();
        let url = resource_url(ruid);

        let data = svc.get_resource(&url).unwrap();
        assert_eq!(data.mime, "image/png");
        assert_eq!(data.bytes, b"\x89PNG");

        let b64 = svc.get_resource_as_base64(&url).unwrap();
        assert_eq!(b64.base64, base64::engine::general_purpose::STANDARD.encode(b"\x89PNG"));

        svc.update_resource(
            ResourceData { uid: ruid, mime: "image/png".into(), filename: "pic.png".into(), bytes: b"NEW".to_vec() },
            0,
        )
        .unwrap();
        assert_eq!(svc.get_resource(&url).unwrap().bytes, b"NEW");

        // offset>0 分段写：段拼接进现有内容（offset 定位）
        svc.update_resource(
            ResourceData { uid: ruid, mime: "image/png".into(), filename: "pic.png".into(), bytes: b"XY".to_vec() },
            1,
        )
        .unwrap();
        assert_eq!(svc.get_resource(&url).unwrap().bytes, b"NXY");
        // 越界段：末端补零延长
        svc.update_resource(
            ResourceData { uid: ruid, mime: "image/png".into(), filename: "pic.png".into(), bytes: b"Z".to_vec() },
            6,
        )
        .unwrap();
        assert_eq!(svc.get_resource(&url).unwrap().bytes, b"NXY\0\0\0Z");
    }

    #[test]
    fn create_note_from_resource_attaches_media() {
        let svc = service();
        let note_uid = svc
            .create_note_from_resource(
                ResourceData { uid: 0, mime: "image/jpeg".into(), filename: "照片.jpg".into(), bytes: vec![1, 2, 3] },
                0,
            )
            .unwrap();
        let view = svc.get_note(note_uid).unwrap();
        assert_eq!(view.title, "照片.jpg");
        assert!(view.html.contains("en-media"));
        assert_eq!(svc.get_resources(note_uid).unwrap().len(), 1);

        // 分段装配：offset>0 的段拼接进整体内容，同一 uid 幂等指向同一笔记/资源
        let note_uid2 = svc
            .create_note_from_resource(
                ResourceData { uid: 0, mime: "image/jpeg".into(), filename: "照片.jpg".into(), bytes: vec![4, 5] },
                3,
            )
            .unwrap();
        assert_eq!(note_uid2, note_uid, "后续段不应另建笔记");
        let resources = svc.get_resources(note_uid).unwrap();
        assert_eq!(resources.len(), 1, "后续段不应另建资源");
        assert_eq!(resources[0].1.bytes, vec![1, 2, 3, 4, 5], "装配内容失真");
        // 段间空隙补零：offset 8 起写一段
        svc.create_note_from_resource(
            ResourceData { uid: 0, mime: "image/jpeg".into(), filename: "照片.jpg".into(), bytes: vec![9] },
            8,
        )
        .unwrap();
        assert_eq!(
            svc.get_resources(note_uid).unwrap()[0].1.bytes,
            vec![1, 2, 3, 4, 5, 0, 0, 0, 9],
            "空隙补零失真"
        );
    }

    #[test]
    fn open_resource_routes_to_opener() {
        let (svc, opened) = recording_service();
        let note = svc.create_note("n", "<en-note/>", None).unwrap();
        let ruid = svc.store().add_resource(note, "a.bin", "application/octet-stream", b"x").unwrap();
        svc.open_resource(&resource_url(ruid)).unwrap();
        assert_eq!(opened.borrow().len(), 1);
        assert!(opened.borrow()[0].contains("biji-a.bin"));
    }

    #[test]
    fn render_page_writes_png_via_injected_renderer() {
        struct FakeRenderer {
            pub pages: std::cell::RefCell<Vec<u32>>,
        }
        impl pdf::PdfPageFileRenderer for FakeRenderer {
            fn render_to_file(
                &self,
                _pdf: &std::path::Path,
                page: u32,
                _scale_to: i32,
                out_png: &std::path::Path,
            ) -> std::result::Result<(), String> {
                self.pages.borrow_mut().push(page);
                std::fs::write(out_png, b"\x89PNG-fake").map_err(|e| e.to_string())
            }
            fn describe(&self) -> String {
                "fake".into()
            }
        }
        let fake = std::rc::Rc::new(FakeRenderer {
            pages: std::cell::RefCell::new(Vec::new()),
        });
        let svc = service().with_pdf_renderer(fake.clone());
        let note = svc.create_note("n", "<en-note/>", None).unwrap();
        let ruid = svc
            .store()
            .add_resource(note, "doc.pdf", "application/pdf", b"%PDF-fake")
            .unwrap();
        let url = resource_url(ruid);

        // 单页渲染：page_num 透传，产出 PNG 文件路径
        let path = svc.render_page(&url, 0, 800, 600, 2).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG-fake");
        assert_eq!(*fake.pages.borrow(), vec![2]);

        // 批量渲染：每个附件渲染首页
        let paths = svc.render_pages(std::slice::from_ref(&url)).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"\x89PNG-fake");
        assert_eq!(*fake.pages.borrow(), vec![2, 1]);

        // 不存在的资源诚实报错（Store 层 NotFound 透传）
        assert!(matches!(
            svc.render_page(&resource_url(999), 0, 0, 0, 1),
            Err(ServiceError::Store(StoreError::NotFound(999)))
        ));
    }

    #[test]
    fn trial_counters_and_events() {
        let svc = service();
        assert_eq!(svc.pdf_trial_started().unwrap(), 1);
        assert_eq!(svc.pdf_trial_started().unwrap(), 2);
        assert_eq!(svc.pres_trial_started().unwrap(), 1);
        assert!(matches!(
            svc.store().meta_get("pdf_trials").as_deref(),
            Some("2")
        ));

        svc.server_started("biji-core");
        svc.markup_window_closed(&resource_url(7));
        svc.show_premium_page("OFFER1");
        let events = svc.take_events();
        assert_eq!(events.len(), 3);
        assert!(events.contains(&DesktopEvent::ServerStarted("biji-core".into())));
        assert!(events.contains(&DesktopEvent::ShowPremiumPage("OFFER1".into())));
        assert!(svc.take_events().is_empty(), "drain 后应清空");
    }

    #[test]
    fn dispatch_reaches_frontend() {
        let mut svc = service();
        svc.dispatch(NodeWebkitCommand::Markup {
            resource_url: resource_url(3),
            title: "标注".into(),
            x: 10,
            y: 20,
            width: 300,
            height: 200,
            trigger: "context-menu".into(),
        });
        svc.dispatch(NodeWebkitCommand::UpdateDisplayInfo(vec![DisplayInfo {
            id: 1,
            name: "主屏".into(),
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
            dpi: 96,
        }]));
        svc.dispatch(NodeWebkitCommand::Quit);
    }

    #[test]
    fn account_and_analytics() {
        let svc = service();
        svc.set_account("lome", "server.biji", Some("lome@example.com")).unwrap();
        let user = svc.get_current_user_info().unwrap();
        assert_eq!(user.username, "lome");
        assert_eq!(user.service_host, "server.biji");
        assert_eq!(user.email.as_deref(), Some("lome@example.com"));

        // 默认业务开关 = 分析文档 §6.1 中国区 profile 实测值。
        let info = svc.get_analytics_info().unwrap();
        assert_eq!(info.service_host, "server.biji");
        assert!(info.enable_shared_notebooks);
        assert!(!info.enable_single_note_sharing);
        assert!(!info.enable_gift_subscriptions);
    }

    #[test]
    fn list_and_search_via_service() {
        let svc = service();
        svc.create_note("Rust 笔记", "<en-note><p>所有权</p></en-note>", None).unwrap();
        svc.create_note("购物", "<en-note><p>牛奶</p></en-note>", None).unwrap();
        assert_eq!(svc.list_notes().unwrap().len(), 2);
        let hits = svc.search_notes("所有权").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust 笔记");
    }
}
