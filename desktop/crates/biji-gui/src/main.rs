//! biji-gui——印象笔记复刻的桌面 GUI（egui/eframe，纯 Rust 单壳）。
//!
//! 对应分析文档 §9.1 选型映射："WPF 外壳 + CEF + NW.js 三引擎"收敛为
//! 单一现代壳；GUI 同时扮演原架构中的两个角色：
//! - 原生外壳：调用 `NoteService`（原 DesktopService 通路）；
//! - NW.js 编辑器：实现 `MarkupFrontend` 消费 `NodeWebkitCommand`
//!   （原 `\\.\pipe\yinxiang.nwserver` 命令通路，本实现为进程内通道），
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
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use annotate::AnnotateState;
use biji_enml::Enml;
use biji_service::{
    parse_resource_url, resource_url, DisplayInfo, MarkupFrontend, NodeWebkitCommand, NoteService,
    NoteView, ResourceData,
};
use biji_store::{ListNotes, Store};
use biji_sync::{MockEdamServer, SyncEngine};

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
            .with_title("biji — 印象笔记复刻"),
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
    tags: Option<String>,
}

#[derive(Debug, Clone)]
enum Confirm {
    DeleteNotebook(i64, String),
}

struct UiState {
    selection: Selection,
    trash_view: bool,
    selected_note: Option<i64>,
    editor: Option<EditorState>,
    search: String,
    rows: Vec<ListRow>,
    rows_sig: String,
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
    fn new() -> UiState {
        UiState {
            selection: Selection::All,
            trash_view: false,
            selected_note: None,
            editor: None,
            search: String::new(),
            rows: Vec::new(),
            rows_sig: String::new(),
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
        let mut app = BijiApp {
            service,
            cmd_rx,
            st: UiState::new(),
        };
        app.refresh_rows();
        app
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
            "{}|{:?}|{}|{}",
            self.current_usn(),
            self.st.selection,
            self.st.trash_view,
            self.st.search
        )
    }

    fn refresh_rows(&mut self) {
        let sig = self.rows_signature();
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
                    tags: meta.tags_text,
                });
            }
        }
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
        let menu_text = |s: &str| egui::RichText::new(s).size(13.0).color(theme::TEXT);
        egui::TopBottomPanel::top("menubar")
            .frame(
                egui::Frame::default()
                    .fill(theme::WHITE)
                    .inner_margin(egui::Margin::symmetric(8.0, 2.0)),
            )
            .show(ctx, |ui| {
                // 菜单按钮扁平化：静态纯文字，hover 才显灰底（对位原生菜单栏）
                {
                    let w = &mut ui.style_mut().visuals.widgets;
                    w.inactive.bg_fill = theme::WHITE;
                    w.inactive.weak_bg_fill = theme::WHITE;
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
                                    .color(theme::SUBTLE),
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
                                "biji — 印象笔记（v7.4.30.9439）复刻 · Rust/egui · ENML 存储 · USN 同步 · 双服务语义"
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
                                .color(theme::SUBTLE),
                        );
                        let pending = self.pending_sync_count();
                        if ui
                            .add(theme::green_button(&format!("同步({pending})")))
                            .clicked()
                        {
                            self.do_sync();
                        }
                    });
                });
            });
    }

    fn do_sync(&mut self) {
        let store = self.service.store();
        let engine = SyncEngine::new(store);
        // 凭证环境变量齐备 → 真实 EDAM 远端（自研 Go 服务端/印象笔记）；
        // 否则内存 mock 演示闭环。
        let mut remote = biji_sync::remote_from_env(Rc::new(std::cell::RefCell::new(
            MockEdamServer::new(),
        )));
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
        let mut action: Option<Selection> = None;
        let mut to_confirm_delete: Option<(i64, String)> = None;
        let mut open_new_nb = false;

        egui::SidePanel::left("sidebar")
            .resizable(false)
            .default_width(190.0)
            .frame(
                egui::Frame::default()
                    .fill(theme::SIDEBAR_BG)
                    .inner_margin(egui::Margin::same(8.0)),
            )
            // 原客户端侧栏与内容区无可见分隔线，靠底色区分
            .show(ctx, |ui| {
                // 新建笔记：白药丸 + 绿圆＋（对位原客户端侧栏顶部组合控件；
                // 内边距 10px 与导航行图标左缘对齐）
                let pill = egui::Frame::default()
                    .fill(theme::WHITE)
                    .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
                    .rounding(15.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 4.0));
                let pill_inner = pill.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let plus = egui::Button::new(
                            egui::RichText::new("＋").small().color(theme::WHITE),
                        )
                        .fill(theme::GREEN)
                        .stroke(egui::Stroke::NONE)
                        .rounding(11.0);
                        ui.add_sized([22.0, 22.0], plus);
                        ui.label(egui::RichText::new("新建笔记").color(theme::TEXT));
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
                            if theme::nav_row_sized(ui, w, "📓", &nb.name, selected).clicked() {
                                action = Some(Selection::Notebook(nb.uid));
                                self.st.trash_view = false;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("✕")
                                                    .small()
                                                    .color(theme::SUBTLE),
                                            )
                                            .fill(theme::SIDEBAR_BG)
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
                                            .color(theme::SUBTLE),
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
                        if theme::nav_row(
                            ui,
                            "🏷",
                            &format!("{} ({})", t.name, t.note_count),
                            selected,
                        )
                        .clicked()
                        {
                            action = Some(Selection::Tag(t.uid));
                            self.st.trash_view = false;
                        }
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
    }

    fn draw_note_list(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("note-list")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
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
                // 搜索框（对位原客户端列表列顶部；F6 聚焦）
                let search_id = egui::Id::new("note-search");
                if ui.input(|i| i.key_pressed(egui::Key::F6)) {
                    ui.ctx().memory_mut(|m| m.request_focus(search_id));
                }
                let mut search = self.st.search.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut search)
                        .hint_text("搜索笔记 (F6)")
                        .id(search_id)
                        .desired_width(ui.available_width()),
                );
                if resp.changed() {
                    self.st.search = search;
                    self.refresh_rows();
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("{header} ▼"))
                        .size(17.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("{}条笔记", self.st.rows.len()))
                        .small()
                        .color(theme::SUBTLE),
                );
                ui.add_space(2.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let rows = self.st.rows.clone();
                    if rows.is_empty() {
                        ui.add_space(20.0);
                        ui.label(egui::RichText::new("（空）").color(theme::SUBTLE));
                    }
                    for row in rows {
                        let selected = self.st.selected_note == Some(row.uid);
                        // 笔记卡：白底圆角发丝边；选中 = 亮绿描边 + 浅绿底
                        // （对位原客户端笔记列表的卡片选中态）
                        let frame = egui::Frame::default()
                            .fill(if selected {
                                theme::GREEN_TINT
                            } else {
                                theme::WHITE
                            })
                            .rounding(8.0)
                            .stroke(egui::Stroke::new(
                                if selected { 2.0 } else { 1.0 },
                                if selected {
                                    theme::GREEN_EDGE
                                } else {
                                    theme::HAIRLINE
                                },
                            ))
                            .inner_margin(egui::Margin::same(10.0));
                        let inner = frame.show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(&row.title)
                                    .size(15.0)
                                    .strong()
                                    .color(theme::TEXT),
                            );
                            if !row.snippet.is_empty() {
                                ui.label(
                                    egui::RichText::new(&row.snippet)
                                        .small()
                                        .color(theme::SUBTLE),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(util::fmt_date(row.date))
                                        .small()
                                        .color(theme::GREEN),
                                );
                                if let Some(tags) = &row.tags {
                                    ui.label(
                                        egui::RichText::new(format!("🏷 {tags}"))
                                            .small()
                                            .color(theme::SUBTLE),
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
                            self.st.selected_note = Some(row.uid);
                        }
                        ui.add_space(6.0);
                    }
                });
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
        let (action, uid) = {
            let Some(ed) = self.st.editor.as_mut() else {
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.add_space(80.0);
                    ui.label(egui::RichText::new("选择或新建一条笔记").size(20.0).weak());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Evernote 复刻 · ENML 存储 · USN 同步").weak());
                });
                return;
            };

            let mut action = Action::None;

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
                                                .color(theme::TEXT),
                                        )
                                        .fill(theme::BG)
                                        .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
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
                                .color(theme::SUBTLE),
                        );
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
                                egui::RichText::new("● 保存").color(theme::GREEN_DARK),
                            )
                            .fill(theme::BG)
                            .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        action = Action::Save;
                    }
                } else {
                    ui.label(egui::RichText::new("已保存").small().color(theme::SUBTLE));
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
                        egui::Button::new(egui::RichText::new("🗑 删除").color(theme::SUBTLE))
                            .fill(theme::BG)
                            .stroke(egui::Stroke::NONE),
                    )
                    .clicked()
                {
                    action = Action::Trash;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("▶ 放映本篇").color(theme::SUBTLE))
                            .fill(theme::BG)
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
                            .color(theme::GREEN_DARK),
                        |ui| {
                            let tags = self.service.store().list_tags().unwrap_or_default();
                            let mut any = false;
                            for t in &tags {
                                if attached.iter().any(|(u, _)| *u == t.uid) {
                                    continue;
                                }
                                any = true;
                                if ui.button(&t.name).clicked() {
                                    action = Action::AttachTag(t.uid);
                                    ui.close_menu();
                                }
                            }
                            if !any {
                                ui.label(
                                    egui::RichText::new("（没有可添加的标签）")
                                        .small()
                                        .color(theme::SUBTLE),
                                );
                            }
                        },
                    );
                    for (t_uid, t_name) in &attached {
                        let chip = egui::Frame::default()
                            .fill(theme::WHITE)
                            .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
                            .rounding(10.0)
                            .inner_margin(egui::Margin::symmetric(8.0, 2.0));
                        chip.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("🏷 {t_name}"))
                                        .small()
                                        .color(theme::TEXT),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("✕").small().color(theme::SUBTLE),
                                        )
                                        .fill(theme::WHITE)
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
                    ("H1", "<h1>", "</h1>"),
                    ("H2", "<h2>", "</h2>"),
                    ("代码", "<code>", "</code>"),
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

            (action, uid)
        };
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
    }
}

impl eframe::App for BijiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 每帧先恢复浅色（标注窗口的深色视觉只局部生效一帧）。
        theme::apply(ctx);
        self.drain_commands(ctx);

        // Ctrl+S 保存
        let save_hotkey = ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.ctrl);
        if save_hotkey && self.st.editor.as_ref().map(|e| e.dirty).unwrap_or(false) {
            self.save_editor();
        }

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

/// CJK 字体安装：egui 默认字体不含汉字字形。原客户端 UI 字体为微软雅黑，
/// 故 msyh 优先，且插入 Proportional 队首（拉丁/数字同样走雅黑，贴近原版）。
fn install_cjk_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "C:\\Windows\\Fonts\\Dengb.ttf",
        "C:\\Windows\\Fonts\\Deng.ttf",
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
            None => (0, 0),
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
        fn wrap_none_range_inserts_at_document_start() {
            let (out, r) = wrap_enml_around_selection("x", None, "<en-todo/>", "");
            assert_eq!(out, "<en-todo/>x");
            assert_eq!(r.primary.index, 10);
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
