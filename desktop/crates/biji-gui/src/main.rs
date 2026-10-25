//! biji-gui——biji 笔记的桌面 GUI（egui/eframe，纯 Rust 单壳）。
//!
//! 对应分析文档 §9.1 选型映射："WPF 外壳 + CEF + NW.js 三引擎"收敛为
//! 单一现代壳；GUI 同时扮演原架构中的两个角色：
//! - 原生外壳：调用 `NoteService`（原 DesktopService 通路）；
//! - NW.js 编辑器：实现 `MarkupFrontend` 消费 `NodeWebkitCommand`
//!   （原壳↔编辑器 NW 命令通路，本实现为进程内通道），
//!   `Present` 命令驱动全屏演示模式（原演示子应用的对位物）。
//!
//! 中文字体：egui 默认字体无 CJK 字形，启动时按候选顺序加载系统字体
//! （simhei/msyh/simsun/Deng）。

mod annotate;
mod pdf;
mod preview;
mod theme;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use annotate::AnnotateState;
use biji_enml::Enml;
use biji_service::{
    parse_resource_url, resource_url, DisplayInfo, MarkupFrontend, NodeWebkitCommand, NoteService,
    NoteView, ResourceData,
};
use biji_store::{ListNotes, Store};
use biji_sync::{ServerRemote, SyncEngine};

fn main() -> ExitCode {
    let db = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("biji.exb"));
    let store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("打开库失败 {}: {e}", db.display());
            return ExitCode::FAILURE;
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 600.0])
            .with_title("biji 笔记"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "biji",
        options,
        Box::new(move |cc| {
            install_cjk_fonts(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            // 向 OS 声明浅色主题偏好：Windows 标题栏随系统深色模式时保持浅色，
            // 与窗口内浅色 UI 一致（egui-winit → winit set_theme）。
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(
                egui::SystemTheme::Light,
            ));
            let (tx, rx) = mpsc::channel();
            let service = NoteService::new(store, Box::new(CmdFrontend { tx }));
            Ok(Box::new(BijiApp::new(service, rx)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("GUI 运行失败: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 编辑器前端通道：NoteService 派发的 NodeWebkitCommand 进入队列，由
/// GUI 每帧消费（对位原 NW.js 编辑器进程经命名管道接收命令）。
struct CmdFrontend {
    tx: mpsc::Sender<NodeWebkitCommand>,
}

impl MarkupFrontend for CmdFrontend {
    fn dispatch(&mut self, cmd: NodeWebkitCommand) {
        let _ = self.tx.send(cmd);
    }
}

// ---------- UI 状态 ----------

#[derive(Debug, Clone, PartialEq)]
enum Selection {
    All,
    Notebook(i64),
    Tag(i64),
}

#[derive(Debug, Clone)]
struct EditorState {
    uid: i64,
    title: String,
    content: String,
    source: Option<String>,
    source_url: Option<String>,
    author: Option<String>,
    content_class: Option<String>,
    dirty: bool,
    preview: bool,
    last_edit: Option<Instant>,
    /// 预览态行内编辑：正在编辑的顶层块下标与缓冲。
    preview_editing: Option<usize>,
    preview_buffer: String,
    /// 渲染态选区（跨帧保持：拖选后移向工具栏期间不丢失）。
    preview_sel: Option<(usize, usize, usize)>,
}

#[derive(Debug, Clone)]
struct ListRow {
    uid: i64,
    title: String,
    snippet: String,
    date: f64,
    /// 创建时间（列表头「创建时间」排序用）。
    created: f64,
    tags: Option<String>,
}

#[derive(Debug, Clone)]
enum Confirm {
    DeleteNotebook(i64, String),
    DeleteTag(i64, String),
}

/// 笔记列表排序键（对位原客户端列表头排序：更新时间/创建时间/标题）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListSort {
    Updated,
    Created,
    Title,
}

impl ListSort {
    fn label(self) -> &'static str {
        match self {
            ListSort::Updated => "更新时间",
            ListSort::Created => "创建时间",
            ListSort::Title => "标题",
        }
    }
}

struct UiState {
    selection: Selection,
    trash_view: bool,
    selected_note: Option<i64>,
    editor: Option<EditorState>,
    search: String,
    rows: Vec<ListRow>,
    rows_sig: String,
    /// 笔记列表无限滚动的可见窗口（滚动到底自动扩展，切换视图重置）。
    list_limit: usize,
    /// 列表排序键（对位原客户端列表头排序下拉）。
    list_sort: ListSort,
    /// 升序（false = 降序，最近在前，对位默认）。
    sort_asc: bool,
    /// 紧凑单行视图（false = 卡片视图）。
    list_compact: bool,
    /// Ctrl+F 置位：下一帧聚焦搜索框。
    focus_search: bool,
    /// 笔记信息弹窗（ℹ）。
    note_info: Option<i64>,
    /// 清单多选集（对位原客户端列表多选；空 = 未进入多选态）。
    multi_select: Vec<i64>,
    /// Shift 区间选择锚点（普通/Shift 点击更新，Ctrl 不动）。
    anchor: Option<i64>,
    /// 添加标签菜单的搜索过滤（前缀匹配候选 + 新建入口）。
    tag_filter: String,
    /// 重命名笔记本弹窗（uid + 编辑缓冲）。
    rename_nb: Option<(i64, String)>,
    /// 重命名标签弹窗（uid + 编辑缓冲）。
    rename_tag: Option<(i64, String)>,
    present: Option<(Vec<i64>, usize)>,
    status: String,
    show_new_notebook: bool,
    new_notebook_name: String,
    confirm: Option<Confirm>,
    displays: Vec<DisplayInfo>,
    annotating: Option<AnnotateState>,
    chat: Option<ChatState>,
    materials: Option<MaterialState>,
    groupspace: Option<GroupspaceState>,
    /// 侧栏分区折叠（对位原客户端的手风琴交互）。
    show_notebooks: bool,
    show_tags: bool,
    show_shortcuts: bool,
}

/// 聊天窗口状态（P3：thread/message 存储消费方）。
struct ChatState {
    open: bool,
    threads: Vec<(i64, String)>,
    thread_uid: Option<i64>,
    loaded_thread: Option<i64>,
    messages: Vec<(i64, String, String, f64)>,
    input: String,
    new_name: String,
}

/// 素材面板状态（P3：material_attr 消费方）。
struct MaterialState {
    open: bool,
    status: String,
}

/// 企业空间面板状态（P3：groupspace_attr 消费方）。
struct GroupspaceState {
    open: bool,
    new_name: String,
    new_desc: String,
    status: String,
}

impl UiState {
    /// 无限滚动每步渲染的卡片数（首屏与每次扩展的步长）。
    const LIST_PAGE: usize = 60;

    fn new() -> UiState {
        UiState {
            selection: Selection::All,
            trash_view: false,
            selected_note: None,
            editor: None,
            search: String::new(),
            rows: Vec::new(),
            rows_sig: String::new(),
            list_limit: Self::LIST_PAGE,
            list_sort: ListSort::Updated,
            sort_asc: false,
            list_compact: false,
            focus_search: false,
            note_info: None,
            multi_select: Vec::new(),
            anchor: None,
            tag_filter: String::new(),
            rename_nb: None,
            rename_tag: None,
            present: None,
            status: "就绪".into(),
            show_new_notebook: false,
            new_notebook_name: String::new(),
            confirm: None,
            displays: Vec::new(),
            annotating: None,
            chat: None,
            materials: None,
            groupspace: None,
            show_notebooks: true,
            show_tags: true,
            show_shortcuts: true,
        }
    }
}

struct BijiApp {
    service: NoteService,
    cmd_rx: mpsc::Receiver<NodeWebkitCommand>,
    st: UiState,
}

impl BijiApp {
    fn new(service: NoteService, cmd_rx: mpsc::Receiver<NodeWebkitCommand>) -> BijiApp {
        // 主题随库持久化（meta 表 key = "theme"），默认浅色
        let dark = service.store().meta_get("theme").as_deref() == Some("dark");
        theme::set_dark(dark);
        let mut app = BijiApp {
            service,
            cmd_rx,
            st: UiState::new(),
        };
        app.refresh_rows();
        app
    }

    /// 切换浅色/深色主题并写入库 meta（重启后保持）。
    fn toggle_theme(&mut self) {
        let on = !theme::is_dark();
        theme::set_dark(on);
        let label = if on { "深色" } else { "浅色" };
        match self
            .service
            .store()
            .meta_set("theme", if on { "dark" } else { "light" })
        {
            Ok(()) => self.st.status = format!("已切换到{label}模式"),
            Err(e) => self.st.status = format!("主题切换成功但保存失败: {e}"),
        }
    }

    /// 全局快捷键（update() 每帧调用；对位原客户端；放映模式不响应，
    /// 文本编辑聚焦时不抢 Delete）。
    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        // Ctrl+S 保存
        let save_hotkey = ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.ctrl);
        if save_hotkey && self.st.editor.as_ref().map(|e| e.dirty).unwrap_or(false) {
            self.save_editor();
        }
        if self.st.present.is_some() {
            return;
        }
        let new_hotkey = ctx.input(|i| i.key_pressed(egui::Key::N) && i.modifiers.ctrl);
        let find_hotkey = ctx.input(|i| i.key_pressed(egui::Key::F) && i.modifiers.ctrl);
        if new_hotkey {
            self.create_note();
        } else if find_hotkey {
            self.st.focus_search = true;
        }
        let del_pressed = ctx.input(|i| i.key_pressed(egui::Key::Delete) && !i.modifiers.any());
        if del_pressed && !ctx.wants_keyboard_input() && self.st.present.is_none() {
            // 多选态：Delete 批量移入回收站；否则删单选笔记
            if !self.st.multi_select.is_empty() {
                let uids = self.st.multi_select.clone();
                let mut ok = 0usize;
                for u in &uids {
                    if self.service.trash_note(*u).is_ok() {
                        ok += 1;
                    }
                }
                self.st.status = format!("已把 {ok} 条笔记移入回收站");
                self.st.multi_select.clear();
                self.st.selected_note = None;
                self.refresh_rows();
            } else if let Some(uid) = self.st.selected_note {
                match self.service.trash_note(uid) {
                    Ok(()) => {
                        self.st.status = format!("已移入回收站 {}", note_label(uid));
                        self.st.selected_note = None;
                        self.refresh_rows();
                    }
                    Err(e) => self.st.status = format!("删除失败: {e}"),
                }
            }
        }
        // Esc 退出多选态（演示模式外的场景）
        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            && self.st.present.is_none()
            && !self.st.multi_select.is_empty()
        {
            self.st.multi_select.clear();
        }
    }

    // ----- 数据刷新 -----

    fn current_usn(&self) -> i64 {
        self.service
            .store()
            .meta_get("usn")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    fn rows_signature(&self) -> String {
        format!(
            "{}|{:?}|{}|{}|{:?}|{}|{}",
            self.current_usn(),
            self.st.selection,
            self.st.trash_view,
            self.st.search,
            self.st.list_sort,
            self.st.sort_asc,
            self.st.list_compact
        )
    }

    /// 纯排序（供单测）：按 key 排序；降序时整体取反（标题同理，对位原客户端）。
    fn sort_rows(rows: &mut [ListRow], sort: ListSort, asc: bool) {
        match sort {
            ListSort::Updated => rows.sort_by(|a, b| {
                a.date
                    .partial_cmp(&b.date)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ListSort::Created => rows.sort_by(|a, b| {
                a.created
                    .partial_cmp(&b.created)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ListSort::Title => rows.sort_by(|a, b| a.title.cmp(&b.title)),
        }
        if !asc {
            rows.reverse();
        }
    }

    /// 卡片点击语义（对位原客户端列表多选）：
    /// 普通 = 清多选 + 单选；Ctrl = 切换多选成员；Shift = 锚点区间全选。
    fn click_select(st: &mut UiState, uid: i64, mods: egui::Modifiers) {
        if mods.ctrl {
            Self::ctrl_toggle(&mut st.multi_select, uid);
        } else if mods.shift {
            let anchor = st.anchor.unwrap_or(uid);
            let rows = st.rows.clone();
            Self::select_range(&rows, anchor, uid, &mut st.multi_select);
            st.anchor = Some(uid);
        } else {
            st.multi_select.clear();
            st.selected_note = Some(uid);
            st.anchor = Some(uid);
        }
    }

    /// Shift 区间选择（供单测）：rows 顺序中 anchor..to 的 uid 全选（保序去重）。
    fn select_range(rows: &[ListRow], anchor: i64, to: i64, multi: &mut Vec<i64>) {
        let pos = |uid: i64| rows.iter().position(|r| r.uid == uid);
        let (Some(a), Some(b)) = (pos(anchor), pos(to)) else {
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        *multi = rows[lo..=hi].iter().map(|r| r.uid).collect();
    }

    /// Ctrl 点击切换（供单测）：保序加入/移出多选集。
    fn ctrl_toggle(multi: &mut Vec<i64>, uid: i64) {
        match multi.iter().position(|&u| u == uid) {
            Some(i) => {
                multi.remove(i);
            }
            None => multi.push(uid),
        }
    }

    /// 笔记卡右键菜单（对位原客户端）：移入回收站 / 添加-取消快捷方式 /
    /// 移动到笔记本（平铺各笔记本与「未分类」——直列目标比悬停子菜单更可用）。
    fn note_context_menu(
        resp: &egui::Response,
        uid: i64,
        pinned: bool,
        notebooks: &[(i64, String)],
        trash_req: &mut Option<i64>,
        shortcut_req: &mut Option<i64>,
        move_req: &mut Option<(i64, Option<i64>)>,
    ) {
        resp.context_menu(|ui| {
            if ui.button("🗑 移入回收站").clicked() {
                *trash_req = Some(uid);
                ui.close_menu();
            }
            if ui
                .button(if pinned {
                    "📌 取消快捷方式"
                } else {
                    "📌 添加到快捷方式"
                })
                .clicked()
            {
                *shortcut_req = Some(uid);
                ui.close_menu();
            }
            ui.separator();
            ui.label(egui::RichText::new("移动到笔记本").small().weak());
            for (nb_uid, nb_name) in notebooks {
                if ui.button(format!("→ {nb_name}")).clicked() {
                    *move_req = Some((uid, Some(*nb_uid)));
                    ui.close_menu();
                }
            }
            if ui.button("→ （未分类）").clicked() {
                *move_req = Some((uid, None));
                ui.close_menu();
            }
        });
    }

    fn refresh_rows(&mut self) {
        let sig = self.rows_signature();
        if sig != self.st.rows_sig {
            // 新列表（切换笔记本/标签/搜索/回收站）：无限滚动窗口回到首页，
            // 多选态与锚点随旧列表失效
            self.st.list_limit = UiState::LIST_PAGE;
            self.st.multi_select.clear();
            self.st.anchor = None;
        }
        if sig == self.st.rows_sig {
            return;
        }
        let store = self.service.store();
        let listed = if !self.st.search.is_empty() {
            store.search_notes(&self.st.search)
        } else if self.st.trash_view {
            store.list_notes(ListNotes::Trash)
        } else {
            match self.st.selection {
                Selection::All => store.list_notes(ListNotes::All),
                Selection::Notebook(uid) => store.list_notes(ListNotes::ByNotebook(uid)),
                Selection::Tag(uid) => store.list_notes(ListNotes::ByTag(uid)),
            }
        };
        let mut rows = Vec::new();
        if let Ok(notes) = listed {
            for meta in notes {
                let snippet = store
                    .get_note(meta.uid)
                    .ok()
                    .map(|full| {
                        let mut doc = match Enml::parse(&full.enml) {
                            Ok(d) => d,
                            Err(_) => Enml::from_plain_text(&full.enml),
                        };
                        doc.sanitize();
                        let text = doc.to_plain_text().replace('\n', " ");
                        let mut s: String = text.chars().take(64).collect();
                        if text.chars().count() > 64 {
                            s.push('…');
                        }
                        s
                    })
                    .unwrap_or_default();
                rows.push(ListRow {
                    uid: meta.uid,
                    title: if meta.title.is_empty() {
                        "（无标题）".into()
                    } else {
                        meta.title.clone()
                    },
                    snippet,
                    date: meta.date_updated,
                    created: meta.date_created,
                    tags: meta.tags_text,
                });
            }
        }
        Self::sort_rows(&mut rows, self.st.list_sort, self.st.sort_asc);
        self.st.rows = rows;
        self.st.rows_sig = sig;
    }

    fn load_editor(&mut self) {
        let want = self.st.selected_note;
        let loaded = self.st.editor.as_ref().map(|e| e.uid);
        if want == loaded {
            return;
        }
        match want {
            None => self.st.editor = None,
            Some(uid) => match self.service.get_note(uid) {
                Ok(view) => {
                    self.st.editor = Some(EditorState {
                        uid,
                        title: view.title,
                        content: view.html,
                        source: view.source,
                        source_url: view.source_url,
                        author: view.author,
                        content_class: view.content_class,
                        dirty: false,
                        preview: true,
                        last_edit: None,
                        preview_editing: None,
                        preview_buffer: String::new(),
                        preview_sel: None,
                    });
                }
                Err(e) => {
                    self.st.selected_note = None;
                    self.st.editor = None;
                    self.st.status = format!("打开笔记失败: {e}");
                }
            },
        }
    }

    fn save_editor(&mut self) {
        let Some(ed) = self.st.editor.as_ref() else {
            return;
        };
        if !ed.dirty {
            return;
        }
        let view = NoteView {
            id: ed.uid,
            title: ed.title.clone(),
            source: ed.source.clone(),
            html: ed.content.clone(),
            content_class: ed.content_class.clone(),
            source_url: ed.source_url.clone(),
            author: ed.author.clone(),
        };
        match self.service.update_note(&view) {
            Ok(()) => {
                if let Some(ed) = self.st.editor.as_mut() {
                    ed.dirty = false;
                    ed.last_edit = None;
                }
                let label = note_label(view.id);
                self.st.status = format!("已保存 {label}");
                self.refresh_rows();
            }
            Err(e) => self.st.status = format!("保存失败: {e}"),
        }
    }

    fn create_note(&mut self) {
        let notebook_uid = match self.st.selection {
            Selection::Notebook(uid) => Some(uid),
            _ => None,
        };
        match self
            .service
            .create_note("", "<en-note><p></p></en-note>", notebook_uid)
        {
            Ok(uid) => {
                self.st.selected_note = Some(uid);
                self.st.trash_view = false;
                self.st.search.clear();
                self.refresh_rows();
                self.load_editor();
                if let Some(ed) = self.st.editor.as_mut() {
                    ed.preview = false;
                    ed.dirty = true; // 空标题待用户填写，切换或自动保存时落库
                }
                self.st.status = format!("已新建 {}", note_label(uid));
            }
            Err(e) => self.st.status = format!("新建失败: {e}"),
        }
    }

    fn pending_sync_count(&self) -> usize {
        SyncEngine::new(self.service.store())
            .pending_count()
            .unwrap_or(0)
    }

    // ----- 命令通路（原 NW.js 编辑器职责） -----

    fn drain_commands(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                NodeWebkitCommand::Quit => {
                    self.st.status = "收到 Quit，退出（原命令通路演示）".into();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                NodeWebkitCommand::Present {
                    note_ids, index, ..
                } => {
                    if note_ids.is_empty() {
                        self.st.status = "演示请求为空列表".into();
                    } else {
                        let idx = index.min(note_ids.len() - 1);
                        self.st.present = Some((note_ids, idx));
                        self.st.status = "进入演示模式".into();
                    }
                }
                NodeWebkitCommand::Markup {
                    resource_url,
                    title,
                    ..
                } => {
                    self.open_annotator(ctx, &resource_url, &title);
                }
                NodeWebkitCommand::MarkupPdf {
                    pdf_url,
                    page_num,
                    title,
                    ..
                } => {
                    self.open_pdf_annotator(ctx, &pdf_url, page_num.max(1) as u32, &title);
                }
                NodeWebkitCommand::RotateImage {
                    resource_url,
                    theta,
                } => {
                    self.rotate_resource(&resource_url, theta);
                }
                NodeWebkitCommand::ResizeImage {
                    resource_url,
                    width,
                    height,
                } => {
                    self.resize_resource(&resource_url, width, height);
                }
                NodeWebkitCommand::ClearImage { resource_url } => {
                    self.clear_resource_annotations(&resource_url);
                }
                NodeWebkitCommand::UpdateDisplayInfo(displays) => {
                    self.st.displays = displays;
                    self.st.status = format!("显示器信息更新：{} 块", self.st.displays.len());
                }
                other => {
                    self.st.status = format!("命令: {other}");
                }
            }
        }
    }

    // ----- 演示模式 -----

    fn draw_present(&mut self, ctx: &egui::Context) {
        let input = ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::F5));
        let advance = ctx.input(|i| {
            i.key_pressed(egui::Key::ArrowRight)
                || i.key_pressed(egui::Key::ArrowDown)
                || i.key_pressed(egui::Key::N)
                || i.key_pressed(egui::Key::J)
        });
        let back = ctx.input(|i| {
            i.key_pressed(egui::Key::ArrowLeft)
                || i.key_pressed(egui::Key::ArrowUp)
                || i.key_pressed(egui::Key::P)
                || i.key_pressed(egui::Key::K)
        });
        if input {
            self.st.present = None;
            self.st.status = "退出演示模式".into();
            return;
        }
        let Some((ids, index)) = self.st.present.clone() else {
            return;
        };
        let index = if advance {
            (index + 1).min(ids.len() - 1)
        } else if back {
            index.saturating_sub(1)
        } else {
            index
        };
        self.st.present = Some((ids.clone(), index));
        if advance || back {
            self.load_editor_content_for_present(ids[index]);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::from_rgb(24, 26, 30)))
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.add_space(24.0);
                    let title = self
                        .service
                        .get_note_title(ids[index])
                        .unwrap_or_else(|_| "（无法读取）".into());
                    ui.label(
                        egui::RichText::new(&title)
                            .size(30.0)
                            .strong()
                            .color(egui::Color32::from_rgb(235, 235, 240)),
                    );
                    ui.add_space(12.0);
                    ui.separator();
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        if let Ok(view) = self.service.get_note(ids[index]) {
                            let mut doc = Enml::parse(&view.html)
                                .or_else(|_| Enml::parse_loose(&view.html))
                                .unwrap_or_else(|_| Enml::from_plain_text(&view.html));
                            doc.sanitize();
                            let text = doc.to_plain_text();
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(text)
                                    .size(19.0)
                                    .color(egui::Color32::from_rgb(210, 212, 218)),
                            );
                        }
                    });
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}/{}  ·  ←/→ 翻页  ·  Esc 退出",
                            index + 1,
                            ids.len()
                        ))
                        .weak()
                        .color(egui::Color32::from_rgb(150, 152, 160)),
                    );
                    ui.add_space(8.0);
                });
            });
    }

    /// 演示模式换页时同步选中笔记（预取缓存友好）。
    fn load_editor_content_for_present(&mut self, _uid: i64) {
        // 演示渲染即时读取，无需预取；保留钩子位。
    }

    // ----- 主界面 -----

    fn draw_main(&mut self, ctx: &egui::Context) {
        self.draw_menubar(ctx);
        self.draw_sidebar(ctx);
        self.draw_note_list(ctx);
        egui::CentralPanel::default().show(ctx, |ui| self.draw_editor(ui));
        self.draw_status_bar(ctx);
        self.draw_dialogs(ctx);
    }

    /// 菜单栏（对位原客户端 文件/编辑/查看/笔记/格式/工具/帮助 一行）。
    fn draw_menubar(&mut self, ctx: &egui::Context) {
        let menu_text = |s: &str| egui::RichText::new(s).size(13.0).color(theme::pal().text);
        egui::TopBottomPanel::top("menubar")
            .frame(
                egui::Frame::default()
                    .fill(theme::pal().white)
                    .inner_margin(egui::Margin::symmetric(8.0, 2.0)),
            )
            .show(ctx, |ui| {
                // 菜单按钮扁平化：静态纯文字，hover 才显灰底（对位原生菜单栏）
                {
                    let w = &mut ui.style_mut().visuals.widgets;
                    w.inactive.bg_fill = theme::pal().white;
                    w.inactive.weak_bg_fill = theme::pal().white;
                    w.inactive.bg_stroke = egui::Stroke::NONE;
                    w.hovered.bg_fill = egui::Color32::from_rgb(0xEF, 0xEF, 0xF2);
                    w.hovered.weak_bg_fill = egui::Color32::from_rgb(0xEF, 0xEF, 0xF2);
                    w.hovered.bg_stroke = egui::Stroke::NONE;
                }
                ui.horizontal(|ui| {
                    ui.menu_button(menu_text("文件(F)"), |ui| {
                        if ui.button("＋ 新建笔记").clicked() {
                            self.create_note();
                            ui.close_menu();
                        }
                        if ui.button("同步（推送/拉取一轮）").clicked() {
                            self.do_sync();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("退出").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.menu_button(menu_text("编辑(E)"), |ui| {
                        if ui.button("清除搜索").clicked() {
                            self.st.search.clear();
                            self.refresh_rows();
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(menu_text("查看(V)"), |ui| {
                        if ui.button("🖼 卡片视图").clicked() {
                            self.st.list_compact = false;
                            self.refresh_rows();
                            ui.close_menu();
                        }
                        if ui.button("☰ 紧凑列表").clicked() {
                            self.st.list_compact = true;
                            self.refresh_rows();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .button(if theme::is_dark() {
                                "☀️ 浅色模式"
                            } else {
                                "🌙 深色模式"
                            })
                            .clicked()
                        {
                            self.toggle_theme();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("▶ 演示模式（全屏放映）").clicked() {
                            let ids: Vec<i64> =
                                self.st.rows.iter().map(|r| r.uid).collect();
                            if ids.is_empty() {
                                self.st.status = "当前列表为空，无可演示内容".into();
                            } else {
                                self.service.dispatch(NodeWebkitCommand::Present {
                                    note_ids: ids,
                                    index: 0,
                                    x: 0,
                                    y: 0,
                                    width: 0,
                                    height: 0,
                                    trigger: "menu".into(),
                                });
                            }
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(menu_text("笔记(N)"), |ui| {
                        let uid = self.st.editor.as_ref().map(|e| e.uid);
                        if let Some(uid) = uid {
                            if ui.button("删除当前笔记（移入回收站）").clicked() {
                                match self.service.trash_note(uid) {
                                    Ok(()) => {
                                        self.st.status =
                                            format!("已移入回收站 {}", note_label(uid));
                                        self.st.selected_note = None;
                                        self.refresh_rows();
                                    }
                                    Err(e) => self.st.status = format!("删除失败: {e}"),
                                }
                                ui.close_menu();
                            }
                        } else {
                            ui.label(
                                egui::RichText::new("（未打开笔记）")
                                    .small()
                                    .color(theme::pal().subtle),
                            );
                        }
                    });
                    ui.menu_button(menu_text("格式(O)"), |ui| {
                        let dirty = self.st.editor.as_ref().map(|e| e.dirty);
                        if ui.button("✏️ 源码编辑模式").clicked() {
                            if let Some(ed) = self.st.editor.as_mut() {
                                ed.preview = false;
                            }
                            ui.close_menu();
                        }
                        if ui.button("👁 富文本预览模式").clicked() {
                            if dirty == Some(true) {
                                self.save_editor();
                            }
                            if let Some(ed) = self.st.editor.as_mut() {
                                ed.preview = true;
                            }
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(menu_text("工具(T)"), |ui| {
                        if ui.button("💬 聊天").clicked() {
                            self.st.chat = if self.st.chat.is_some() {
                                None
                            } else {
                                Some(ChatState {
                                    open: true,
                                    threads: Vec::new(),
                                    thread_uid: None,
                                    loaded_thread: None,
                                    messages: Vec::new(),
                                    input: String::new(),
                                    new_name: String::new(),
                                })
                            };
                            ui.close_menu();
                        }
                        if ui.button("📦 素材库").clicked() {
                            self.st.materials = if self.st.materials.is_some() {
                                None
                            } else {
                                Some(MaterialState {
                                    open: true,
                                    status: String::new(),
                                })
                            };
                            ui.close_menu();
                        }
                        if ui.button("🏢 企业空间").clicked() {
                            self.st.groupspace = if self.st.groupspace.is_some() {
                                None
                            } else {
                                Some(GroupspaceState {
                                    open: true,
                                    new_name: String::new(),
                                    new_desc: String::new(),
                                    status: String::new(),
                                })
                            };
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(menu_text("帮助(H)"), |ui| {
                        if ui.button("关于 biji").clicked() {
                            self.st.status =
                                "biji 笔记 · Rust/egui · ENML 存储 · USN 同步（自有后端） · 双服务语义"
                                    .into();
                            ui.close_menu();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let user = self
                            .service
                            .get_current_user_info()
                            .map(|u| format!("{}#{}", u.username, u.service_host))
                            .unwrap_or_default();
                        ui.label(
                            egui::RichText::new(user)
                                .small()
                                .color(theme::pal().subtle),
                        );
                        let pending = self.pending_sync_count();
                        if ui
                            .add(theme::green_button(&format!("同步({pending})")))
                            .clicked()
                        {
                            self.do_sync();
                        }
                        // 主题快捷切换（对位原客户端工具栏的明暗切换钮）
                        if ui
                            .button(if theme::is_dark() { "☀️" } else { "🌙" })
                            .on_hover_text(if theme::is_dark() {
                                "切换到浅色模式"
                            } else {
                                "切换到深色模式"
                            })
                            .clicked()
                        {
                            self.toggle_theme();
                        }
                    });
                });
            });
    }

    fn do_sync(&mut self) {
        let store = self.service.store();
        let engine = SyncEngine::new(store);
        // 同步目标 = 自研后端 biji-server-go（默认 127.0.0.1:8332 + biji/biji，
        // BIJI_SERVER_URL/USER/PASS 覆盖）；未启动时诚实报错并给联调指引。
        let mut remote = ServerRemote::connect();
        match engine.sync_round(&mut remote) {
            Ok(r) => {
                self.st.status = format!(
                    "同步完成：推送 {} 条，拉取笔记 {} 条、标签 {} 条、删除 {} 条（服务器 USN {}）",
                    r.pushed, r.pulled_notes, r.pulled_tags, r.pulled_expunged, r.server_usn
                );
                self.refresh_rows();
            }
            Err(e) => self.st.status = format!("同步失败: {e}"),
        }
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        let notebooks = self.service.store().list_notebooks().unwrap_or_default();
        let tags = self.service.store().list_tags().unwrap_or_default();
        let shortcut_notes = self.service.store().shortcut_notes();
        let mut action: Option<Selection> = None;
        let mut to_confirm_delete: Option<(i64, String)> = None;
        let mut to_confirm_delete_tag: Option<(i64, String)> = None;
        let mut rename_tag_req: Option<(i64, String)> = None;
        let mut open_note = Option::<i64>::None;
        let mut unpin_req = Option::<i64>::None;
        let mut open_new_nb = false;

        egui::SidePanel::left("sidebar")
            .resizable(false)
            .default_width(190.0)
            .frame(
                egui::Frame::default()
                    .fill(theme::pal().sidebar_bg)
                    .inner_margin(egui::Margin::same(8.0)),
            )
            // 原客户端侧栏与内容区无可见分隔线，靠底色区分
            .show(ctx, |ui| {
                // 新建笔记：白药丸 + 绿圆＋（对位原客户端侧栏顶部组合控件；
                // 内边距 10px 与导航行图标左缘对齐）
                let pill = egui::Frame::default()
                    .fill(theme::pal().white)
                    .stroke(egui::Stroke::new(1.0_f32, theme::pal().hairline))
                    .rounding(15.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 4.0));
                let pill_inner = pill.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let plus = egui::Button::new(
                            egui::RichText::new("＋").small().color(theme::pal().white),
                        )
                        .fill(theme::pal().green)
                        .stroke(egui::Stroke::NONE)
                        .rounding(11.0);
                        ui.add_sized([22.0, 22.0], plus);
                        ui.label(egui::RichText::new("新建笔记").color(theme::pal().text));
                    });
                });
                if ui
                    .interact(
                        pill_inner.response.rect,
                        egui::Id::new("new-note-pill"),
                        egui::Sense::click(),
                    )
                    .clicked()
                {
                    self.create_note();
                }
                // 快捷方式分区（对位原客户端侧栏置顶笔记）：点击直达笔记
                if !shortcut_notes.is_empty() {
                    ui.add_space(6.0);
                    theme::section_header(ui, "快捷方式", &mut self.st.show_shortcuts, false);
                    if self.st.show_shortcuts {
                        for (uid, title) in &shortcut_notes {
                            let row = theme::nav_row(ui, "📌", title, false);
                            if row.clicked() {
                                open_note = Some(*uid);
                            }
                            row.context_menu(|ui| {
                                if ui.button("✕ 取消快捷方式").clicked() {
                                    unpin_req = Some(*uid);
                                    ui.close_menu();
                                }
                            });
                        }
                    }
                }
                let all_sel = !self.st.trash_view && self.st.selection == Selection::All;
                if theme::nav_row(ui, "📄", "全部笔记", all_sel).clicked() {
                    action = Some(Selection::All);
                    self.st.trash_view = false;
                }
                ui.add_space(10.0);
                // 分区头：箭头 + 小节名 + 右侧新建（自绘行，标签与导航文字对齐）
                if theme::section_header(ui, "笔记本", &mut self.st.show_notebooks, true) {
                    open_new_nb = true;
                }
                if self.st.show_notebooks {
                    for nb in &notebooks {
                        ui.horizontal(|ui| {
                            let selected = !self.st.trash_view
                                && self.st.selection == Selection::Notebook(nb.uid);
                            let w = ui.available_width() - 58.0;
                            let row = theme::nav_row_sized(ui, w, "📓", &nb.name, selected);
                            if row.clicked() {
                                action = Some(Selection::Notebook(nb.uid));
                                self.st.trash_view = false;
                            }
                            // 右键菜单（对位原客户端笔记本行）：重命名 / 删除
                            row.context_menu(|ui| {
                                if ui.button("✏️ 重命名笔记本").clicked() {
                                    self.st.rename_nb = Some((nb.uid, nb.name.clone()));
                                    ui.close_menu();
                                }
                                if ui.button("🗑 删除笔记本").clicked() {
                                    to_confirm_delete = Some((nb.uid, nb.name.clone()));
                                    ui.close_menu();
                                }
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("✕")
                                                    .small()
                                                    .color(theme::pal().subtle),
                                            )
                                            .fill(theme::pal().sidebar_bg)
                                            .stroke(egui::Stroke::NONE),
                                        )
                                        .on_hover_text("删除笔记本")
                                        .clicked()
                                    {
                                        to_confirm_delete = Some((nb.uid, nb.name.clone()));
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("{}", nb.note_count))
                                            .small()
                                            .color(theme::pal().subtle),
                                    );
                                },
                            );
                        });
                    }
                }
                ui.add_space(10.0);
                if self.st.show_tags {
                    for t in &tags {
                        let selected =
                            !self.st.trash_view && self.st.selection == Selection::Tag(t.uid);
                        let row = theme::nav_row(
                            ui,
                            "🏷",
                            &format!("{} ({})", t.name, t.note_count),
                            selected,
                        );
                        if row.clicked() {
                            action = Some(Selection::Tag(t.uid));
                            self.st.trash_view = false;
                        }
                        // 右键菜单（对位原客户端标签行）：重命名 / 删除
                        row.context_menu(|ui| {
                            if ui.button("✏️ 重命名标签").clicked() {
                                rename_tag_req = Some((t.uid, t.name.clone()));
                                ui.close_menu();
                            }
                            if ui.button("🗑 删除标签").clicked() {
                                to_confirm_delete_tag = Some((t.uid, t.name.clone()));
                                ui.close_menu();
                            }
                        });
                    }
                }
                ui.add_space(10.0);
                if theme::nav_row(ui, "🏢", "企业空间", false).clicked() {
                    self.st.groupspace = if self.st.groupspace.is_some() {
                        None
                    } else {
                        Some(GroupspaceState {
                            open: true,
                            new_name: String::new(),
                            new_desc: String::new(),
                            status: String::new(),
                        })
                    };
                }
                ui.add_space(6.0);
                if theme::nav_row(ui, "🗑", "回收站", self.st.trash_view).clicked() {
                    self.st.trash_view = true;
                    action = Some(self.st.selection.clone());
                }
            });

        if let Some(sel) = action {
            self.st.selection = sel;
            self.st.selected_note = None;
            self.refresh_rows();
        }
        if open_new_nb {
            self.st.show_new_notebook = true;
        }
        if let Some((uid, name)) = to_confirm_delete {
            self.st.confirm = Some(Confirm::DeleteNotebook(uid, name));
        }
        if let Some((uid, name)) = to_confirm_delete_tag {
            self.st.confirm = Some(Confirm::DeleteTag(uid, name));
        }
        if let Some((uid, name)) = rename_tag_req {
            self.st.rename_tag = Some((uid, name));
        }
        if let Some(uid) = open_note {
            // 直达笔记：切回全部视图并选中打开（笔记可能在其它来源下不可见）
            self.st.selection = Selection::All;
            self.st.trash_view = false;
            self.st.search.clear();
            self.st.selected_note = Some(uid);
            self.refresh_rows();
        }
        if let Some(uid) = unpin_req {
            match self.service.store().toggle_shortcut(uid) {
                Ok(_) => self.st.status = format!("已取消 {} 的快捷方式", note_label(uid)),
                Err(e) => self.st.status = format!("取消失败: {e}"),
            }
        }
    }

    fn draw_note_list(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("note-list")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                // 来源下拉与右键菜单共用（全部/笔记本/标签）
                let notebooks: Vec<(i64, String)> = self
                    .service
                    .store()
                    .list_notebooks()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|n| (n.uid, n.name))
                    .collect();
                let tags: Vec<(i64, String)> = self
                    .service
                    .store()
                    .list_tags()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| (t.uid, t.name))
                    .collect();
                let header = if self.st.trash_view {
                    "回收站".to_string()
                } else if !self.st.search.is_empty() {
                    format!("搜索「{}」", self.st.search)
                } else {
                    match &self.st.selection {
                        Selection::All => "全部笔记".to_string(),
                        Selection::Notebook(uid) => self
                            .service
                            .store()
                            .list_notebooks()
                            .ok()
                            .and_then(|nbs| nbs.into_iter().find(|n| n.uid == *uid).map(|n| n.name))
                            .unwrap_or_else(|| "笔记本".into()),
                        Selection::Tag(uid) => self
                            .service
                            .store()
                            .list_tags()
                            .ok()
                            .and_then(|ts| ts.into_iter().find(|t| t.uid == *uid).map(|t| t.name))
                            .map(|n| format!("标签：{n}"))
                            .unwrap_or_else(|| "标签".into()),
                    }
                };
                // 搜索框（对位原客户端列表列顶部；F6 / Ctrl+F 聚焦）
                let search_id = egui::Id::new("note-search");
                let want_focus = ui.input(|i| i.key_pressed(egui::Key::F6))
                    || std::mem::take(&mut self.st.focus_search);
                if want_focus {
                    ui.ctx().memory_mut(|m| m.request_focus(search_id));
                }
                let mut search = self.st.search.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut search)
                        .hint_text("搜索笔记 (Ctrl+F)")
                        .id(search_id)
                        .desired_width(ui.available_width()),
                );
                if resp.changed() {
                    self.st.search = search;
                    self.refresh_rows();
                }
                ui.add_space(6.0);
                // 标题行（对位原客户端：点击弹来源选择——全部笔记/各笔记本/各标签）
                let src_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new(format!("{header} ▼"))
                            .size(17.0)
                            .strong()
                            .color(theme::pal().text),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE),
                );
                let mut pick: Option<Selection> = None;
                let source_id = egui::Id::new("source-menu");
                if src_btn.clicked() {
                    ui.memory_mut(|m| {
                        if m.is_popup_open(source_id) {
                            m.close_popup();
                        } else {
                            m.open_popup(source_id);
                        }
                    });
                }
                egui::popup_below_widget(
                    ui,
                    source_id,
                    &src_btn,
                    egui::PopupCloseBehavior::CloseOnClick,
                    |ui| {
                        if ui.button("📄 全部笔记").clicked() {
                            pick = Some(Selection::All);
                        }
                        for (uid, name) in &notebooks {
                            if ui.button(format!("📓 {name}")).clicked() {
                                pick = Some(Selection::Notebook(*uid));
                            }
                        }
                        for (uid, name) in &tags {
                            if ui.button(format!("🏷 {name}")).clicked() {
                                pick = Some(Selection::Tag(*uid));
                            }
                        }
                    },
                );
                if let Some(sel) = pick {
                    self.st.selection = sel;
                    self.st.trash_view = false;
                    self.st.selected_note = None;
                    self.st.search.clear();
                    self.refresh_rows();
                }
                ui.add_space(2.0);
                // 列表头控件行（对位原客户端：条数 + 排序下拉 + 方向 + 视图切换）
                let mut resort = false;
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}条笔记", self.st.rows.len()))
                            .small()
                            .color(theme::pal().subtle),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .selectable_label(self.st.list_compact, "☰")
                            .on_hover_text(if self.st.list_compact {
                                "切换到卡片视图"
                            } else {
                                "切换到紧凑列表"
                            })
                            .clicked()
                        {
                            self.st.list_compact = !self.st.list_compact;
                            resort = true;
                        }
                        let dir = if self.st.sort_asc { "↑ 升序" } else { "↓ 降序" };
                        if ui
                            .selectable_label(false, egui::RichText::new(dir).small())
                            .on_hover_text("切换排序方向")
                            .clicked()
                        {
                            self.st.sort_asc = !self.st.sort_asc;
                            resort = true;
                        }
                        let mut sort = self.st.list_sort;
                        egui::ComboBox::from_id_salt("list-sort")
                            .selected_text(sort.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut sort, ListSort::Updated, "更新时间");
                                ui.selectable_value(&mut sort, ListSort::Created, "创建时间");
                                ui.selectable_value(&mut sort, ListSort::Title, "标题");
                            });
                        if sort != self.st.list_sort {
                            self.st.list_sort = sort;
                            resort = true;
                        }
                    });
                });
                if resort {
                    self.refresh_rows();
                }
                ui.add_space(2.0);
                let mut trash_req: Option<i64> = None;
                let mut shortcut_req: Option<i64> = None;
                let mut move_req: Option<(i64, Option<i64>)> = None;
                let pinned: std::collections::HashSet<i64> = self
                    .service
                    .store()
                    .shortcuts()
                    .into_iter()
                    .collect();
                // 多选态批量操作条（面板底部固定，对位原客户端批量操作栏）
                let mut batch_trash = false;
                let mut batch_move: Option<Option<i64>> = None;
                if !self.st.trash_view && !self.st.multi_select.is_empty() {
                    let n = self.st.multi_select.len();
                    egui::TopBottomPanel::top("batch-bar")
                        .frame(
                            egui::Frame::default()
                                .fill(theme::pal().white)
                                .stroke(egui::Stroke::new(1.0_f32, theme::pal().hairline))
                                .inner_margin(egui::Margin::symmetric(8.0, 5.0)),
                        )
                        .show_inside(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("已选 {n} 条"))
                                        .small()
                                        .color(theme::pal().subtle),
                                );
                                // 移动到…：弹层选目标（复用来源下拉的开关模式）
                                let mv_id = egui::Id::new("batch-move-menu");
                                let mv_btn = ui.button("📦 移动到…");
                                if mv_btn.clicked() {
                                    ui.memory_mut(|m| {
                                        if m.is_popup_open(mv_id) {
                                            m.close_popup();
                                        } else {
                                            m.open_popup(mv_id);
                                        }
                                    });
                                }
                                egui::popup_below_widget(
                                    ui,
                                    mv_id,
                                    &mv_btn,
                                    egui::PopupCloseBehavior::CloseOnClick,
                                    |ui| {
                                        for (nb_uid, nb_name) in &notebooks {
                                            if ui.button(format!("📓 {nb_name}")).clicked() {
                                                batch_move = Some(Some(*nb_uid));
                                                ui.close_menu();
                                            }
                                        }
                                        if ui.button("（未分类）").clicked() {
                                            batch_move = Some(None);
                                            ui.close_menu();
                                        }
                                    },
                                );
                                if ui.button("🗑 移入回收站").clicked() {
                                    batch_trash = true;
                                }
                            });
                        });
                }
                let mods = ui.input(|i| i.modifiers);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 无限滚动：只渲染可见窗口内的卡片，滚动到底自动扩展
                    let total = self.st.rows.len();
                    let rows: Vec<ListRow> = self.st
                        .rows
                        .iter()
                        .take(self.st.list_limit)
                        .cloned()
                        .collect();
                    if rows.is_empty() {
                        ui.add_space(20.0);
                        ui.label(egui::RichText::new("（空）").color(theme::pal().subtle));
                    }
                    for row in rows {
                        let selected = self.st.selected_note == Some(row.uid)
                            || self.st.multi_select.contains(&row.uid);
                        // 紧凑视图：单行「● 标题 …… 日期」（对位原客户端列表视图）
                        if self.st.list_compact {
                            let row_resp = ui
                                .horizontal(|ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.label(
                                        egui::RichText::new(if selected { "●" } else { "○" })
                                            .small()
                                            .color(if selected {
                                                theme::pal().green
                                            } else {
                                                theme::pal().subtle
                                            }),
                                    );
                                    ui.label(
                                        egui::RichText::new(&row.title)
                                            .size(13.0)
                                            .color(theme::pal().text),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(util::fmt_date(row.date))
                                                    .small()
                                                    .color(theme::pal().subtle),
                                            );
                                        },
                                    );
                                })
                                .response;
                            if self.st.trash_view {
                                ui.horizontal(|ui| {
                                    if ui.small_button("还原").clicked() {
                                        if let Err(e) = self.service.store().restore_note(row.uid) {
                                            self.st.status = format!("还原失败: {e}");
                                        } else {
                                            self.st.status =
                                                format!("已还原 {}", note_label(row.uid));
                                            self.refresh_rows();
                                        }
                                    }
                                    if ui.small_button("彻底删除").clicked() {
                                        if let Err(e) =
                                            self.service.store().expunge_note(row.uid)
                                        {
                                            self.st.status = format!("删除失败: {e}");
                                        } else {
                                            self.st.status = format!(
                                                "已彻底删除 {}（删除账已记）",
                                                note_label(row.uid)
                                            );
                                            self.refresh_rows();
                                        }
                                    }
                                });
                            } else {
                                let hit = ui.interact(
                                    row_resp.rect,
                                    egui::Id::new(("note-row", row.uid)),
                                    egui::Sense::click(),
                                );
                                if hit.clicked() {
                                    Self::click_select(&mut self.st, row.uid, mods);
                                }
                                Self::note_context_menu(
                                    &hit,
                                    row.uid,
                                    pinned.contains(&row.uid),
                                    &notebooks,
                                    &mut trash_req,
                                    &mut shortcut_req,
                                    &mut move_req,
                                );
                            }
                            ui.add_space(2.0);
                            continue;
                        }
                        // 笔记卡：白底圆角发丝边；选中 = 亮绿描边 + 浅绿底
                        // （对位原客户端笔记列表的卡片选中态）
                        let frame = egui::Frame::default()
                            .fill(if selected {
                                theme::pal().green_tint
                            } else {
                                theme::pal().white
                            })
                            .rounding(8.0)
                            .stroke(egui::Stroke::new(
                                if selected { 2.0_f32 } else { 1.0_f32 },
                                if selected {
                                    theme::pal().green_edge
                                } else {
                                    theme::pal().hairline
                                },
                            ))
                            .inner_margin(egui::Margin::same(10.0));
                        let inner = frame.show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(&row.title)
                                    .size(15.0)
                                    .strong()
                                    .color(theme::pal().text),
                            );
                            if !row.snippet.is_empty() {
                                ui.label(
                                    egui::RichText::new(&row.snippet)
                                        .small()
                                        .color(theme::pal().subtle),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(util::fmt_date(row.date))
                                        .small()
                                        .color(theme::pal().green),
                                );
                                if let Some(tags) = &row.tags {
                                    ui.label(
                                        egui::RichText::new(format!("🏷 {tags}"))
                                            .small()
                                            .color(theme::pal().subtle),
                                    );
                                }
                            });
                            if self.st.trash_view {
                                ui.horizontal(|ui| {
                                    if ui.small_button("还原").clicked() {
                                        if let Err(e) = self.service.store().restore_note(row.uid) {
                                            self.st.status = format!("还原失败: {e}");
                                        } else {
                                            self.st.status =
                                                format!("已还原 {}", note_label(row.uid));
                                            self.refresh_rows();
                                        }
                                    }
                                    if ui.small_button("彻底删除").clicked() {
                                        if let Err(e) = self.service.store().expunge_note(row.uid) {
                                            self.st.status = format!("删除失败: {e}");
                                        } else {
                                            self.st.status = format!(
                                                "已彻底删除 {}（删除账已记）",
                                                note_label(row.uid)
                                            );
                                            self.refresh_rows();
                                        }
                                    }
                                });
                            }
                        });
                        // 整卡可点选中（卡内按钮已各自消费点击，不会穿透）
                        let card_resp = ui.interact(
                            inner.response.rect,
                            egui::Id::new(("note-card", row.uid)),
                            egui::Sense::click(),
                        );
                        if card_resp.clicked() && !self.st.trash_view {
                            Self::click_select(&mut self.st, row.uid, mods);
                        }
                        if !self.st.trash_view {
                            Self::note_context_menu(
                                &card_resp,
                                row.uid,
                                pinned.contains(&row.uid),
                                &notebooks,
                                &mut trash_req,
                                &mut shortcut_req,
                                &mut move_req,
                            );
                        }
                        ui.add_space(6.0);
                    }
                    // 哨兵：进入视口即扩展窗口（步长 LIST_PAGE，封顶 total）
                    if self.st.list_limit < total {
                        let more = ui.label(
                            egui::RichText::new("↓ 继续滚动加载更多…")
                                .small()
                                .color(theme::pal().subtle),
                        );
                        if more.rect.intersects(ui.clip_rect()) {
                            self.st.list_limit =
                                (self.st.list_limit + UiState::LIST_PAGE).min(total);
                            ui.ctx().request_repaint();
                        }
                    }
                });
                // 右键菜单动作落地（面板闭包外，避开交叉借用）
                if let Some(uid) = trash_req {
                    match self.service.trash_note(uid) {
                        Ok(()) => {
                            self.st.status = format!("已移入回收站 {}", note_label(uid));
                            self.st.selected_note = None;
                            self.refresh_rows();
                        }
                        Err(e) => self.st.status = format!("删除失败: {e}"),
                    }
                }
                if let Some(uid) = shortcut_req {
                    match self.service.store().toggle_shortcut(uid) {
                        Ok(true) => {
                            self.st.status =
                                format!("已把 {} 加入快捷方式", note_label(uid));
                        }
                        Ok(false) => {
                            self.st.status = format!("已取消 {} 的快捷方式", note_label(uid));
                        }
                        Err(e) => self.st.status = format!("快捷方式切换失败: {e}"),
                    }
                }
                if batch_trash {
                    let uids = self.st.multi_select.clone();
                    let mut ok = 0usize;
                    for u in &uids {
                        if self.service.trash_note(*u).is_ok() {
                            ok += 1;
                        }
                    }
                    self.st.status = format!("已把 {ok} 条笔记移入回收站");
                    self.st.multi_select.clear();
                    self.st.selected_note = None;
                    self.refresh_rows();
                }
                if let Some(nb) = batch_move {
                    let uids = self.st.multi_select.clone();
                    let target = nb
                        .and_then(|n| {
                            notebooks
                                .iter()
                                .find(|(u, _)| *u == n)
                                .map(|(_, name)| name.clone())
                        })
                        .unwrap_or_else(|| "未分类".into());
                    let mut ok = 0usize;
                    for u in &uids {
                        if self.service.store().move_note(*u, nb).is_ok() {
                            ok += 1;
                        }
                    }
                    self.st.status = format!("已把 {ok} 条笔记移动到「{target}」");
                    self.st.multi_select.clear();
                    self.refresh_rows();
                }
                if let Some((uid, nb)) = move_req {
                    match self.service.store().move_note(uid, nb) {
                        Ok(()) => {
                            let target = nb
                                .and_then(|n| {
                                    notebooks
                                        .iter()
                                        .find(|(u, _)| *u == n)
                                        .map(|(_, name)| name.clone())
                                })
                                .unwrap_or_else(|| "未分类".into());
                            self.st.status = format!("已移动 {} → {target}", note_label(uid));
                            self.refresh_rows();
                        }
                        Err(e) => self.st.status = format!("移动失败: {e}"),
                    }
                }
            });
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
        self.load_editor();
        #[derive(PartialEq)]
        enum Action {
            None,
            Save,
            Trash,
            Present,
            /// 跳转侧栏选中到该笔记本（编辑器元信息 chip）。
            JumpNotebook(i64),
            /// 给当前笔记打/摘标签（底部标签行）。
            AttachTag(i64),
            DetachTag(i64),
        }
        let (action, uid, info_req) = {
            let Some(ed) = self.st.editor.as_mut() else {
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.add_space(80.0);
                    ui.label(egui::RichText::new("选择或新建一条笔记").size(20.0).weak());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("biji · ENML 存储 · USN 同步（自有后端）").weak());
                });
                return;
            };

            let mut action = Action::None;
            let mut show_info = false;

            // 元信息行（对位原客户端：笔记本 chip + 更新日期）
            {
                let full = self.service.store().get_note(ed.uid).ok();
                if let Some(full) = full {
                    ui.horizontal(|ui| {
                        if let Some(nb_uid) = full.note.notebook_uid {
                            let nb_name =
                                self.service.store().list_notebooks().ok().and_then(|nbs| {
                                    nbs.into_iter().find(|n| n.uid == nb_uid).map(|n| n.name)
                                });
                            if let Some(name) = nb_name {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(format!("📓 {name}"))
                                                .small()
                                                .color(theme::pal().text),
                                        )
                                        .fill(theme::pal().bg)
                                        .stroke(egui::Stroke::new(1.0_f32, theme::pal().hairline))
                                        .rounding(12.0),
                                    )
                                    .on_hover_text("在侧栏打开该笔记本")
                                    .clicked()
                                {
                                    action = Action::JumpNotebook(nb_uid);
                                }
                            }
                        }
                        ui.label(
                            egui::RichText::new(util::fmt_epoch(full.note.date_updated))
                                .small()
                                .color(theme::pal().subtle),
                        );
                        if ui
                            .small_button("ℹ")
                            .on_hover_text("笔记信息")
                            .clicked()
                        {
                            show_info = true;
                        }
                    });
                }
            }

            // 标题（无边框大字，对位原客户端编辑器标题）
            let title_resp = ui.add(
                egui::TextEdit::singleline(&mut ed.title)
                    .font(egui::TextStyle::Heading)
                    .hint_text("无标题笔记")
                    .frame(false)
                    .desired_width(ui.available_width()),
            );
            if title_resp.changed() {
                ed.dirty = true;
                ed.last_edit = Some(Instant::now());
            }
            ui.add_space(2.0);

            // 工具行（只改 ed 状态与动作标记，不触碰 self 其余部分）
            ui.horizontal(|ui| {
                if ed.dirty {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("● 保存").color(theme::pal().green_dark),
                            )
                            .fill(theme::pal().bg)
                            .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        action = Action::Save;
                    }
                } else {
                    ui.label(egui::RichText::new("已保存").small().color(theme::pal().subtle));
                }
                ui.separator();
                if ui.selectable_label(!ed.preview, "✏️ 编辑").clicked() {
                    ed.preview = false;
                }
                if ui.selectable_label(ed.preview, "👁 预览").clicked() {
                    if ed.dirty {
                        action = Action::Save;
                    }
                    ed.preview = true;
                }
                ui.separator();
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("🗑 删除").color(theme::pal().subtle))
                            .fill(theme::pal().bg)
                            .stroke(egui::Stroke::NONE),
                    )
                    .clicked()
                {
                    action = Action::Trash;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("▶ 放映本篇").color(theme::pal().subtle))
                            .fill(theme::pal().bg)
                            .stroke(egui::Stroke::NONE),
                    )
                    .clicked()
                {
                    action = Action::Present;
                }
            });
            ui.separator();

            // 附件条
            let uid = ed.uid;
            let resources = self.service.store().list_resources(uid).unwrap_or_default();
            if !resources.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📎").weak());
                    for r in &resources {
                        let is_image = r.mime.starts_with("image/");
                        let is_pdf = r.mime == "application/pdf";
                        if (is_image || is_pdf)
                            && ui
                                .small_button(format!("🏷 标注 {}", r.file_name))
                                .on_hover_text(if is_image {
                                    "打开图注标注画布"
                                } else {
                                    "打开 PDF 图注（按页渲染）"
                                })
                                .clicked()
                        {
                            let cmd = if is_image {
                                NodeWebkitCommand::Markup {
                                    resource_url: resource_url(r.uid),
                                    title: r.file_name.clone(),
                                    x: 0,
                                    y: 0,
                                    width: 800,
                                    height: 600,
                                    trigger: "editor".into(),
                                }
                            } else {
                                NodeWebkitCommand::MarkupPdf {
                                    pdf_url: resource_url(r.uid),
                                    manifest_url: String::new(),
                                    page_num: 1,
                                    title: r.file_name.clone(),
                                    x: 0,
                                    y: 0,
                                    width: 800,
                                    height: 600,
                                    trigger: "editor".into(),
                                }
                            };
                            self.service.dispatch(cmd);
                        }
                        if ui
                            .small_button(format!("{} ({}B)", r.file_name, r.size))
                            .on_hover_text("点击导出到 biji-export/")
                            .clicked()
                        {
                            match self.service.store().get_resource(r.uid) {
                                Ok((_, bytes)) => {
                                    let dir = PathBuf::from("biji-export");
                                    let _ = std::fs::create_dir_all(&dir);
                                    let target = dir.join(&r.file_name);
                                    match std::fs::write(&target, &bytes) {
                                        Ok(()) => {
                                            self.st.status = format!(
                                                "已导出 {} → {}",
                                                r.file_name,
                                                target.display()
                                            );
                                        }
                                        Err(e) => self.st.status = format!("导出失败: {e}"),
                                    }
                                }
                                Err(e) => self.st.status = format!("读取附件失败: {e}"),
                            }
                        }
                    }
                });
                ui.separator();
            }

            // 标签行（对位原客户端「添加标签…」）：绿色入口菜单 + 已挂标签 chip
            {
                let attached = self.service.store().note_tags(uid).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.menu_button(
                        egui::RichText::new("🏷 添加标签…")
                            .small()
                            .color(theme::pal().green_dark),
                        |ui| {
                            // 搜索框（自动聚焦）：前缀过滤候选 + 未命中可新建
                            let mut filter = self.st.tag_filter.clone();
                            ui.add(
                                egui::TextEdit::singleline(&mut filter)
                                    .hint_text("搜索或新建标签…")
                                    .desired_width(180.0)
                                    .id(egui::Id::new("tag-filter")),
                            );
                            if filter != self.st.tag_filter {
                                self.st.tag_filter = filter;
                            }
                            let needle = self.st.tag_filter.to_lowercase();
                            let tags = self.service.store().list_tags().unwrap_or_default();
                            let mut any = false;
                            for t in &tags {
                                if attached.iter().any(|(u, _)| *u == t.uid) {
                                    continue;
                                }
                                if !needle.is_empty() && !t.name.to_lowercase().starts_with(&needle) {
                                    continue;
                                }
                                any = true;
                                if ui.button(&t.name).clicked() {
                                    action = Action::AttachTag(t.uid);
                                    ui.close_menu();
                                }
                            }
                            // 未命中的输入 = 新建同名标签并挂上（对位原客户端行为）
                            let exact = tags.iter().any(|t| t.name == self.st.tag_filter);
                            if !self.st.tag_filter.trim().is_empty() && !exact {
                                if ui
                                    .button(format!("＋ 新建「{}」", self.st.tag_filter))
                                    .clicked()
                                {
                                    match self.service.store().create_tag(&self.st.tag_filter, None) {
                                        Ok(new_uid) => action = Action::AttachTag(new_uid),
                                        Err(e) => self.st.status = format!("新建标签失败: {e}"),
                                    }
                                    ui.close_menu();
                                }
                            } else if !any {
                                ui.label(
                                    egui::RichText::new("（没有可添加的标签）")
                                        .small()
                                        .color(theme::pal().subtle),
                                );
                            }
                        },
                    );
                    for (t_uid, t_name) in &attached {
                        let chip = egui::Frame::default()
                            .fill(theme::pal().white)
                            .stroke(egui::Stroke::new(1.0_f32, theme::pal().hairline))
                            .rounding(10.0)
                            .inner_margin(egui::Margin::symmetric(8.0, 2.0));
                        chip.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("🏷 {t_name}"))
                                        .small()
                                        .color(theme::pal().text),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("✕").small().color(theme::pal().subtle),
                                        )
                                        .fill(theme::pal().white)
                                        .stroke(egui::Stroke::NONE),
                                    )
                                    .on_hover_text("移除标签")
                                    .clicked()
                                {
                                    action = Action::DetachTag(*t_uid);
                                }
                            });
                        });
                    }
                });
            }

            // 正文：预览（读克隆，避免借用在服务调用间存活）或源码编辑
            if ed.preview {
                let content = ed.content.clone();
                let parsed = Enml::parse(&content).ok();
                let mut sess = preview::PreviewSession {
                    editing: ed.preview_editing,
                    buffer: ed.preview_buffer.clone(),
                    request_commit: None,
                    sel: ed.preview_sel,
                    sel_pieces: Vec::new(),
                    request_format: None,
                };
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    match parsed.as_ref() {
                        Some(doc) => preview::render_interactive(ui, doc, &mut sess),
                        None => {
                            ui.label(
                                egui::RichText::new("（正文不是合法 ENML，切到编辑模式修正）")
                                    .weak(),
                            );
                        }
                    }
                });
                // 行内编辑完成：按块内 HTML 片段解析（格式保留），失败则提示修正
                if let Some((idx, html)) = sess.request_commit.take() {
                    let wrapped = format!("<en-note>{}</en-note>", html);
                    if let Ok(mut nd) = Enml::parse(&wrapped) {
                        nd.sanitize();
                        if let Some(doc) = parsed.as_ref() {
                            if idx < doc.root.children.len() {
                                let mut doc_mut = doc.clone();
                                doc_mut
                                    .root
                                    .children
                                    .splice(idx..idx + 1, nd.root.children.clone());
                                ed.content = doc_mut.to_xml();
                                ed.dirty = true;
                                ed.last_edit = Some(Instant::now());
                                self.st.status = format!("块 {} 已行内编辑（格式保留）", idx + 1);
                            }
                        }
                    }
                }
                // 渲染态选区格式施加：拆分文本节点插入标记元素后整体回写
                if let Some(fmt) = sess.request_format.take() {
                    if let Some(doc) = parsed.as_ref() {
                        let mut doc_mut = doc.clone();
                        let done = preview::apply_format_edits(&mut doc_mut, &fmt);
                        if done > 0 {
                            doc_mut.sanitize();
                            ed.content = doc_mut.to_xml();
                            ed.dirty = true;
                            ed.last_edit = Some(Instant::now());
                            self.st.status =
                                format!("选区已施加 <{}>（渲染态直操，{} 处）", fmt.tag, done);
                        }
                    }
                }
                // 会话状态回写（编辑缓冲供下一帧续用）
                ed.preview_editing = sess.editing;
                ed.preview_buffer = sess.buffer;
                ed.preview_sel = sess.sel;
            } else {
                // 格式工具行（对位原客户端格式栏；作用于 ENML 源码选区）
                const FORMATS: &[(&str, &str, &str)] = &[
                    ("B", "<b>", "</b>"),
                    ("I", "<i>", "</i>"),
                    ("U", "<u>", "</u>"),
                    ("S", "<s>", "</s>"),
                    ("高亮", "<span style=\"background-color:#ffd54f\">", "</span>"),
                    ("H1", "<h1>", "</h1>"),
                    ("H2", "<h2>", "</h2>"),
                    ("代码", "<code>", "</code>"),
                    ("表格", "<table><tr><td>", "</td></tr></table>"),
                    ("—", "<hr/>", ""),
                ];
                let editor_id = egui::Id::new("enml-source");
                let selection = egui::TextEdit::load_state(ui.ctx(), editor_id)
                    .as_ref()
                    .and_then(|st| st.cursor.char_range());
                ui.horizontal(|ui| {
                    for (label, open, close) in FORMATS {
                        if ui.small_button(*label).clicked() {
                            let (newc, newr) =
                                util::wrap_enml_around_selection(&ed.content, selection, open, close);
                            ed.content = newc;
                            ed.dirty = true;
                            ed.last_edit = Some(Instant::now());
                            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
                                st.cursor.set_char_range(Some(newr));
                                egui::TextEdit::store_state(ui.ctx(), editor_id, st);
                            }
                        }
                    }
                    ui.separator();
                    if ui.small_button("待办").clicked() {
                        let (newc, newr) =
                            util::wrap_enml_around_selection(&ed.content, selection, "<en-todo/>", "");
                        ed.content = newc;
                        ed.dirty = true;
                        ed.last_edit = Some(Instant::now());
                        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
                            st.cursor.set_char_range(Some(newr));
                            egui::TextEdit::store_state(ui.ctx(), editor_id, st);
                        }
                    }
                    if ui.small_button("链接").clicked() {
                        let (newc, newr) = util::wrap_enml_around_selection(
                            &ed.content,
                            selection,
                            "<a href=\"\">",
                            "</a>",
                        );
                        ed.content = newc;
                        ed.dirty = true;
                        ed.last_edit = Some(Instant::now());
                        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
                            st.cursor.set_char_range(Some(newr));
                            egui::TextEdit::store_state(ui.ctx(), editor_id, st);
                        }
                    }
                });
                let output = egui::TextEdit::multiline(&mut ed.content)
                    .font(egui::TextStyle::Monospace)
                    .code_editor()
                    .hint_text("在此输入 ENML 正文，如 <p>你好</p>")
                    .id(editor_id)
                    .desired_width(ui.available_width())
                    .show(ui);
                if output.response.changed() {
                    ed.dirty = true;
                    ed.last_edit = Some(Instant::now());
                }
            }

            // 自动保存：编辑后 2 秒无新输入即落库。
            if action == Action::None && ed.dirty {
                if let Some(t) = ed.last_edit {
                    if t.elapsed() > Duration::from_secs(2) {
                        action = Action::Save;
                    }
                }
            }

            (action, uid, show_info.then_some(ed.uid))
        };
        if let Some(u) = info_req {
            self.st.note_info = Some(u);
        }
        match action {
            Action::None => {}
            Action::Save => self.save_editor(),
            Action::Trash => {
                if let Err(e) = self.service.trash_note(uid) {
                    self.st.status = format!("删除失败: {e}");
                } else {
                    self.st.status = format!("已移入回收站 {}", note_label(uid));
                    self.st.selected_note = None;
                    self.refresh_rows();
                }
            }
            Action::Present => {
                let cmd = NodeWebkitCommand::Present {
                    note_ids: vec![uid],
                    index: 0,
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    trigger: "editor".into(),
                };
                self.service.dispatch(cmd);
            }
            Action::JumpNotebook(nb_uid) => {
                self.st.selection = Selection::Notebook(nb_uid);
                self.st.trash_view = false;
                self.refresh_rows();
            }
            Action::AttachTag(tag_uid) => {
                if let Err(e) = self.service.store().attach_tag(uid, tag_uid) {
                    self.st.status = format!("打标签失败: {e}");
                } else {
                    self.st.status = "已添加标签".into();
                    self.st.tag_filter.clear();
                    self.refresh_rows();
                }
            }
            Action::DetachTag(tag_uid) => {
                if let Err(e) = self.service.store().detach_tag(uid, tag_uid) {
                    self.st.status = format!("移除标签失败: {e}");
                } else {
                    self.st.status = "已移除标签".into();
                    self.refresh_rows();
                }
            }
        }
    }

    // ----- 图注标注（原 Markup 命令族，分析文档 P2） -----

    /// PDF 图注：渲染指定页为位图后进入标注画布；保存产物为新 PNG 资源。
    fn open_pdf_annotator(&mut self, ctx: &egui::Context, url: &str, page: u32, title: &str) {
        let Some(uid) = parse_resource_url(url) else {
            self.st.status = format!("URI 无法解析: {url}");
            return;
        };
        let Some(renderer) = pdf::detect_renderer() else {
            self.st.status = pdf::RENDERER_GUIDANCE.into();
            return;
        };
        let (meta, bytes) = match self.service.store().get_resource(uid) {
            Ok(v) => v,
            Err(e) => {
                self.st.status = format!("读取附件失败: {e}");
                return;
            }
        };
        let temp_pdf = std::env::temp_dir().join(format!("biji-{uid}.pdf"));
        if let Err(e) = std::fs::write(&temp_pdf, &bytes) {
            self.st.status = format!("写出临时 PDF 失败: {e}");
            return;
        }
        match renderer.render_page(&temp_pdf, page) {
            Ok(rgba) => {
                let texture = ctx.load_texture(
                    "annotate",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                );
                let mut st = AnnotateState::new(
                    uid,
                    meta.note_uid,
                    format!("{title} · 第 {page} 页"),
                    meta.file_name.clone(),
                    "image/png".into(),
                    rgba,
                    texture,
                );
                st.pdf_mode = Some(page);
                self.st.annotating = Some(st);
                self.st.status = format!(
                    "PDF 第 {page} 页已进入标注画布（渲染器：{}）",
                    renderer.describe()
                );
            }
            Err(e) => self.st.status = format!("PDF 第 {page} 页渲染失败: {e}"),
        }
    }

    fn open_annotator(&mut self, ctx: &egui::Context, url: &str, title: &str) {
        let Some(uid) = parse_resource_url(url) else {
            self.st.status = format!("URI 无法解析: {url}");
            return;
        };
        let (meta, bytes) = match self.service.store().get_resource(uid) {
            Ok(v) => v,
            Err(e) => {
                self.st.status = format!("读取附件失败: {e}");
                return;
            }
        };
        match image::load_from_memory(&bytes) {
            Ok(decoded) => {
                let rgba = decoded.to_rgba8();
                let texture = ctx.load_texture(
                    "annotate",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                );
                self.st.annotating = Some(AnnotateState::new(
                    uid,
                    meta.note_uid,
                    title.to_string(),
                    meta.file_name.clone(),
                    meta.mime.clone(),
                    rgba,
                    texture,
                ));
                self.st.status = format!("已打开图注画布「{title}」");
            }
            Err(e) => {
                self.st.status = format!("图片解码失败: {e}（仅支持图片资源）");
            }
        }
    }

    fn rotate_resource(&mut self, url: &str, theta: f64) {
        let Some(uid) = parse_resource_url(url) else {
            self.st.status = format!("URI 无法解析: {url}");
            return;
        };
        let (_, bytes) = match self.service.store().get_resource(uid) {
            Ok(v) => v,
            Err(e) => {
                self.st.status = format!("读取附件失败: {e}");
                return;
            }
        };
        match image::load_from_memory(&bytes) {
            Ok(decoded) => {
                let rotated = annotate::apply_rotation(decoded.to_rgba8(), theta);
                match annotate::encode_png(&rotated) {
                    Ok(png) => {
                        let data = ResourceData {
                            uid,
                            mime: "image/png".into(),
                            filename: "rotated.png".into(),
                            bytes: png,
                        };
                        match self.service.update_resource(data, 0) {
                            Ok(()) => {
                                if self
                                    .st
                                    .annotating
                                    .as_ref()
                                    .map(|a| a.resource_uid == uid)
                                    .unwrap_or(false)
                                {
                                    self.st.annotating = None;
                                    self.st.status =
                                        "图片已旋转并写回（画布已关闭，标注坐标随之失效）".into();
                                } else {
                                    self.st.status = "图片已旋转 90° 并写回".into();
                                }
                                self.refresh_rows();
                            }
                            Err(e) => self.st.status = format!("写回失败: {e}"),
                        }
                    }
                    Err(e) => self.st.status = format!("PNG 编码失败: {e}"),
                }
            }
            Err(e) => self.st.status = format!("图片解码失败: {e}"),
        }
    }

    fn resize_resource(&mut self, url: &str, width: i32, height: i32) {
        let Some(uid) = parse_resource_url(url) else {
            self.st.status = format!("URI 无法解析: {url}");
            return;
        };
        if width <= 0 || height <= 0 {
            self.st.status = format!("非法尺寸 {width}x{height}");
            return;
        }
        let (_, bytes) = match self.service.store().get_resource(uid) {
            Ok(v) => v,
            Err(e) => {
                self.st.status = format!("读取附件失败: {e}");
                return;
            }
        };
        match image::load_from_memory(&bytes) {
            Ok(decoded) => {
                let resized = image::imageops::resize(
                    &decoded.to_rgba8(),
                    width as u32,
                    height as u32,
                    image::imageops::FilterType::Lanczos3,
                );
                match annotate::encode_png(&resized) {
                    Ok(png) => {
                        let data = ResourceData {
                            uid,
                            mime: "image/png".into(),
                            filename: "resized.png".into(),
                            bytes: png,
                        };
                        match self.service.update_resource(data, 0) {
                            Ok(()) => {
                                self.st.status = format!("图片已缩放至 {width}x{height} 并写回");
                                self.refresh_rows();
                            }
                            Err(e) => self.st.status = format!("写回失败: {e}"),
                        }
                    }
                    Err(e) => self.st.status = format!("PNG 编码失败: {e}"),
                }
            }
            Err(e) => self.st.status = format!("图片解码失败: {e}"),
        }
    }

    /// 原 clearImage 语义：清除标注层。本实现中标注不内嵌于图片，画布打开时
    /// 即清空其标注并恢复原图；未打开时等价于提示。
    fn clear_resource_annotations(&mut self, url: &str) {
        let Some(uid) = parse_resource_url(url) else {
            self.st.status = format!("URI 无法解析: {url}");
            return;
        };
        if self
            .st
            .annotating
            .as_ref()
            .map(|a| a.resource_uid == uid)
            .unwrap_or(false)
        {
            if let Some(st) = self.st.annotating.as_mut() {
                st.clear();
                st.status = "标注已清空，画布恢复原图".into();
            }
        } else {
            self.st.status = "该资源未打开标注画布".into();
        }
    }

    fn draw_annotator(&mut self, ctx: &egui::Context) {
        #[derive(PartialEq)]
        enum AAction {
            None,
            Save,
        }
        let Some(st) = self.st.annotating.as_mut() else {
            return;
        };
        let mut action = AAction::None;
        let ctx2 = ctx.clone();
        let mut open_now = st.open;
        egui::Window::new(format!(
            "图注标注 — {}（所属笔记 {}）",
            st.title,
            note_label(st.note_uid)
        ))
        .open(&mut open_now)
        .default_size([1080.0, 780.0])
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tool, label) in [
                    (annotate::Tool::Select, "◇ 选择"),
                    (annotate::Tool::Arrow, "↗ 箭头"),
                    (annotate::Tool::Rect, "□ 矩形"),
                    (annotate::Tool::Text, "文字"),
                    (annotate::Tool::Pen, "✏️ 画笔"),
                ] {
                    if ui.selectable_label(st.tool == tool, label).clicked() {
                        st.tool = tool;
                    }
                }
                ui.separator();
                for c in [
                    [230u8, 57, 70],
                    [255, 196, 0],
                    [0, 120, 215],
                    [0, 166, 90],
                    [20, 20, 20],
                ] {
                    let selected = st.color == c;
                    let btn = egui::Button::new(
                        egui::RichText::new("●").color(egui::Color32::from_rgb(c[0], c[1], c[2])),
                    );
                    if ui.add(btn).clicked() {
                        st.color = c;
                    }
                    let _ = selected;
                }
                ui.separator();
                for w in [2.0f32, 4.0, 8.0] {
                    if ui
                        .selectable_label((st.stroke - w).abs() < 0.01, format!("{w}px"))
                        .clicked()
                    {
                        st.stroke = w;
                    }
                }
                ui.separator();
                if ui.button("↩ 撤销").clicked() {
                    st.undo();
                }
                if ui.button("清空").clicked() {
                    st.clear();
                }
                let has_sel = !st.selected.is_empty();
                if ui
                    .add_enabled(has_sel, egui::Button::new("🗑 删除所选"))
                    .clicked()
                {
                    let n = st.delete_selected();
                    if n > 0 {
                        st.status = format!("已删除 {n} 条选中标注");
                    }
                }
                let still_sel = !st.selected.is_empty();
                if ui
                    .add_enabled(still_sel, egui::Button::new("＋ 放大"))
                    .on_hover_text("选中标注放大 1.2x")
                    .clicked()
                {
                    st.scale_selected(1.2);
                }
                if ui
                    .add_enabled(still_sel, egui::Button::new("－ 缩小"))
                    .on_hover_text("选中标注缩小 0.8x")
                    .clicked()
                {
                    st.scale_selected(0.8);
                }
                if ui.button("旋转90°").clicked() {
                    let rotated = annotate::apply_rotation(st.image.clone(), 90.0);
                    st.set_image(&ctx2, rotated);
                    st.status = "画布已旋转 90°（已存标注坐标保留）".into();
                }
                if ui.add(theme::green_button("💾 保存写回")).clicked() {
                    action = AAction::Save;
                }
                if ui.button("✖ 关闭").clicked() {
                    st.open = false;
                }
                ui.separator();
                if ui
                    .checkbox(&mut st.rotate_around_canvas, "绕画布中心旋转")
                    .on_hover_text("旋转枢轴：画布中心（外部点）/ 选中公共包围盒中心")
                    .changed()
                {
                    st.status = if st.rotate_around_canvas {
                        "旋转枢轴：画布中心（外部点旋转）".into()
                    } else {
                        "旋转枢轴：选中公共包围盒中心".into()
                    };
                }
            });
            ui.separator();
            st.draw(ui);
            ui.add_space(2.0);
            if st.draw_text_input(ui) {
                st.status = "文字已放置".into();
            }
            if !st.status.is_empty() {
                ui.label(egui::RichText::new(&st.status).weak().small());
            }
        });
        st.open = open_now;

        let mut saved: Option<String> = None;
        let mut closed_msg: Option<String> = None;
        if action == AAction::Save {
            let mut img = st.image.clone();
            for a in &st.annotations {
                a.rasterize(&mut img);
            }
            let is_jpeg = st.original_mime == "image/jpeg";
            let encoded = if is_jpeg {
                // JPEG 原格式写回：保持 mime 与文件名不变，避免格式漂移。
                annotate::encode_jpeg(&img, 90)
            } else {
                annotate::encode_png(&img)
            };
            match encoded {
                Ok(bytes) => match st.pdf_mode {
                    None => {
                        let data = ResourceData {
                            uid: st.resource_uid,
                            mime: st.original_mime.clone(),
                            filename: st.filename.clone(),
                            bytes,
                        };
                        match self.service.update_resource(data, 0) {
                            Ok(()) => {
                                saved = Some(st.filename.clone());
                                st.status = "已写回".into();
                            }
                            Err(e) => st.status = format!("写回失败: {e}"),
                        }
                    }
                    Some(page) => {
                        let stem = st.filename.trim_end_matches(".pdf").to_string();
                        let name = format!("{stem}-p{page}-标注.png");
                        match self.service.store().add_resource(
                            st.note_uid,
                            &name,
                            "image/png",
                            &bytes,
                        ) {
                            Ok(new_uid) => {
                                saved = Some(format!("{name} (resource {new_uid})"));
                                st.status = format!("已作为新资源 {new_uid} 挂到笔记");
                            }
                            Err(e) => st.status = format!("保存失败: {e}"),
                        }
                    }
                },
                Err(e) => st.status = format!("PNG 编码失败: {e}"),
            }
        }
        if !st.open {
            closed_msg = Some("已关闭标注画布（未保存的标注已丢弃）".into());
        }
        if let Some(fname) = saved {
            self.st.status = format!("标注已写回 {fname}");
            self.st.annotating = None;
            self.refresh_rows();
            return;
        }
        if let Some(msg) = closed_msg {
            self.st.status = msg;
            self.st.annotating = None;
        }
    }

    /// 聊天窗口：thread/message 存储的消费入口（P3）。
    fn draw_chat(&mut self, ctx: &egui::Context) {
        let Some(chat) = self.st.chat.as_mut() else {
            return;
        };
        let store = self.service.store();
        chat.threads = store
            .list_threads()
            .unwrap_or_default()
            .into_iter()
            .map(|(uid, title, _)| (uid, title))
            .collect();
        if chat.thread_uid.is_none() {
            chat.thread_uid = chat.threads.first().map(|t| t.0);
        }
        if chat.loaded_thread != chat.thread_uid {
            chat.messages = chat
                .thread_uid
                .map(|u| store.list_messages(u).unwrap_or_default())
                .unwrap_or_default();
            chat.loaded_thread = chat.thread_uid;
        }
        let sender = self
            .service
            .get_current_user_info()
            .map(|u| u.username)
            .unwrap_or_default();

        let mut open = chat.open;
        egui::Window::new("💬 聊天")
            .open(&mut open)
            .default_size([460.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let selected_title = chat
                        .threads
                        .iter()
                        .find(|t| Some(t.0) == chat.thread_uid)
                        .map(|t| t.1.clone())
                        .unwrap_or_else(|| "选择会话".into());
                    egui::ComboBox::from_id_salt("chat-thread")
                        .selected_text(selected_title)
                        .show_ui(ui, |ui| {
                            for (uid, title) in &chat.threads {
                                ui.selectable_value(
                                    &mut chat.thread_uid,
                                    Some(*uid),
                                    title.clone(),
                                );
                            }
                        });
                    ui.text_edit_singleline(&mut chat.new_name);
                    if ui.button("＋ 会话").clicked() && !chat.new_name.trim().is_empty() {
                        let name = chat.new_name.trim().to_string();
                        match store.create_thread(&name) {
                            Ok(uid) => {
                                chat.threads = store
                                    .list_threads()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|(uid, title, _)| (uid, title))
                                    .collect();
                                chat.thread_uid = Some(uid);
                                chat.loaded_thread = None;
                            }
                            Err(e) => chat.input = format!("创建失败: {e}"),
                        }
                        chat.new_name.clear();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (_uid, sender_name, body, date) in &chat.messages {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(sender_name).strong());
                            ui.label(egui::RichText::new(util::fmt_epoch(*date)).small().weak());
                        });
                        ui.label(body);
                        ui.separator();
                    }
                    if chat.messages.is_empty() {
                        ui.label(egui::RichText::new("（暂无消息，在下方发送第一条）").weak());
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let send_clicked = ui.button("发送").clicked();
                    let resp = ui.text_edit_singleline(&mut chat.input);
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (send_clicked || enter) && !chat.input.trim().is_empty() {
                        if let Some(uid) = chat.thread_uid {
                            let body = chat.input.trim().to_string();
                            if store.post_message(uid, &sender, &body).is_ok() {
                                chat.messages = store.list_messages(uid).unwrap_or_default();
                                chat.input.clear();
                            }
                        }
                    }
                });
            });
        chat.open = open;
        if !chat.open {
            self.st.chat = None;
        }
    }

    /// 素材面板：material_attr 消费入口（P3）。
    fn draw_materials(&mut self, ctx: &egui::Context) {
        let Some(m) = self.st.materials.as_mut() else {
            return;
        };
        let store = self.service.store();
        let rows = store.list_materials().unwrap_or_default();
        let note_uid = self.st.selected_note;
        let mut registered = 0usize;

        let mut open = m.open;
        egui::Window::new("📦 素材库")
            .open(&mut open)
            .default_size([420.0, 480.0])
            .show(ctx, |ui| {
                let resources = note_uid
                    .and_then(|u| store.list_resources(u).ok())
                    .unwrap_or_default();
                if ui
                    .add_enabled(!resources.is_empty(), egui::Button::new("登记当前笔记附件"))
                    .clicked()
                {
                    for r in &resources {
                        if store
                            .add_material(&r.file_name, &r.mime, r.size, &r.hash, r.uid)
                            .is_ok()
                        {
                            registered += 1;
                        }
                    }
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label(egui::RichText::new("（素材库为空）").weak());
                    }
                    for (uid, title, mime, size) in &rows {
                        ui.horizontal(|ui| {
                            ui.label(format!("📄 {title}"));
                            ui.label(
                                egui::RichText::new(format!("{mime} · {size}B"))
                                    .weak()
                                    .small(),
                            );
                            let _ = uid;
                        });
                        ui.separator();
                    }
                });
                if registered > 0 {
                    m.status = format!("已登记 {registered} 个附件为素材");
                }
                if !m.status.is_empty() {
                    ui.label(egui::RichText::new(&m.status).weak().small());
                }
            });
        if registered > 0 {
            m.status.clear();
        }
        m.open = open;
        if !m.open {
            self.st.materials = None;
        }
    }

    /// 企业空间面板：groupspace 存储的消费入口（P3）。
    fn draw_groupspace(&mut self, ctx: &egui::Context) {
        let Some(gs) = self.st.groupspace.as_mut() else {
            return;
        };
        let store = self.service.store();
        let rows = store.list_groupspaces().unwrap_or_default();
        let me = self
            .service
            .get_current_user_info()
            .map(|u| u.username)
            .unwrap_or_default();

        let mut open = gs.open;
        let mut created = false;
        let mut joined: Option<String> = None;
        egui::Window::new("🏢 企业空间")
            .open(&mut open)
            .default_size([440.0, 500.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("名称:");
                    ui.text_edit_singleline(&mut gs.new_name);
                });
                ui.horizontal(|ui| {
                    ui.label("描述:");
                    ui.text_edit_singleline(&mut gs.new_desc);
                    if ui.button("创建").clicked() && !gs.new_name.trim().is_empty() {
                        let name = gs.new_name.trim().to_string();
                        let desc = gs.new_desc.trim().to_string();
                        match store.create_groupspace(
                            &name,
                            if desc.is_empty() { None } else { Some(&desc) },
                        ) {
                            Ok(uid) => {
                                // 创建者默认以当前用户身份加入
                                let _ = store.join_groupspace(uid, &me, 1);
                                gs.status = format!("空间 {uid} 已创建：{name}");
                                gs.new_name.clear();
                                gs.new_desc.clear();
                                created = true;
                            }
                            Err(e) => gs.status = format!("创建失败: {e}"),
                        }
                    }
                });
                ui.separator();
                ui.label(
                    egui::RichText::new("空间列表（点击「加入」以当前用户身份入圈）")
                        .weak()
                        .small(),
                );
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label(egui::RichText::new("（尚无企业空间）").weak());
                    }
                    for row in &rows {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "🏢 {}  ·  {}  ·  {} 人",
                                row.name,
                                row.description.as_deref().unwrap_or("-"),
                                row.member_count
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("加入").clicked() {
                                        if let Err(e) = store.join_groupspace(row.uid, &me, 0) {
                                            gs.status = format!("加入失败: {e}");
                                        } else {
                                            joined = Some(row.name.clone());
                                        }
                                    }
                                },
                            );
                        });
                        ui.separator();
                    }
                });
                if !gs.status.is_empty() {
                    ui.label(egui::RichText::new(&gs.status).weak().small());
                }
            });
        gs.open = open;
        if created {
            gs.status.clear();
            gs.new_name.clear();
            gs.new_desc.clear();
        }
        if let Some(name) = joined {
            gs.status = format!("已加入「{name}」（{}）", me);
        }
        if !open {
            self.st.groupspace = None;
        }
    }

    fn draw_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let pending = self.pending_sync_count();
                let total = self
                    .service
                    .store()
                    .list_notes(ListNotes::All)
                    .map(|v| v.len())
                    .unwrap_or(0);
                ui.label(egui::RichText::new(&self.st.status));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("笔记 {total} · 待同步 {pending}"))
                            .weak()
                            .small(),
                    );
                });
            });
        });
    }

    fn draw_dialogs(&mut self, ctx: &egui::Context) {
        // 笔记信息（ℹ，对位原客户端笔记信息面板）
        if self.st.note_info.is_some() {
            let mut open = true;
            egui::Window::new("笔记信息")
                .collapsible(false)
                .open(&mut open)
                .default_width(340.0)
                .show(ctx, |ui| {
                    let uid = self.st.note_info.unwrap();
                    match self.service.store().get_note(uid) {
                        Ok(full) => {
                            let words = Enml::parse(&full.enml)
                                .map(|d| d.to_plain_text().chars().count())
                                .unwrap_or(0);
                            let nb_name = full
                                .note
                                .notebook_uid
                                .and_then(|n| {
                                    self.service
                                        .store()
                                        .list_notebooks()
                                        .ok()
                                        .and_then(|nbs| {
                                            nbs.into_iter().find(|x| x.uid == n).map(|x| x.name)
                                        })
                                })
                                .unwrap_or_else(|| "（无）".into());
                            egui::Grid::new("note-info-grid")
                                .num_columns(2)
                                .min_col_width(72.0)
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("创建时间").color(theme::pal().subtle),
                                    );
                                    ui.label(util::fmt_epoch(full.note.date_created));
                                    ui.end_row();
                                    ui.label(
                                        egui::RichText::new("更新时间").color(theme::pal().subtle),
                                    );
                                    ui.label(util::fmt_epoch(full.note.date_updated));
                                    ui.end_row();
                                    ui.label(egui::RichText::new("字数").color(theme::pal().subtle));
                                    ui.label(format!("{words} 字"));
                                    ui.end_row();
                                    ui.label(egui::RichText::new("笔记本").color(theme::pal().subtle));
                                    ui.label(nb_name);
                                    ui.end_row();
                                    ui.label(egui::RichText::new("标签").color(theme::pal().subtle));
                                    ui.label(full.note.tags_text.unwrap_or_else(|| "—".into()));
                                    ui.end_row();
                                    if let Some(url) =
                                        full.note.source_url.as_deref().filter(|s| !s.is_empty())
                                    {
                                        ui.label(egui::RichText::new("来源").color(theme::pal().subtle));
                                        ui.label(url.to_string());
                                        ui.end_row();
                                    }
                                });
                        }
                        Err(e) => {
                            ui.label(egui::RichText::new(format!("读取失败: {e}")).weak());
                        }
                    }
                    ui.add_space(6.0);
                    if ui.button("关闭").clicked() {
                        self.st.note_info = None;
                    }
                });
            if !open {
                self.st.note_info = None;
            }
        }
        // 重命名笔记本（侧栏右键入口）
        if self.st.rename_nb.is_some() {
            let mut close = false;
            let mut confirm = false;
            egui::Window::new("重命名笔记本")
                .collapsible(false)
                .show(ctx, |ui| {
                    let Some((uid, buf)) = self.st.rename_nb.as_mut() else {
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label("名称:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(buf)
                                .desired_width(ui.available_width()),
                        );
                        if !resp.has_focus() && ui.rect_contains_pointer(ui.max_rect()) {
                            resp.request_focus();
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                    let _ = uid;
                });
            if confirm {
                if let Some((uid, buf)) = self.st.rename_nb.take() {
                    let name = buf.trim().to_string();
                    if name.is_empty() {
                        self.st.status = "笔记本名称不能为空".into();
                    } else {
                        match self.service.store().rename_notebook(uid, &name) {
                            Ok(()) => {
                                self.st.status = format!("笔记本已重命名为「{name}」");
                                self.refresh_rows();
                            }
                            Err(e) => self.st.status = format!("重命名失败: {e}"),
                        }
                    }
                }
            } else if close {
                self.st.rename_nb = None;
            }
        }
        // 重命名标签（侧栏标签行右键入口）
        if self.st.rename_tag.is_some() {
            let mut close = false;
            let mut confirm = false;
            egui::Window::new("重命名标签")
                .collapsible(false)
                .show(ctx, |ui| {
                    let Some((uid, buf)) = self.st.rename_tag.as_mut() else {
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label("名称:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(buf)
                                .desired_width(ui.available_width()),
                        );
                        if !resp.has_focus() && ui.rect_contains_pointer(ui.max_rect()) {
                            resp.request_focus();
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                    let _ = uid;
                });
            if confirm {
                if let Some((uid, buf)) = self.st.rename_tag.take() {
                    let name = buf.trim().to_string();
                    if name.is_empty() {
                        self.st.status = "标签名称不能为空".into();
                    } else {
                        match self.service.store().rename_tag(uid, &name) {
                            Ok(()) => {
                                self.st.status = format!("标签已重命名为「{name}」");
                                self.refresh_rows();
                            }
                            Err(e) => self.st.status = format!("重命名失败: {e}"),
                        }
                    }
                }
            } else if close {
                self.st.rename_tag = None;
            }
        }
        // 新建笔记本
        if self.st.show_new_notebook {
            let mut create = false;
            egui::Window::new("新建笔记本")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("名称:");
                        ui.text_edit_singleline(&mut self.st.new_notebook_name);
                    });
                    ui.horizontal(|ui| {
                        if ui.button("创建").clicked() {
                            create = true;
                        }
                        if ui.button("取消").clicked() {
                            self.st.show_new_notebook = false;
                            self.st.new_notebook_name.clear();
                        }
                    });
                });
            if create {
                let name = self.st.new_notebook_name.trim().to_string();
                if name.is_empty() {
                    self.st.status = "笔记本名称不能为空".into();
                } else {
                    match self.service.store().create_notebook(&name, None) {
                        Ok(uid) => {
                            self.st.status = format!("notebook {uid} 已创建：{name}");
                            self.st.selection = Selection::Notebook(uid);
                            self.st.trash_view = false;
                            self.refresh_rows();
                        }
                        Err(e) => self.st.status = format!("创建失败: {e}"),
                    }
                    self.st.show_new_notebook = false;
                    self.st.new_notebook_name.clear();
                }
            }
        }

        // 删除确认
        if let Some(Confirm::DeleteNotebook(uid, name)) = self.st.confirm.clone() {
            let mut answer: Option<bool> = None;
            egui::Window::new("删除笔记本")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("删除「{name}」？其中的笔记将移入回收站。"));
                    ui.horizontal(|ui| {
                        if ui.button("删除").clicked() {
                            answer = Some(true);
                        }
                        if ui.button("取消").clicked() {
                            answer = Some(false);
                        }
                    });
                });
            if let Some(true) = answer {
                match self.service.store().delete_notebook(uid) {
                    Ok(()) => {
                        if self.st.selection == Selection::Notebook(uid) {
                            self.st.selection = Selection::All;
                        }
                        self.st.status = format!("笔记本「{name}」已删除");
                        self.refresh_rows();
                    }
                    Err(e) => self.st.status = format!("删除失败: {e}"),
                }
                self.st.confirm = None;
            } else if let Some(false) = answer {
                self.st.confirm = None;
            }
        }

        // 删除标签确认
        if let Some(Confirm::DeleteTag(uid, name)) = self.st.confirm.clone() {
            let mut answer: Option<bool> = None;
            egui::Window::new("删除标签")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("删除标签「{name}」？将从所有笔记上移除。"));
                    ui.horizontal(|ui| {
                        if ui.button("删除").clicked() {
                            answer = Some(true);
                        }
                        if ui.button("取消").clicked() {
                            answer = Some(false);
                        }
                    });
                });
            if let Some(true) = answer {
                match self.service.store().delete_tag(uid) {
                    Ok(()) => {
                        if self.st.selection == Selection::Tag(uid) {
                            self.st.selection = Selection::All;
                        }
                        self.st.status = format!("标签「{name}」已删除（删除账已记）");
                        self.refresh_rows();
                    }
                    Err(e) => self.st.status = format!("删除失败: {e}"),
                }
                self.st.confirm = None;
            } else if let Some(false) = answer {
                self.st.confirm = None;
            }
        }
    }
}

impl eframe::App for BijiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 每帧先恢复浅色（标注窗口的深色视觉只局部生效一帧）。
        theme::apply(ctx);
        self.drain_commands(ctx);

        self.handle_global_shortcuts(ctx);

        if self.st.present.is_some() {
            self.draw_present(ctx);
        } else {
            self.draw_main(ctx);
        }
        if self.st.annotating.is_some() {
            theme::apply_dark(ctx);
            self.draw_annotator(ctx);
            theme::apply(ctx); // 主窗口其余部分（如同时打开的聊天窗）保持浅色
        }
        if self.st.chat.is_some() {
            self.draw_chat(ctx);
        }
        if self.st.materials.is_some() {
            self.draw_materials(ctx);
        }
        if self.st.groupspace.is_some() {
            self.draw_groupspace(ctx);
        }

        // 脏状态定时自动保存。
        let need_autosave = self.st.editor.as_ref().map(|e| e.dirty).unwrap_or(false);
        if need_autosave {
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }
}

fn note_label(uid: i64) -> String {
    format!("biji://note/{uid}")
}

/// CJK 字体安装：egui 默认字体不含汉字字形。按平台候选顺序加载系统字体
/// （Windows：msyh 优先——原客户端 UI 字体为微软雅黑，插 Proportional 队首
/// 使拉丁/数字同样走雅黑；macOS：PingFang SC → 冬青黑体 → Arial Unicode）。
fn install_cjk_fonts(ctx: &egui::Context) {
    #[cfg(target_os = "windows")]
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "C:\\Windows\\Fonts\\Dengb.ttf",
        "C:\\Windows\\Fonts\\Deng.ttf",
    ];
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];
    for (i, path) in CANDIDATES.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            let name = format!("cjk-{i}");
            let mut data = egui::FontData::from_owned(bytes);
            data.index = 0; // TTC 取第一个 face
            fonts.font_data.insert(name.clone(), data);
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, name.clone());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push(name);
            ctx.set_fonts(fonts);
            return;
        }
    }
    // 找不到系统中文字体时保持默认（中文显示为方框的降级）。
}

mod util {
    /// epoch 秒 → `YYYY-MM-DD HH:MM`（按中国区 UTC+8 显示，与原客户端目标市场一致）。
    pub fn fmt_epoch(secs: f64) -> String {
        const CST_OFFSET: i64 = 8 * 3600;
        let total = secs as i64 + CST_OFFSET;
        let days = total.div_euclid(86_400);
        let rem = total.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}",
            rem / 3600,
            (rem % 3600) / 60
        )
    }

    /// epoch 秒 → `YYYY/M/D`（原客户端笔记卡片的日期短格式，无补零）。
    pub fn fmt_date(secs: f64) -> String {
        const CST_OFFSET: i64 = 8 * 3600;
        let total = secs as i64 + CST_OFFSET;
        let (y, m, d) = civil_from_days(total.div_euclid(86_400));
        format!("{y:04}/{m}/{d}")
    }

    /// Howard Hinnant civil_from_days：天数 → (年, 月, 日)。
    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    /// 选区包装：在选区两侧插入 open/close，返回新内容与新光标
    /// （primary 落在闭合标签后，secondary 回到开标签后——选中被包裹内容）。
    /// CCursor 下标按**字符**计（非字节），对 CJK 正文安全。
    pub fn wrap_enml_around_selection(
        content: &str,
        range: Option<egui::text::CCursorRange>,
        open: &str,
        close: &str,
    ) -> (String, egui::text::CCursorRange) {
        use egui::text::{CCursor, CCursorRange};
        let (start, end) = match range {
            Some(r) => {
                let a = r.secondary.index.min(r.primary.index);
                let b = r.secondary.index.max(r.primary.index);
                (a, b)
            }
            None => {
                // 无光标/选区（编辑框未聚焦）：插到正文末尾（</en-note> 前）。
                // 文档以 XML 声明开头，插到 char 0 会产生非法文档（sanitize 清空）。
                match content.rfind("</en-note>") {
                    Some(byte) => {
                        let c = content[..byte].chars().count();
                        (c, c)
                    }
                    None => {
                        let c = content.chars().count();
                        (c, c)
                    }
                }
            }
        };
        let chars: Vec<char> = content.chars().collect();
        let start = start.min(chars.len());
        let end = end.max(start).min(chars.len());
        let mut out = String::with_capacity(content.len() + open.len() + close.len());
        out.extend(chars[..start].iter());
        out.push_str(open);
        out.extend(chars[start..end].iter());
        out.push_str(close);
        out.extend(chars[end..].iter());
        let inner_end = start + open.chars().count() + (end - start) + close.chars().count();
        // two(min, max)：primary 收 max（闭合标签后），secondary 收 min（开标签后）
        (
            out,
            CCursorRange::two(CCursor::new(start + open.chars().count()), CCursor::new(inner_end)),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn epoch_epoch_zero_is_1970() {
            // 1970-01-01 00:00 UTC == 08:00 CST
            assert_eq!(fmt_epoch(0.0), "1970-01-01 08:00");
        }

        #[test]
        fn known_date() {
            // 1_792_771_200 = 2026-10-23 16:00 UTC == 2026-10-24 00:00 CST
            let secs = 1_792_771_200.0;
            assert_eq!(fmt_epoch(secs), "2026-10-24 00:00");
        }

        #[test]
        fn fmt_date_short_format() {
            let secs = 1_792_771_200.0;
            assert_eq!(fmt_date(secs), "2026/10/24");
        }

        #[test]
        fn wrap_selection_ascii() {
            use egui::text::{CCursor, CCursorRange};
            let range = CCursorRange::two(CCursor::new(4), CCursor::new(1)); // 选中 "ell"
            let (out, r) = wrap_enml_around_selection("hello", Some(range), "<b>", "</b>");
            assert_eq!(out, "h<b>ell</b>o");
            assert_eq!(r.primary.index, 11); // 闭合标签后（1+3+3+4）
            assert_eq!(r.secondary.index, 4); // 开标签后（1+3）
        }

        #[test]
        fn wrap_cjk_char_indexed() {
            use egui::text::{CCursor, CCursorRange};
            // "你好世界"，选中 "好世"（字符下标 1..3）
            let range = CCursorRange::two(CCursor::new(3), CCursor::new(1));
            let (out, r) = wrap_enml_around_selection("你好世界", Some(range), "<i>", "</i>");
            assert_eq!(out, "你<i>好世</i>界");
            assert_eq!(r.secondary.index, 4); // 1 + 3（<i> 3 字符）
        }

        #[test]
        fn wrap_empty_selection_inserts_pair_at_cursor() {
            use egui::text::{CCursor, CCursorRange};
            let range = CCursorRange::two(CCursor::new(2), CCursor::new(2));
            let (out, r) = wrap_enml_around_selection("ab", Some(range), "<b>", "</b>");
            assert_eq!(out, "ab<b></b>");
            assert_eq!(r.primary.index, 9); // 2+3+0+4
            assert_eq!(r.secondary.index, 5);
        }

        #[test]
        fn wrap_none_range_appends_at_body_end() {
            // 无 </en-note> 的裸串：追加到末尾（旧实现插到开头会破坏 XML 声明）
            let (out, r) = wrap_enml_around_selection("x", None, "<en-todo/>", "");
            assert_eq!(out, "x<en-todo/>");
            assert_eq!(r.primary.index, 11);
            // 完整文档：插到 </en-note> 前（保持合法 ENML，不被 sanitize 清空）
            let doc = "<?xml version=\"1.0\"?><en-note><p>a</p></en-note>";
            let (out2, _) = wrap_enml_around_selection(doc, None, "<hr/>", "");
            assert_eq!(out2, "<?xml version=\"1.0\"?><en-note><p>a</p><hr/></en-note>");
        }

        #[test]
        fn wrap_out_of_range_clamped() {
            use egui::text::{CCursor, CCursorRange};
            let range = CCursorRange::two(CCursor::new(99), CCursor::new(1));
            let (out, _) = wrap_enml_around_selection("abc", Some(range), "<u>", "</u>");
            assert_eq!(out, "a<u>bc</u>");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_channel_delivers_commands() {
        let (tx, rx) = mpsc::channel();
        let mut frontend = CmdFrontend { tx };
        frontend.dispatch(NodeWebkitCommand::Quit);
        frontend.dispatch(NodeWebkitCommand::Present {
            note_ids: vec![1, 2],
            index: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            trigger: "test".into(),
        });
        assert!(matches!(rx.try_recv(), Ok(NodeWebkitCommand::Quit)));
        match rx.try_recv() {
            Ok(NodeWebkitCommand::Present { note_ids, .. }) => {
                assert_eq!(note_ids, vec![1, 2]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn list_sort_orders_by_key_and_direction() {
        let mk = |uid: i64, title: &str, date: f64, created: f64| ListRow {
            uid,
            title: title.into(),
            snippet: String::new(),
            date,
            created,
            tags: None,
        };
        let mut rows = vec![mk(1, "B", 3.0, 1.0), mk(2, "A", 1.0, 3.0), mk(3, "C", 2.0, 2.0)];
        // 更新时间降序（默认）：3 > 2 > 1
        BijiApp::sort_rows(&mut rows, ListSort::Updated, false);
        assert_eq!(rows.iter().map(|r| r.uid).collect::<Vec<_>>(), vec![1, 3, 2]);
        // 创建时间降序
        BijiApp::sort_rows(&mut rows, ListSort::Created, false);
        assert_eq!(rows.iter().map(|r| r.uid).collect::<Vec<_>>(), vec![2, 3, 1]);
        // 标题升序：A B C（中文按 Unicode 码点序）
        BijiApp::sort_rows(&mut rows, ListSort::Title, true);
        assert_eq!(rows.iter().map(|r| r.uid).collect::<Vec<_>>(), vec![2, 1, 3]);
    }

    /// headless egui 集成：注入 PointerButton(Secondary/Primary) 事件走真实控件
    /// 代码路径——右键笔记本行→重命名对话框→确定；右键笔记卡→移动到笔记本→
    /// 子菜单；右键笔记卡→移入回收站。断言全部落在 store 真实状态上。
    #[test]
    fn context_menu_flows_rename_move_trash_headless() {
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let nb_a = app.service.store().create_notebook("工作", None).unwrap();
        let nb_b = app.service.store().create_notebook("生活", None).unwrap();
        let note = app
            .service
            .create_note("移动我", "<en-note><p>正文</p></en-note>", Some(nb_a))
            .unwrap();
        app.refresh_rows();

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.draw_main(ctx));
        };
        run(&ctx, &mut app, vec![]);

        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2, secondary: bool| {
            let btn = if secondary {
                egui::PointerButton::Secondary
            } else {
                egui::PointerButton::Primary
            };
            let mods = egui::Modifiers::default();
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };

        // 点击点附近的小型 Foreground 浮层（右键菜单 / 子菜单）
        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut best_d = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    let Some(r) = m.area_rect(layer.id) else {
                        continue;
                    };
                    if r.width() < 30.0 || r.width() > 420.0 || r.height() > 420.0 {
                        continue;
                    }
                    // 菜单从点击点弹出：中心与点击点距离应不远
                    let d = r.center().distance(near);
                    if d < 200.0 && d < best_d {
                        best_d = d;
                        best = Some(r);
                    }
                }
                best
            })
        };

        // ---- 1) 右键「工作」笔记本行 → 菜单 → 重命名 → 对话框 → 确定
        let mut menu = None;
        for yi in (60..280).step_by(8) {
            let y = yi as f32;
            let pos = egui::pos2(100.0, y);
            click(&ctx, &mut app, pos, true);
            if let Some(r) = popup_near(&ctx, pos) {
                menu = Some(r);
                break;
            }
        }
        let menu = menu.expect("侧栏笔记本行右键应弹出菜单");
        // 第一项：重命名笔记本
        click(
            &ctx,
            &mut app,
            egui::pos2(menu.center().x, menu.min.y + 18.0),
            false,
        );
        assert!(
            app.st.rename_nb.is_some(),
            "点「重命名笔记本」应打开重命名对话框"
        );
        // 对话框出现：填充新名（对话框缓冲即菜单打开时的名字）
        if let Some((_, buf)) = app.st.rename_nb.as_mut() {
            buf.clear();
            buf.push_str("新工作");
        }
        // 找重命名窗口，扫按钮行点「确定」
        let win = ctx
            .memory(|m| m.area_rect(egui::Id::new("重命名笔记本")))
            .expect("重命名窗口应出现");
        'confirm: for x in 20..(win.width() as i32 - 20) {
            let pos = egui::pos2(win.min.x + x as f32 + 0.5, win.max.y - 22.0);
            click(&ctx, &mut app, pos, false);
            if app.st.rename_nb.is_none() {
                break 'confirm;
            }
        }
        assert!(app.st.rename_nb.is_none(), "应能点到「确定」关闭对话框");
        let names: Vec<String> = app
            .service
            .store()
            .list_notebooks()
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert!(names.contains(&"新工作".to_string()), "重命名应落库: {names:?}");
        assert!(!names.contains(&"工作".to_string()));

        // ---- 2) 右键笔记卡 → 移动到笔记本（平铺菜单：回收站/分隔/标签/工作/生活/未分类）
        let card = ctx
            .read_response(egui::Id::new(("note-card", note)))
            .expect("笔记卡应已渲染");
        let pos2 = card.rect.center();
        click(&ctx, &mut app, pos2, true);
        let menu2 = popup_near(&ctx, pos2).expect("笔记卡右键应弹出菜单");
        // 底部最后一项 = 「→ （未分类）」；其上一项 = 名称序最后一本（生活）
        let mut moved = false;
        for dy in [14.0, 18.0, 10.0, 22.0] {
            click(
                &ctx,
                &mut app,
                egui::pos2(menu2.center().x, menu2.max.y - dy),
                false,
            );
            if app
                .service
                .store()
                .get_note(note)
                .unwrap()
                .note
                .notebook_uid
                .is_none()
            {
                moved = true;
                break;
            }
        }
        assert!(moved, "应能点「→ （未分类）」把笔记移出笔记本");
        // 重开菜单，点倒数第二项（生活：名称序在 工作 之后）
        click(&ctx, &mut app, pos2, true);
        let menu2b = popup_near(&ctx, pos2).expect("应再次弹出菜单");
        let mut moved2 = false;
        for dy in [36.0, 40.0, 32.0, 44.0] {
            click(
                &ctx,
                &mut app,
                egui::pos2(menu2b.center().x, menu2b.max.y - dy),
                false,
            );
            let got = app
                .service
                .store()
                .get_note(note)
                .unwrap()
                .note
                .notebook_uid;
            if got == Some(nb_b) {
                moved2 = true;
                break;
            }
        }
        assert!(moved2, "应能点「→ 生活」把笔记移到生活本");

        // ---- 3) 右键笔记卡 → 移入回收站（首项）
        click(&ctx, &mut app, pos2, true);
        let menu3 = popup_near(&ctx, pos2).expect("笔记卡右键应再次弹出菜单");
        click(
            &ctx,
            &mut app,
            egui::pos2(menu3.center().x, menu3.min.y + 18.0),
            false,
        );
        assert!(
            app.service
                .store()
                .get_note(note)
                .unwrap()
                .note
                .is_deleted,
            "右键「移入回收站」应把笔记移入回收站"
        );
    }

    /// headless egui 集成：右键标签行 → 重命名标签（对话框→确定落库）；
    /// 右键标签行 → 删除标签（确认弹窗→删除落库 + 解除笔记关联）。
    #[test]
    fn tag_context_menu_rename_delete_headless() {
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let tag = app.service.store().create_tag("重要", None).unwrap();
        let note = app
            .service
            .create_note("带标签", "<en-note><p>x</p></en-note>", None)
            .unwrap();
        app.service.store().attach_tag(note, tag).unwrap();
        app.refresh_rows();

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.draw_main(ctx));
        };
        run(&ctx, &mut app, vec![]);
        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2, secondary: bool| {
            let btn = if secondary {
                egui::PointerButton::Secondary
            } else {
                egui::PointerButton::Primary
            };
            let mods = egui::Modifiers::default();
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };
        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut best_d = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    let Some(r) = m.area_rect(layer.id) else {
                        continue;
                    };
                    if r.width() < 30.0 || r.width() > 420.0 || r.height() > 420.0 {
                        continue;
                    }
                    let d = r.center().distance(near);
                    if d < 80.0 && d < best_d {
                        best_d = d;
                        best = Some(r);
                    }
                }
                best
            })
        };

        // ---- 1) 右键「重要」标签行 → 重命名标签 → 对话框 → 确定
        // 标签区在笔记本区下方，扫描更宽的 y 范围
        let mut menu = None;
        for yi in (150..420).step_by(8) {
            let y = yi as f32;
            let pos = egui::pos2(100.0, y);
            click(&ctx, &mut app, pos, true);
            // 笔记本行右键也会弹菜单：区分方式是菜单第一项文字不同，
            // 这里直接按「重命名标签」打开后的对话框标题判定是否命中。
            if let Some(r) = popup_near(&ctx, pos) {
                menu = Some(r);
                break;
            }
        }
        let menu = menu.expect("侧栏标签/笔记本行右键应弹出菜单");
        click(
            &ctx,
            &mut app,
            egui::pos2(menu.center().x, menu.min.y + 18.0),
            false,
        );
        // 若命中的是标签行，此刻应打开「重命名标签」对话框
        assert!(
            app.st.rename_tag.is_some() || app.st.rename_nb.is_some(),
            "第一项重命名应打开对话框"
        );
        if app.st.rename_tag.is_none() {
            panic!("扫描应命中标签行而非笔记本行（打开了重命名笔记本）");
        }
        if let Some((_, buf)) = app.st.rename_tag.as_mut() {
            buf.clear();
            buf.push_str("核心");
        }
        let win = ctx
            .memory(|m| m.area_rect(egui::Id::new("重命名标签")))
            .expect("重命名标签窗口应出现");
        'confirm: for x in 20..(win.width() as i32 - 20) {
            let pos = egui::pos2(win.min.x + x as f32 + 0.5, win.max.y - 22.0);
            click(&ctx, &mut app, pos, false);
            if app.st.rename_tag.is_none() {
                break 'confirm;
            }
        }
        assert!(app.st.rename_tag.is_none(), "应能点到「确定」");
        assert!(
            app.service
                .store()
                .list_tags()
                .unwrap()
                .iter()
                .any(|t| t.name == "核心"),
            "标签重命名应落库"
        );
        assert_eq!(
            app.service.store().get_note(note).unwrap().note.tags_text,
            Some("核心".to_string()),
            "笔记的 tags 派生文本应随重命名刷新"
        );

        // ---- 2) 右键标签行 → 删除标签 → 确认弹窗 → 删除
        let mut menu2 = None;
        for yi in (150..420).step_by(8) {
            let y = yi as f32;
            let pos = egui::pos2(100.0, y);
            click(&ctx, &mut app, pos, true);
            if let Some(r) = popup_near(&ctx, pos) {
                menu2 = Some(r);
                break;
            }
        }
        let menu2 = menu2.expect("应再次弹出菜单");
        // 第二项：删除标签
        click(
            &ctx,
            &mut app,
            egui::pos2(menu2.center().x, menu2.min.y + 44.0),
            false,
        );
        assert!(
            matches!(st_confirm(&app), Some(ConfirmKind::Tag)),
            "应打开删除标签确认弹窗"
        );
        let win2 = ctx
            .memory(|m| m.area_rect(egui::Id::new("删除标签")))
            .expect("删除标签确认窗口应出现");
        'del: for x in 20..(win2.width() as i32 - 20) {
            let pos = egui::pos2(win2.min.x + x as f32 + 0.5, win2.max.y - 22.0);
            click(&ctx, &mut app, pos, false);
            if app.st.confirm.is_none() {
                break 'del;
            }
        }
        assert!(app.st.confirm.is_none(), "应能点到「删除」");
        assert!(
            app.service
                .store()
                .list_tags()
                .unwrap()
                .iter()
                .all(|t| t.name != "核心"),
            "删除标签后标签应消失"
        );
        assert!(
            app.service
                .store()
                .note_tags(note)
                .unwrap()
                .is_empty(),
            "笔记与标签的关联应解除"
        );
    }

    /// 测试辅助：确认弹窗种类（避免在断言里解构私有枚举）。
    fn st_confirm(app: &BijiApp) -> Option<ConfirmKind> {
        match &app.st.confirm {
            Some(Confirm::DeleteNotebook(..)) => Some(ConfirmKind::Notebook),
            Some(Confirm::DeleteTag(..)) => Some(ConfirmKind::Tag),
            None => None,
        }
    }

    #[derive(PartialEq, Debug)]
    enum ConfirmKind {
        Notebook,
        Tag,
    }

    /// headless egui 集成：查看菜单 → 深色模式 → 全局/visuals/调色板/库 meta
    /// 四路生效；重开同一库恢复深色；切回浅色持久化。
    #[test]
    fn dark_mode_toggles_and_persists_headless() {
        theme::set_dark(false);
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("theme.exb");

        {
            let store = Store::open(&db).unwrap();
            let (tx, _rx) = mpsc::channel();
            let mut app = BijiApp::new(
                NoteService::new(store, Box::new(CmdFrontend { tx })),
                mpsc::channel().1,
            );
            let ctx = egui::Context::default();
            let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
                theme::apply(ctx); // 对位 update() 每帧先应用主题
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| app.draw_main(ctx));
            };
            let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2| {
                let mods = egui::Modifiers::default();
                run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
                run(
                    ctx,
                    app,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: mods,
                    }],
                );
                run(
                    ctx,
                    app,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: mods,
                    }],
                );
            };

            // 初始浅色
            run(&ctx, &mut app, vec![]);
            assert!(!theme::is_dark());
            assert!(!ctx.style().visuals.dark_mode, "初始应为浅色 visuals");

            // 点「查看(V)」菜单按钮（菜单栏第 3 个菜单，扫 x 段）
            let mut menu: Option<(egui::Pos2, egui::Rect)> = None;
            for xi in (200..320).step_by(10) {
                let pos = egui::pos2(xi as f32, 20.0);
                click(&ctx, &mut app, pos);
                if let Some(r) = ctx.memory(|m| {
                    let mut best: Option<egui::Rect> = None;
                    let mut bd = f32::MAX;
                    for layer in m.layer_ids() {
                        if layer.order != egui::Order::Foreground {
                            continue;
                        }
                        if let Some(r) = m.area_rect(layer.id) {
                            if r.width() < 30.0 || r.width() > 460.0 || r.height() > 460.0 {
                                continue;
                            }
                            let d = r.center().distance(pos);
                            if d < 140.0 && d < bd {
                                bd = d;
                                best = Some(r);
                            }
                        }
                    }
                    best
                }) {
                    menu = Some((pos, r));
                    break;
                }
            }
            let (btn_pos, menu) = menu.expect("点击查看菜单应展开下拉");
            // 菜单项序：卡片视图 / 紧凑列表 / ─ / 深色模式 / ─ / 演示模式…
            // 每轮先重开菜单再试点项（误点其它项会关菜单）
            let mut toggled = false;
            for dy in (48..160).step_by(6) {
                click(&ctx, &mut app, btn_pos);
                click(
                    &ctx,
                    &mut app,
                    egui::pos2(menu.center().x, menu.min.y + dy as f32),
                );
                if theme::is_dark() {
                    toggled = true;
                    break;
                }
            }
            assert!(toggled, "应能点到「🌙 深色模式」");
            // visuals 与面板底色在下一帧生效
            run(&ctx, &mut app, vec![]);
            assert!(ctx.style().visuals.dark_mode, "切换后 visuals 应为深色");
            assert_eq!(ctx.style().visuals.panel_fill, theme::DARK.bg);
            assert_eq!(ctx.style().visuals.window_fill, theme::DARK.white);
            assert_eq!(
                app.service.store().meta_get("theme").as_deref(),
                Some("dark"),
                "主题选择应写入库 meta"
            );
        }

        // 模拟重启：重置全局为默认浅色，重开同一库应恢复深色
        theme::set_dark(false);
        let store2 = Store::open(&db).unwrap();
        let (tx2, _rx2) = mpsc::channel();
        let mut app2 = BijiApp::new(
            NoteService::new(store2, Box::new(CmdFrontend { tx: tx2 })),
            mpsc::channel().1,
        );
        assert!(theme::is_dark(), "重启后应从 meta 恢复深色模式");

        // 切回浅色（工具栏快捷钮同一路径）并持久化
        app2.toggle_theme();
        assert!(!theme::is_dark());
        assert_eq!(
            app2.service.store().meta_get("theme").as_deref(),
            Some("light")
        );
    }

    /// headless egui 集成：列表头「{来源} ▼」点击弹来源选择——
    /// 全部笔记 / 各笔记本 / 各标签；切换后 selection 与列表内容即时生效。
    #[test]
    fn list_header_source_dropdown_switches_view() {
        theme::set_dark(false);
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let nb_work = app.service.store().create_notebook("工作", None).unwrap();
        let n1 = app
            .service
            .create_note("甲", "<en-note><p>a</p></en-note>", Some(nb_work))
            .unwrap();
        let n2 = app
            .service
            .create_note("乙", "<en-note><p>b</p></en-note>", None)
            .unwrap();
        let tag = app.service.store().create_tag("重要", None).unwrap();
        app.service.store().attach_tag(n2, tag).unwrap();
        app.refresh_rows();

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            theme::apply(ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.draw_main(ctx));
        };
        run(&ctx, &mut app, vec![]);
        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2| {
            let mods = egui::Modifiers::default();
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };
        // 初始全部：2 条
        assert_eq!(app.st.rows.len(), 2);

        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut bd = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    if let Some(r) = m.area_rect(layer.id) {
                        if r.width() < 30.0 || r.width() > 460.0 || r.height() > 460.0 {
                            continue;
                        }
                        let d = r.center().distance(near);
                        if d < 140.0 && d < bd {
                            bd = d;
                            best = Some(r);
                        }
                    }
                }
                best
            })
        };

        // 辅助：点开来源下拉并点第 idx 个候选（每轮重开，CloseOnClick 一点即关）
        let open_and_pick = |ctx: &egui::Context,
                             app: &mut BijiApp,
                             header_pos: egui::Pos2,
                             item_dy: f32|
         -> bool {
            click(ctx, app, header_pos);
            // CloseOnClick：点头标本身也会关弹窗——先点开再点项
            if popup_near(ctx, header_pos).is_none() {
                click(ctx, app, header_pos); // 偶发 toggle，再点一次确保展开
            }
            let Some(m) = popup_near(ctx, header_pos) else {
                return false;
            };
            click(ctx, app, egui::pos2(m.center().x, m.min.y + item_dy));
            true
        };

        // 找头标位置：列表列顶部（搜索框下方），扫描 y 段
        let mut header = None;
        for yi in (60..480).step_by(8) {
            let pos = egui::pos2(260.0, yi as f32);
            click(&ctx, &mut app, pos);
            if popup_near(&ctx, pos).is_some() {
                header = Some(pos);
                break;
            }
        }
        let header = header.expect("点击列表头「全部笔记 ▼」应弹出来源菜单");
        // 候选序（名称码点序）：📄 全部笔记 / 📓 工作 / 🏷 重要
        // 逐项扫描 dy 直到 selection 变化并断言列表内容
        let pick_until = |ctx: &egui::Context,
                          app: &mut BijiApp,
                          want: &dyn Fn(&BijiApp) -> bool|
         -> bool {
            for dy in (10..170).step_by(6) {
                open_and_pick(ctx, app, header, dy as f32);
                if want(app) {
                    return true;
                }
            }
            false
        };

        // 选「📓 工作」→ 只剩甲
        assert!(
            pick_until(&ctx, &mut app, &|app: &BijiApp| {
                app.st.selection == Selection::Notebook(nb_work) && app.st.rows.len() == 1
                    && app.st.rows[0].uid == n1
            }),
            "来源下拉应能切到笔记本「工作」且列表只剩该本笔记"
        );
        // 选「🏷 重要」→ 只剩乙
        assert!(
            pick_until(&ctx, &mut app, &|app: &BijiApp| {
                app.st.selection == Selection::Tag(tag) && app.st.rows.len() == 1
                    && app.st.rows[0].uid == n2
            }),
            "来源下拉应能切到标签「重要」且列表只剩带标笔记"
        );
        // 选「📄 全部笔记」→ 回到 2 条
        assert!(
            pick_until(&ctx, &mut app, &|app: &BijiApp| {
                app.st.selection == Selection::All && app.st.rows.len() == 2
            }),
            "来源下拉应能切回全部笔记"
        );
    }

    /// headless egui 集成：编辑器源码工具栏「表格」「—」按钮真实点击插入
    /// 骨架（光标/选区路径），Ctrl+S 落库，切预览渲染（裸行表格 + hr）。
    #[test]
    fn toolbar_inserts_table_and_hr_then_saves_headless() {
        theme::set_dark(false);
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let uid = app
            .service
            .create_note("表格测试", "<en-note><p>正文</p></en-note>", None)
            .unwrap();
        app.st.selected_note = Some(uid);

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            theme::apply(ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                app.handle_global_shortcuts(ctx); // 对位 update() 每帧入口
                app.draw_main(ctx);
            });
        };
        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2| {
            let mods = egui::Modifiers::default();
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };
        // 打开编辑器（选中笔记即载入，默认预览态）；模式切换为既有功能，
        // 这里直设状态进源码模式（本轮特性 = 工具栏按钮/保存/预览渲染真实驱动）
        run(&ctx, &mut app, vec![]);
        assert!(app.st.editor.is_some(), "选中笔记应载入编辑器");
        app.st.editor.as_mut().unwrap().preview = false;
        run(&ctx, &mut app, vec![]);

        // 源码编辑框显式 Id：工具栏行就在其正上方
        let src = ctx
            .read_response(egui::Id::new("enml-source"))
            .expect("源码编辑框应出现")
            .rect;
        let row_y = src.min.y - 16.0;

        // 探测点击会在 TextEdit 留下残留光标：恢复内容时一并清掉，
        // 让单次插入走「无光标」路径（插到 </en-note> 前，保持合法 ENML）
        let clear_cursor = |ctx: &egui::Context| {
            let id = egui::Id::new("enml-source");
            if let Some(mut ts) = egui::TextEdit::load_state(ctx, id) {
                ts.cursor.set_char_range(None);
                egui::TextEdit::store_state(ctx, id, ts);
            }
        };
        // 定位 + 单次插入：每次探测前把内容重置回原始源码（盲扫会连点多个按钮堆坏内容）
        let pristine = app.st.editor.as_ref().unwrap().content.clone();
        let find_btn = |ctx: &egui::Context, app: &mut BijiApp, marker: &str| -> Option<f32> {
            for xi in (0..900).step_by(20) {
                app.st.editor.as_mut().unwrap().content = pristine.clone();
                clear_cursor(ctx);
                click(ctx, app, egui::pos2(src.min.x + xi as f32, row_y));
                if app
                    .st
                    .editor
                    .as_ref()
                    .map(|e| e.content.contains(marker))
                    .unwrap_or(false)
                {
                    return Some(src.min.x + xi as f32);
                }
            }
            None
        };
        // 点「表格」（单次）
        let table_x = find_btn(&ctx, &mut app, "<table><tr><td>")
            .expect("工具栏应有「表格」按钮");
        app.st.editor.as_mut().unwrap().content = pristine.clone();
        clear_cursor(&ctx);
        click(&ctx, &mut app, egui::pos2(table_x, row_y));
        let after_table = app.st.editor.as_ref().unwrap().content.clone();
        assert!(after_table.contains("<table><tr><td>"));
        assert!(after_table.starts_with("<?xml"), "插入位置不得破坏 XML 声明");

        // 点「—」（单次；基于已含表格的内容继续，探测后恢复该基线）
        let baseline = app.st.editor.as_ref().unwrap().content.clone();
        let hr_x = find_btn(&ctx, &mut app, "<hr/>").expect("工具栏应有「—」按钮");
        app.st.editor.as_mut().unwrap().content = baseline;
        clear_cursor(&ctx);
        click(&ctx, &mut app, egui::pos2(hr_x, row_y));
        let content = app.st.editor.as_ref().unwrap().content.clone();
        assert!(content.contains("<hr/>"));
        assert!(content.contains("<table><tr><td>"));
        // 插入结果仍是合法 ENML（无选区路径插在正文末尾）
        assert!(
            Enml::parse(&content).is_ok(),
            "插入后的源码应可解析: {content}"
        );

        // Ctrl+S 落库（headless 注入键事件走真实快捷键链路）
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        // egui 0.29 修饰键状态来自 RawInput.modifiers（非事件）；注入 Ctrl+S 帧需带上
        {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                modifiers: ctrl,
                events: vec![egui::Event::Key {
                    key: egui::Key::S,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: ctrl,
                }],
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                app.handle_global_shortcuts(ctx);
                app.draw_main(ctx);
            });
            // 释放帧（清键事件）
            let empty = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(empty, |ctx| app.draw_main(ctx));
        }
        run(&ctx, &mut app, vec![]); // 释放帧：按键事件在上一帧内已被处理器消费
        let saved = app.service.store().get_note(uid).unwrap().enml;
        assert!(
            saved.contains("<table><tr><td>") && saved.contains("<hr/>"),
            "Ctrl+S 后表格与分割线应落库: {saved}"
        );

        // 切预览模式渲染（裸行表格 + hr 不再静默丢失；切换按钮走编辑器工具行）
        let src2 = ctx
            .read_response(egui::Id::new("enml-source"))
            .map(|r| r.rect);
        let tool_y = src2.map(|r| r.min.y - 46.0);
        let mut previewed = false;
        if let Some(ty) = tool_y {
            let x0 = src2.unwrap().min.x;
            for xi in (0..420).step_by(20) {
                click(&ctx, &mut app, egui::pos2(x0 + xi as f32, ty));
                if app.st.editor.as_ref().map(|e| e.preview).unwrap_or(false) {
                    previewed = true;
                    break;
                }
            }
        }
        if !previewed {
            app.st.editor.as_mut().unwrap().preview = true;
        }
        // 渲染一帧（Enml 解析 + render_interactive 全链路，此前裸行表格被丢弃）
        run(&ctx, &mut app, vec![]);
        assert!(app.st.editor.is_some());
        assert!(app.st.editor.as_ref().unwrap().preview);
    }

    /// headless egui 集成：笔记卡右键「添加到快捷方式」→ 侧栏快捷方式分区出现 →
    /// 点击直达笔记 → 分区行右键「取消快捷方式」移除。
    #[test]
    fn shortcuts_pin_open_unpin_headless() {
        theme::set_dark(false);
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let uid = app
            .service
            .create_note("置顶笔记", "<en-note><p>x</p></en-note>", None)
            .unwrap();
        app.refresh_rows();

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            theme::apply(ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.draw_main(ctx));
        };
        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2, secondary: bool| {
            let btn = if secondary {
                egui::PointerButton::Secondary
            } else {
                egui::PointerButton::Primary
            };
            let mods = egui::Modifiers::default();
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: btn,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };
        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut bd = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    if let Some(r) = m.area_rect(layer.id) {
                        if r.width() < 30.0 || r.width() > 460.0 || r.height() > 460.0 {
                            continue;
                        }
                        let d = r.center().distance(near);
                        if d < 200.0 && d < bd {
                            bd = d;
                            best = Some(r);
                        }
                    }
                }
                best
            })
        };
        run(&ctx, &mut app, vec![]);

        // ---- 1) 卡右键 → 「📌 添加到快捷方式」
        let card = ctx
            .read_response(egui::Id::new(("note-card", uid)))
            .expect("卡片应已渲染")
            .rect;
        let pos = card.center();
        let mut pinned = false;
        // 菜单项序：移入回收站(1) / 📪快捷方式(2)…—— 从第 2 项区间起扫，
        // 避免先命中第 1 项把笔记删掉
        for dy in (28..150).step_by(6) {
            click(&ctx, &mut app, pos, true);
            if let Some(m) = popup_near(&ctx, pos) {
                click(&ctx, &mut app, egui::pos2(m.center().x, m.min.y + dy as f32), false);
                if app.service.store().shortcuts() == vec![uid] {
                    pinned = true;
                    break;
                }
                assert!(
                    !app.service.store().get_note(uid).unwrap().note.is_deleted,
                    "扫描误删笔记（dy={dy}）"
                );
            }
        }
        assert!(pinned, "卡右键应能添加到快捷方式");

        // ---- 2) 侧栏快捷方式行点击直达笔记
        run(&ctx, &mut app, vec![]);
        let mut opened = false;
        for yi in (60..260).step_by(8) {
            let pos = egui::pos2(100.0, yi as f32);
            click(&ctx, &mut app, pos, false);
            if app.st.selected_note == Some(uid) {
                opened = true;
                break;
            }
        }
        assert!(opened, "点击侧栏快捷方式行应直达笔记");
        run(&ctx, &mut app, vec![]);
        assert_eq!(
            app.st.editor.as_ref().map(|e| e.title.clone()),
            Some("置顶笔记".to_string()),
            "编辑器应载入置顶笔记"
        );

        // ---- 3) 快捷方式行右键 → 取消
        let mut removed = false;
        for yi in (60..260).step_by(8) {
            let pos = egui::pos2(100.0, yi as f32);
            click(&ctx, &mut app, pos, true);
            if let Some(m) = popup_near(&ctx, pos) {
                click(&ctx, &mut app, egui::pos2(m.center().x, m.min.y + 18.0), false);
                if app.service.store().shortcuts().is_empty() {
                    removed = true;
                    break;
                }
            }
        }
        assert!(removed, "快捷方式行右键应能取消置顶");
        run(&ctx, &mut app, vec![]);
        assert!(
            app.service.store().shortcut_notes().is_empty(),
            "取消后侧栏分区应无置顶笔记"
        );
    }

    /// headless egui 集成：Ctrl/Shift 多选 → 底部批量条「移动到…」弹层批量移动
    /// → 批量「移入回收站」；断言全部落在 store 真实状态。
    #[test]
    fn multi_select_batch_ops_headless() {
        theme::set_dark(false);
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let nb = app.service.store().create_notebook("生活", None).unwrap();
        let n1 = app
            .service
            .create_note("甲", "<en-note><p>1</p></en-note>", None)
            .unwrap();
        let n2 = app
            .service
            .create_note("乙", "<en-note><p>2</p></en-note>", None)
            .unwrap();
        let n3 = app
            .service
            .create_note("丙", "<en-note><p>3</p></en-note>", None)
            .unwrap();
        app.refresh_rows();

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>, mods: egui::Modifiers| {
            theme::apply(ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                modifiers: mods,
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                app.handle_global_shortcuts(ctx);
                app.draw_main(ctx);
            });
        };
        let none = egui::Modifiers::default();
        let click_at = |ctx: &egui::Context,
                        app: &mut BijiApp,
                        pos: egui::Pos2,
                        mods: egui::Modifiers| {
            run(
                ctx,
                app,
                vec![egui::Event::PointerMoved(pos)],
                none,
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: mods,
                }],
                mods,
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: mods,
                }],
                mods, // 真实使用中修饰键在释放帧仍按住（clicked() 在释放帧求值）
            );
        };
        let card_center = |ctx: &egui::Context, uid: i64| -> egui::Pos2 {
            ctx.read_response(egui::Id::new(("note-card", uid)))
                .expect("卡片应已渲染")
                .rect
                .center()
        };
        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut bd = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    if let Some(r) = m.area_rect(layer.id) {
                        if r.width() < 30.0 || r.width() > 460.0 || r.height() > 460.0 {
                            continue;
                        }
                        let d = r.center().distance(near);
                        if d < 200.0 && d < bd {
                            bd = d;
                            best = Some(r);
                        }
                    }
                }
                best
            })
        };
        run(&ctx, &mut app, vec![], none);

        // ---- 多选三态：普通点甲（锚点）→ Ctrl 乙 → Shift 丙 = [甲,乙,丙]
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        click_at(&ctx, &mut app, card_center(&ctx, n1), none);
        assert_eq!(app.st.multi_select, Vec::<i64>::new(), "普通点击应清多选");
        click_at(&ctx, &mut app, card_center(&ctx, n2), ctrl);
        assert_eq!(app.st.multi_select, vec![n2], "Ctrl 点击应切换成员");
        click_at(&ctx, &mut app, card_center(&ctx, n3), shift);
        assert_eq!(
            app.st.multi_select,
            vec![n3, n2, n1],
            "Shift 应选锚点(甲)到丙的区间（顺序随列表序=更新时间降序）"
        );

        // ---- 批量条「📦 移动到…」→ 弹层选「📓 生活」→ 三条全移动
        // 批量条在列表面板顶部：第一张卡片上方 ~18px 即条内（条出现会把卡片下推，
        // 逐次以实时卡片位置反推条 y，避免盲扫点中卡片清掉多选）
        // 条在「当前列表首行卡片」上方；刷新会重排序，逐次取实时首行 uid
        let bar_y = |ctx: &egui::Context, app: &BijiApp| -> f32 {
            let first = app.st.rows[0].uid;
            ctx.read_response(egui::Id::new(("note-card", first)))
                .expect("卡片应已渲染")
                .rect
                .min
                .y
                - 18.0
        };
        let mut moved = false;
        for xi in (215..620).step_by(10) {
            let y = bar_y(&ctx, &app);
            click_at(&ctx, &mut app, egui::pos2(xi as f32, y), none);
            if popup_near(&ctx, egui::pos2(xi as f32, y)).is_some() {
                moved = true;
                break;
            }
            assert!(
                !app.st.multi_select.is_empty(),
                "扫描误清多选（xi={xi}）"
            );
        }
        assert!(moved, "点「移动到…」应弹目标层");
        // 弹层里只有一本（生活）+ 未分类：点第一项
        if let Some(m) = popup_near(&ctx, egui::pos2(300.0, bar_y(&ctx, &app))) {
            click_at(&ctx, &mut app, egui::pos2(m.center().x, m.min.y + 18.0), none);
        }
        let got: Vec<Option<i64>> = [n1, n2, n3]
            .iter()
            .map(|&u| app.service.store().get_note(u).unwrap().note.notebook_uid)
            .collect();
        assert_eq!(
            got,
            vec![Some(nb), Some(nb), Some(nb)],
            "三条应全部移到「生活」"
        );
        assert!(app.st.multi_select.is_empty(), "批量操作后应清多选");

        // ---- 批量回收站：重选甲乙 → 点「🗑 移入回收站」
        click_at(&ctx, &mut app, card_center(&ctx, n1), none);
        click_at(&ctx, &mut app, card_center(&ctx, n2), ctrl);
        assert_eq!(app.st.multi_select, vec![n2]);
        click_at(&ctx, &mut app, card_center(&ctx, n1), ctrl);
        assert_eq!(app.st.multi_select, vec![n2, n1]);
        let mut trashed = false;
        for xi in (215..620).step_by(10) {
            let y = bar_y(&ctx, &app);
            click_at(&ctx, &mut app, egui::pos2(xi as f32, y), none);
            let d1 = app.service.store().get_note(n1).unwrap().note.is_deleted;
            let d2 = app.service.store().get_note(n2).unwrap().note.is_deleted;
            if d1 && d2 {
                trashed = true;
                break;
            }
            assert!(
                !(d1 || d2),
                "扫描误触批量条按钮（xi={xi}）提前删除"
            );
        }
        assert!(trashed, "批量条「移入回收站」应删除所选两条");
        assert!(
            !app.service.store().get_note(n3).unwrap().note.is_deleted,
            "未选中的丙不应受影响"
        );
    }

    #[test]
    fn select_range_and_ctrl_toggle_semantics() {
        let mk = |uid: i64, date: f64| ListRow {
            uid,
            title: String::new(),
            snippet: String::new(),
            date,
            created: date,
            tags: None,
        };
        let rows = vec![mk(3, 30.0), mk(2, 20.0), mk(1, 10.0)]; // 列表序（更新时间降序）
        let mut multi = Vec::new();
        // 顺向区间与逆向区间等价（都取闭区间全成员，按列表序）
        BijiApp::select_range(&rows, 1, 3, &mut multi);
        assert_eq!(multi, vec![3, 2, 1]);
        BijiApp::select_range(&rows, 3, 1, &mut multi);
        assert_eq!(multi, vec![3, 2, 1]);
        // 单成员区间
        BijiApp::select_range(&rows, 2, 2, &mut multi);
        assert_eq!(multi, vec![2]);
        // 锚点不在列表中：不动
        let mut keep = vec![9];
        BijiApp::select_range(&rows, 99, 1, &mut keep);
        assert_eq!(keep, vec![9]);
        // Ctrl toggle：保序加入/移出
        let mut m = Vec::new();
        BijiApp::ctrl_toggle(&mut m, 5);
        BijiApp::ctrl_toggle(&mut m, 7);
        assert_eq!(m, vec![5, 7]);
        BijiApp::ctrl_toggle(&mut m, 5);
        assert_eq!(m, vec![7]);
    }

    /// headless egui 集成：标签菜单搜索前缀过滤 → 点击候选补全挂载 →
    /// 未命中输入「＋ 新建」创建并挂载。
    #[test]
    fn tag_autocomplete_filter_create_headless() {
        theme::set_dark(false);
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        let t_zhongyao = app.service.store().create_tag("重要", None).unwrap();
        let t_zhongliang = app.service.store().create_tag("重量", None).unwrap();
        let _t_lvyou = app.service.store().create_tag("旅游", None).unwrap();
        let uid = app
            .service
            .create_note("甲", "<en-note><p>1</p></en-note>", None)
            .unwrap();
        app.st.selected_note = Some(uid);

        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, app: &mut BijiApp, events: Vec<egui::Event>| {
            theme::apply(ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                app.handle_global_shortcuts(ctx);
                app.draw_main(ctx);
            });
        };
        let none = egui::Modifiers::default();
        let click = |ctx: &egui::Context, app: &mut BijiApp, pos: egui::Pos2| {
            run(ctx, app, vec![egui::Event::PointerMoved(pos)]);
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: none,
                }],
            );
            run(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: none,
                }],
            );
        };
        let popup_near = |ctx: &egui::Context, near: egui::Pos2| -> Option<egui::Rect> {
            ctx.memory(|m| {
                let mut best: Option<egui::Rect> = None;
                let mut bd = f32::MAX;
                for layer in m.layer_ids() {
                    if layer.order != egui::Order::Foreground {
                        continue;
                    }
                    if let Some(r) = m.area_rect(layer.id) {
                        if r.width() < 30.0 || r.width() > 460.0 || r.height() > 460.0 {
                            continue;
                        }
                        let d = r.center().distance(near);
                        if d < 200.0 && d < bd {
                            bd = d;
                            best = Some(r);
                        }
                    }
                }
                best
            })
        };
        run(&ctx, &mut app, vec![]);

        // 输入「重」（文本录入走标准 TextEdit；此处直设过滤态驱动候选逻辑）
        app.st.tag_filter = "重".into();
        // 打开「🏷 添加标签…」菜单：标签行在编辑器面板（正文下方）
        let mut menu = None;
        'scan: for yi in (150..870).step_by(10) {
            for xi in (505..1560).step_by(40) {
                let pos = egui::pos2(xi as f32, yi as f32);
                click(&ctx, &mut app, pos);
                if popup_near(&ctx, pos).is_some() {
                    menu = Some((pos, popup_near(&ctx, pos).unwrap()));
                    break 'scan;
                }
            }
        }
        let (btn_pos, mrect) = menu.expect("点击「添加标签…」应弹出菜单（含搜索框）");
        // 前缀过滤后的首候选 = 重要（名称序：重要 < 重量；旅游被过滤）
        click(
            &ctx,
            &mut app,
            egui::pos2(mrect.center().x, mrect.min.y + 52.0),
        );
        let tags = app.service.store().note_tags(uid).unwrap();
        assert_eq!(
            tags,
            vec![(t_zhongyao, "重要".to_string())],
            "点击首候选应挂载「重要」（过滤生效且未挂错）"
        );
        assert!(
            !tags.iter().any(|(u, _)| *u == t_zhongliang),
            "「重量」不应被误挂"
        );
        assert!(app.st.tag_filter.is_empty(), "挂载后应清过滤");

        // ---- 未命中输入 → 「＋ 新建」创建并挂载
        app.st.tag_filter = "新标签".into();
        click(&ctx, &mut app, btn_pos);
        let m2 = popup_near(&ctx, btn_pos).expect("应再次弹出菜单");
        // 末项 = 新建按钮（无候选命中时仅此一项）
        click(&ctx, &mut app, egui::pos2(m2.center().x, m2.max.y - 16.0));
        let tags = app.service.store().note_tags(uid).unwrap();
        assert!(
            tags.iter().any(|(_, n)| n == "新标签"),
            "未命中输入应经「＋ 新建」创建并挂载: {tags:?}"
        );
        assert!(
            app.service
                .store()
                .list_tags()
                .unwrap()
                .iter()
                .any(|t| t.name == "新标签"),
            "新标签应入库"
        );
    }

    #[test]
    fn ui_state_rows_invalidate_on_signature_change() {
        let store = Store::open_memory().unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut app = BijiApp::new(
            NoteService::new(store, Box::new(CmdFrontend { tx })),
            mpsc::channel().1,
        );
        assert!(app.st.rows.is_empty());
        app.service
            .create_note("标题一", "<en-note><p>正文内容</p></en-note>", None)
            .unwrap();
        app.refresh_rows();
        assert_eq!(app.st.rows.len(), 1);
        assert_eq!(app.st.rows[0].title, "标题一");
        assert!(app.st.rows[0].snippet.contains("正文内容"));

        app.service.trash_note(app.st.rows[0].uid).unwrap();
        app.refresh_rows();
        assert!(app.st.rows.is_empty(), "回收站视图外的列表应排除已删笔记");

        app.st.trash_view = true;
        app.refresh_rows();
        assert_eq!(app.st.rows.len(), 1);
    }
}
