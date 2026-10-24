//! biji CLI——复刻客户端的验收入口。
//!
//! 每个子命令对应服务层的一个能力面；`markup`/`sync` 演示原客户端
//! 双 Thrift 服务的两条命令通路（分析文档 §5）。

use std::io::Read as _;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use biji_service::{
    LoggingFrontend, NodeWebkitCommand, NoteService, note_link,
};
use biji_store::{ListNotes, Store};
use biji_sync::{LocalEdamRemote, MockEdamServer, ServerRemote, SyncEngine, SyncRemote};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "biji",
    version,
    about = "印象笔记复刻（Rust 实现）——数据模型 / ENML / 双服务语义 / USN 同步骨架"
)]
struct Cli {
    /// 库文件路径（默认 ./biji.exb，附件目录自动推导为 biji.exb.attachments，
    /// 对位原客户端布局）
    #[arg(long, global = true, default_value = "biji.exb")]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 初始化库（幂等）
    Init,
    /// 笔记本
    Nb {
        #[command(subcommand)]
        cmd: NbCmd,
    },
    /// 标签
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// 笔记
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
    },
    /// 全文检索（标题 + 正文子串）
    Search { query: String },
    /// 附件
    Res {
        #[command(subcommand)]
        cmd: ResCmd,
    },
    /// 原生→编辑器命令通路演示（原 NodeWebkitService 语义）
    Markup {
        note_uid: i64,
        #[arg(long, default_value = "context-menu")]
        trigger: String,
    },
    /// 同步（USN 记账 + 删除账；目标为自有后端 biji-server-go）
    Sync {
        #[command(subcommand)]
        cmd: SyncCmd,
    },
    /// 账号（原 user#service_host 模型）
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
    /// ENML 工具
    Enml {
        #[command(subcommand)]
        cmd: EnmlCmd,
    },
}

#[derive(Subcommand)]
enum NbCmd {
    Add {
        name: String,
        #[arg(long)]
        stack: Option<String>,
    },
    List,
    Rm { name: String },
}

#[derive(Subcommand)]
enum TagCmd {
    Add { name: String },
    List,
    Attach { name: String, note_uid: i64 },
    Rm { name: String },
}

#[derive(Subcommand)]
enum NoteCmd {
    Add {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        notebook: Option<String>,
        /// 正文文件；缺省读 stdin。ENML 原文或纯文本均可。
        #[arg(long)]
        file: Option<String>,
    },
    Show {
        uid: i64,
        #[arg(long, conflicts_with = "text")]
        html: bool,
        #[arg(long)]
        text: bool,
    },
    List {
        #[arg(long)]
        notebook: Option<String>,
        #[arg(long)]
        trash: bool,
    },
    Rm {
        uid: i64,
        #[arg(long, help = "彻底删除（写删除账）")]
        hard: bool,
    },
    Title { uid: i64, new_title: String },
}

#[derive(Subcommand)]
enum ResCmd {
    Add {
        note_uid: i64,
        file: PathBuf,
        #[arg(long, default_value = "application/octet-stream")]
        mime: String,
    },
    Ls { note_uid: i64 },
    Extract {
        uid: i64,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// PDF 附件分页渲染（对位 DesktopService.renderPage；产出 PNG 路径）
    Render {
        uid: i64,
        #[arg(long, default_value_t = 1)]
        page: i32,
        #[arg(long, default_value_t = 0)]
        width: i32,
        #[arg(long, default_value_t = 0)]
        height: i32,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// 查看待推送变更与同步点
    Status,
    /// 完整同步轮次（pull→apply→push→commit；--demo 用本地 EDAM 服务器）
    Round {
        #[arg(long)]
        demo: bool,
    },
    /// 仅推送（默认指向印象笔记中国区 EDAM 接缝）
    Push {
        #[arg(long)]
        demo: bool,
    },
    /// 免认证握手：对真实印象笔记服务端验证 Thrift 传输（无需凭证）
    Ping,
}

#[derive(Subcommand)]
enum AccountCmd {
    Set {
        user: String,
        #[arg(default_value = "app.yinxiang.com")]
        host: String,
        email: Option<String>,
    },
    Show,
}

#[derive(Subcommand)]
enum EnmlCmd {
    ToText { file: String },
    ToHtml { file: String },
    Strip { file: String },
    Validate { file: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("错误: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Init => {
            Store::open(&cli.db).map_err(|e| e.to_string())?;
            println!(
                "已初始化 {}（附件目录 {}）",
                cli.db.display(),
                biji_store::attachments_dir_for(&cli.db).display()
            );
        }
        Command::Nb { cmd } => nb(&cli.db, cmd)?,
        Command::Tag { cmd } => tag(&cli.db, cmd)?,
        Command::Note { cmd } => note(&cli.db, cmd)?,
        Command::Search { query } => {
            let svc = service(&cli.db)?;
            for n in svc.search_notes(&query).map_err(|e| e.to_string())? {
                println!("{}\t{}", n.id, n.title);
            }
        }
        Command::Res { cmd } => res(&cli.db, cmd)?,
        Command::Markup { note_uid, trigger } => {
            let mut svc = service(&cli.db)?;
            svc.dispatch(NodeWebkitCommand::Markup {
                resource_url: "biji://resource/1".to_string(),
                title: format!("标注：{}", svc.get_note_title(note_uid).map_err(|e| e.to_string())?),
                x: 100,
                y: 100,
                width: 640,
                height: 480,
                trigger,
            });
            svc.dispatch(NodeWebkitCommand::FocusMarkupWindow);
            println!("已派发 NodeWebkitCommand（原 \\\\.\\pipe\\yinxiang.nwserver 通路）：");
            println!("（本实现中命令打印到 stdout，等价于原编辑器进程消费命令）");
        }
        Command::Sync { cmd } => sync(&cli.db, cmd)?,
        Command::Account { cmd } => account(&cli.db, cmd)?,
        Command::Enml { cmd } => enml(cmd)?,
    }
    Ok(())
}

fn service(db: &Path) -> Result<NoteService, String> {
    let store = Store::open(db).map_err(|e| e.to_string())?;
    Ok(NoteService::new(store, Box::new(LoggingFrontend::default())))
}

fn nb(db: &Path, cmd: NbCmd) -> Result<(), String> {
    let svc = service(db)?;
    match cmd {
        NbCmd::Add { name, stack } => {
            let uid = svc
                .store()
                .create_notebook(&name, stack.as_deref())
                .map_err(|e| e.to_string())?;
            println!("notebook {uid} 已创建：{name}");
        }
        NbCmd::List => {
            for nb in svc.store().list_notebooks().map_err(|e| e.to_string())? {
                println!(
                    "{}\t{}\t[{}]\t{} 条",
                    nb.uid, nb.name, nb.stack.as_deref().unwrap_or("-"), nb.note_count
                );
            }
        }
        NbCmd::Rm { name } => {
            let nb = svc
                .store()
                .list_notebooks()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|n| n.name == name)
                .ok_or_else(|| format!("笔记本不存在：{name}"))?;
            svc.store().delete_notebook(nb.uid).map_err(|e| e.to_string())?;
            println!("notebook {} 已删除（笔记移入回收站）", nb.uid);
        }
    }
    Ok(())
}

fn tag(db: &Path, cmd: TagCmd) -> Result<(), String> {
    let svc = service(db)?;
    match cmd {
        TagCmd::Add { name } => {
            let uid = svc.store().create_tag(&name, None).map_err(|e| e.to_string())?;
            println!("tag {uid} 已创建：{name}");
        }
        TagCmd::List => {
            for t in svc.store().list_tags().map_err(|e| e.to_string())? {
                println!("{}\t{}\t{} 条", t.uid, t.name, t.note_count);
            }
        }
        TagCmd::Attach { name, note_uid } => {
            let tag = svc
                .store()
                .list_tags()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|t| t.name == name)
                .ok_or_else(|| format!("标签不存在：{name}"))?;
            svc.store().attach_tag(note_uid, tag.uid).map_err(|e| e.to_string())?;
            println!("已给笔记 {note_uid} 打标签「{name}」");
        }
        TagCmd::Rm { name } => {
            let tag = svc
                .store()
                .list_tags()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|t| t.name == name)
                .ok_or_else(|| format!("标签不存在：{name}"))?;
            svc.store().delete_tag(tag.uid).map_err(|e| e.to_string())?;
            println!("tag {} 已删除（进入删除账）：{name}", tag.uid);
        }
    }
    Ok(())
}

fn note(db: &Path, cmd: NoteCmd) -> Result<(), String> {
    let svc = service(db)?;
    match cmd {
        NoteCmd::Add { title, notebook, file } => {
            let content = read_content(file.as_deref())?;
            let notebook_uid = match notebook {
                Some(name) => {
                    let nb = svc
                        .store()
                        .list_notebooks()
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .find(|n| n.name == name)
                        .ok_or_else(|| format!("笔记本不存在：{name}"))?;
                    Some(nb.uid)
                }
                None => None,
            };
            let uid = svc
                .create_note(title.as_deref().unwrap_or(""), &content, notebook_uid)
                .map_err(|e| e.to_string())?;
            println!("{}", note_link(uid));
        }
        NoteCmd::Show { uid, html, text } => {
            let view = svc.get_note(uid).map_err(|e| e.to_string())?;
            if html {
                print!("{}", view.html);
            } else if text {
                let mut doc = biji_enml::Enml::parse(&view.html)
                    .map_err(|e| e.to_string())?;
                doc.sanitize();
                println!("{}", doc.to_plain_text());
            } else {
                println!("标题:   {}", view.title);
                println!("链接:   {}", note_link(uid));
                println!("来源:   {}", view.source_url.as_deref().unwrap_or("-"));
                println!("作者:   {}", view.author.as_deref().unwrap_or("-"));
                println!("正文:");
                let mut doc = biji_enml::Enml::parse(&view.html)
                    .map_err(|e| e.to_string())?;
                doc.sanitize();
                println!("{}", doc.to_plain_text());
            }
        }
        NoteCmd::List { notebook, trash } => {
            let filter = if trash {
                ListNotes::Trash
            } else if let Some(name) = notebook {
                let nb = svc
                    .store()
                    .list_notebooks()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|n| n.name == name)
                    .ok_or_else(|| format!("笔记本不存在：{name}"))?;
                ListNotes::ByNotebook(nb.uid)
            } else {
                ListNotes::All
            };
            for n in svc.store().list_notes(filter).map_err(|e| e.to_string())? {
                println!(
                    "{}\t{}\t标签[{}]\t{}B",
                    n.uid,
                    n.title,
                    n.tags_text.as_deref().unwrap_or(""),
                    n.size
                );
            }
        }
        NoteCmd::Rm { uid, hard } => {
            if hard {
                svc.store().expunge_note(uid).map_err(|e| e.to_string())?;
                println!("note {uid} 已彻底删除（删除账已记）");
            } else {
                svc.trash_note(uid).map_err(|e| e.to_string())?;
                println!("note {uid} 已移入回收站");
            }
        }
        NoteCmd::Title { uid, new_title } => {
            svc.store()
                .update_note_meta(uid, Some(&new_title), None, None)
                .map_err(|e| e.to_string())?;
            println!("note {uid} 标题已更新");
        }
    }
    Ok(())
}

fn res(db: &Path, cmd: ResCmd) -> Result<(), String> {
    let svc = service(db)?;
    match cmd {
        ResCmd::Add { note_uid, file, mime } => {
            let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
            let name = file
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "attachment.bin".into());
            let uid = svc
                .store()
                .add_resource(note_uid, &name, &mime, &bytes)
                .map_err(|e| e.to_string())?;
            println!("resource {uid} 已添加（{}B）", bytes.len());
        }
        ResCmd::Ls { note_uid } => {
            for r in svc.store().list_resources(note_uid).map_err(|e| e.to_string())? {
                println!("{}\t{}\t{}\t{}B", r.uid, r.file_name, r.mime, r.size);
            }
        }
        ResCmd::Extract { uid, out } => {
            let (meta, bytes) = svc.store().get_resource(uid).map_err(|e| e.to_string())?;
            let target = out.unwrap_or_else(|| PathBuf::from(&meta.file_name));
            std::fs::write(&target, &bytes).map_err(|e| e.to_string())?;
            println!("已导出 {}B → {}", bytes.len(), target.display());
        }
        ResCmd::Render { uid, page, width, height } => {
            let url = format!("biji://resource/{uid}");
            let path = svc
                .render_page(&url, 0, width, height, page)
                .map_err(|e| e.to_string())?;
            println!("已渲染第 {page} 页 → {path}");
        }
    }
    Ok(())
}

fn sync(db: &Path, cmd: SyncCmd) -> Result<(), String> {
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let engine = SyncEngine::new(&store);
    match cmd {
        SyncCmd::Status => {
            let plan = engine.plan_push().map_err(|e| e.to_string())?;
            println!("同步点 USN: {}", engine.last_synced_usn());
            println!("待推送变更: {} 条", plan.changes.len());
            for c in &plan.changes {
                println!("  [{}] {} {} (usn {})", c.kind, c.op, c.item_uid, c.usn);
            }
            println!("待推送删除: {} 条", plan.expunged.len());
            for e in &plan.expunged {
                println!("  [{}] {} (usn {})", e.kind, e.item_uid, e.usn);
            }
        }
        SyncCmd::Ping => {
            let remote = ServerRemote::connect();
            match remote.check_version("biji") {
                Ok(true) => println!(
                    "握手成功：自有后端（{}）确认 EDAM 协议兼容（Thrift 传输验证通过）",
                    remote.service_host()
                ),
                Ok(false) => println!("握手成功但服务端报告协议版本不兼容"),
                Err(e) => println!("握手失败: {e}"),
            }
        }
        SyncCmd::Round { demo } => {
            let mut remote: Box<dyn SyncRemote> = if demo {
                Box::new(LocalEdamRemote::new(Rc::new(std::cell::RefCell::new(
                    MockEdamServer::new(),
                ))))
            } else {
                Box::new(ServerRemote::connect())
            };
            match engine.sync_round(&mut remote) {
                Ok(r) => {
                    println!(
                        "同步轮次完成：推送 {} 条，拉取笔记 {} 条、标签 {} 条、删除 {} 条，服务器 USN {}",
                        r.pushed, r.pulled_notes, r.pulled_tags, r.pulled_expunged, r.server_usn
                    );
                }
                Err(biji_sync::SyncError::Unsupported(msg)) => {
                    return Err(format!(
                        "自有后端未接通：{msg}（或用 --demo 走内置 mock 闭环）"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        SyncCmd::Push { demo } => {
            let result = if demo {
                let mut remote: Box<dyn SyncRemote> = Box::new(LocalEdamRemote::new(Rc::new(
                    std::cell::RefCell::new(MockEdamServer::new()),
                )));
                engine.push_via(&mut remote)
            } else {
                let mut remote = ServerRemote::connect();
                engine.push_via(&mut remote)
            };
            match result {
                Ok(0) => println!("无待推送变更"),
                Ok(n) => println!("已推送 {n} 条变更，同步点推进至 {}", engine.last_synced_usn()),
                Err(biji_sync::SyncError::Unsupported(msg)) => {
                    return Err(format!("自有后端未接通：{msg}（或用 --demo 演示内置 mock 闭环）"));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(())
}

fn account(db: &Path, cmd: AccountCmd) -> Result<(), String> {
    let svc = service(db)?;
    match cmd {
        AccountCmd::Set { user, host, email } => {
            svc.set_account(&user, &host, email.as_deref())
                .map_err(|e| e.to_string())?;
            println!("账号已设置：{user}#{host}");
        }
        AccountCmd::Show => {
            let u = svc.get_current_user_info().map_err(|e| e.to_string())?;
            println!("{}#{}（{}）", u.username, u.service_host, u.email.as_deref().unwrap_or("未绑定邮箱"));
        }
    }
    Ok(())
}

fn enml(cmd: EnmlCmd) -> Result<(), String> {
    let (action, file) = match cmd {
        EnmlCmd::ToText { file } => ("to-text", file),
        EnmlCmd::ToHtml { file } => ("to-html", file),
        EnmlCmd::Strip { file } => ("strip", file),
        EnmlCmd::Validate { file } => ("validate", file),
    };
    let input = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let mut doc = biji_enml::Enml::parse_loose(&input).map_err(|e| e.to_string())?;
    match action {
        "to-text" => println!("{}", doc.to_plain_text()),
        "to-html" => println!("{}", doc.to_html_fragment()),
        "strip" => {
            doc.sanitize();
            println!("{}", doc.to_xml());
        }
        "validate" => {
            let violations = doc.validate();
            if violations.is_empty() {
                println!("ENML 合法");
            } else {
                let count = violations.len();
                for v in &violations {
                    println!("违例: {v}");
                }
                return Err(format!("共 {count} 处违例"));
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn read_content(file: Option<&str>) -> Result<String, String> {
    match file {
        Some("-") | None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| e.to_string())?;
            Ok(buf)
        }
        Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string()),
    }
}
