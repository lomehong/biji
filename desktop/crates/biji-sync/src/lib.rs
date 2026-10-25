//! 同步闭环：USN 记账 + 删除账 + EDAM 风格本地服务器。
//!
//! 对应分析文档 §7.3（`sync_journal`/`expunged_items`）与 §6（EDAM 协议）：
//! - `SyncEngine::plan_push` 汇总本地日志为推送计划（含标题/正文快照）；
//! - [`SyncRemote`] 双向协议：`upload`（携带身份解析器，guid ↔ 本地 uid）
//!   与 `pull`（增量拉取服务器变更与删除账）；
//! - [`MockEdamServer`] 是进程内 EDAM 风格服务器（update_count 单调递增、
//!   每对象独立 usn、expunged 删除账），[`LocalEdamRemote`] 为其客户端实现；
//! - `SyncEngine::sync_round` 跑通 **pull → apply → push → commit** 全轮次，
//!   双客户端经由同一服务器即可互见对方的笔记、更新与删除。
//!
//! 同步远端是**自研后端 biji-server-go**（`ServerRemote`，EDAM 风格
//! Thrift over HTTP，服务端即本仓库 `biji-server-go/`）：
//! 默认 `http://127.0.0.1:8332` + `biji/biji`，开箱直连，零环境变量；
//! [`MockEdamServer`]/[`LocalEdamRemote`] 保留为离线演示（CLI `--demo`）与测试闭环。

pub mod edam;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use biji_store::Store;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("存储层错误: {0}")]
    Store(#[from] biji_store::StoreError),
    #[error("远端暂不支持: {0}")]
    Unsupported(&'static str),
    #[error("网络传输错误: {0}")]
    Transport(String),
    #[error("服务端拒绝: {0}")]
    Edam(String),
}

/// 自有后端（biji-server-go）默认地址：与其 `-addr` 默认值一致。
pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8332";
/// 自有后端认证用户名环境变量（与服务端 `BIJI_SERVER_USER` 同名同默认）。
pub const SERVER_USER_ENV: &str = "BIJI_SERVER_USER";
/// 自有后端认证密码环境变量（与服务端 `BIJI_SERVER_PASS` 同名同默认）。
pub const SERVER_PASS_ENV: &str = "BIJI_SERVER_PASS";
/// 覆盖自有后端地址（默认 [`DEFAULT_SERVER_URL`]；非本机部署时设置）。
pub const SERVER_URL_ENV: &str = "BIJI_SERVER_URL";

const LAST_USN_KEY: &str = "sync.last_usn";
/// 推送侧本地 journal 水位（与服务器同步点分离，见 `journal_push_mark`）。
const JOURNAL_MARK_KEY: &str = "sync.journal_mark";

// ---------- 推送/拉取数据模型 ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: String,
    pub item_uid: i64,
    pub op: String,
    pub usn: i64,
    /// note 变更的标题快照。
    pub title: Option<String>,
    /// note 变更的 ENML 正文快照。
    pub content: Option<String>,
    /// notebook/tag 变更的名称快照。
    pub name: Option<String>,
    /// notebook 变更的分组快照（Some 携带 f7，空串清除；None 不携带）。
    pub stack: Option<String>,
    /// note 变更的本地笔记本 uid（上云时经 identity 映射为服务器 guid）。
    pub notebook_uid: Option<i64>,
    /// resource 变更载荷：所属笔记 + 该笔记全部附件（含字节）。
    pub resource: Option<ResourceUpload>,
}

/// resource 变更的上行载荷：重推所属笔记（全量附件随笔记上云）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceUpload {
    pub note_uid: i64,
    pub title: String,
    pub content: String,
    pub notebook_uid: Option<i64>,
    pub items: Vec<edam::ResourceUploadItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpungedItem {
    pub item_uid: i64,
    pub kind: String,
    pub usn: i64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PushPlan {
    pub since_usn: i64,
    pub changes: Vec<Change>,
    pub expunged: Vec<ExpungedItem>,
}

impl PushPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.expunged.is_empty()
    }
}

/// 服务器为首次上传的对象分配 guid 后回执：`(kind, local_uid, guid)`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UploadAck {
    pub usn: u64,
    pub assignments: Vec<(String, i64, String)>,
}

/// 服务器拉取的单条笔记（EDAM Note 的最小投影）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteNote {
    pub guid: String,
    pub title: String,
    pub content: String,
    pub deleted: bool,
    pub updated: u64,
    pub notebook: Option<String>,
    /// 附件资源（chunk 只带元数据，body 经 `fetch_resource` 按需拉取；mock 为空）。
    pub resources: Vec<edam::RemoteEdamResource>,
}

/// 服务器拉取结果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PullChanges {
    pub usn: u64,
    pub notes: Vec<RemoteNote>,
    /// 远端笔记本（EDAM：服务器 guid + 名称；mock 为空 Vec）。
    pub notebooks: Vec<RemoteNotebook>,
    /// 远端标签（EDAM：服务器 guid + 名称）。
    pub tags: Vec<RemoteTag>,
    /// `(guid, kind)`。
    pub expunged: Vec<(String, String)>,
}

/// 远端笔记本（拉取方向）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteNotebook {
    pub guid: String,
    pub name: String,
    /// 笔记本分组（None = 远端未携带，保持本地现值）。
    pub stack: Option<String>,
}

/// 远端标签（拉取方向）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTag {
    pub guid: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncReport {
    pub pushed: usize,
    pub pulled_notes: usize,
    pub pulled_tags: usize,
    pub pulled_expunged: usize,
    pub server_usn: u64,
}

/// 身份解析：服务器 guid ↔ 本地 uid 的查询（已映射）与铸造请求（未映射）。
pub struct IdentityResolver<'a> {
    store: &'a Store,
    inflight: HashMap<(String, i64), String>,
    minted: Vec<(String, i64, String)>,
}

impl<'a> IdentityResolver<'a> {
    pub fn new(store: &'a Store) -> IdentityResolver<'a> {
        IdentityResolver {
            store,
            inflight: HashMap::new(),
            minted: Vec::new(),
        }
    }

    /// 只查不铸：已有映射（含本轮 createNote 回填）返回 guid，否则 None。
    /// 真实 EDAM 上传用它区分 create/update 与可跳过的远端删除。
    pub fn lookup(&self, kind: &str, local_uid: i64) -> Option<String> {
        self.inflight
            .get(&(kind.to_string(), local_uid))
            .cloned()
            .or_else(|| self.store.identity_get_guid(kind, local_uid))
    }

    /// 已映射则返回 guid；未映射则铸造新 guid 并记录回执。
    pub fn get_or_mint(&mut self, kind: &str, local_uid: i64) -> String {
        let key = (kind.to_string(), local_uid);
        if let Some(g) = self.inflight.get(&key) {
            return g.clone();
        }
        if let Some(g) = self.store.identity_get_guid(kind, local_uid) {
            self.inflight.insert(key, g.clone());
            return g;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let g = format!("{kind}-{local_uid}-{nanos}");
        self.inflight.insert(key.clone(), g.clone());
        self.minted.push((kind.to_string(), local_uid, g.clone()));
        g
    }

    pub fn take_minted(&mut self) -> Vec<(String, i64, String)> {
        std::mem::take(&mut self.minted)
    }

    /// 回填服务器分配的真实 guid（真实 EDAM createNote 后调用）：
    /// 记入 inflight（本轮后续引用）与 minted（UploadAck 分配表，由引擎
    /// 持久化到 identity 表）。
    pub fn record_external(&mut self, kind: &str, local_uid: i64, guid: &str) {
        let key = (kind.to_string(), local_uid);
        if self.inflight.contains_key(&key)
            || self.store.identity_get_guid(kind, local_uid).is_some()
        {
            return; // 已有映射：服务器 guid 以既有映射为准，不回填
        }
        self.inflight.insert(key.clone(), guid.to_string());
        self.minted
            .push((kind.to_string(), local_uid, guid.to_string()));
    }
}

// ---------- 双向协议 trait ----------

pub trait SyncRemote {
    fn name(&self) -> &str;
    fn service_host(&self) -> &str;
    /// 推送变更；服务器为首次上传对象分配 guid（经 `identities` 解析/铸造），
    /// 返回确认 USN 与分配表。
    fn upload(&mut self, plan: &PushPlan, identities: &mut IdentityResolver) -> Result<UploadAck>;
    /// 拉取服务器上 usn > after 的笔记变更与删除账。
    fn pull(&mut self, after_usn: u64) -> Result<PullChanges>;
    /// 按需拉取单个附件的全量数据（chunk 只带元数据 → getResource 取 body）。
    /// 默认实现返回 `None`：不支持按需拉取的远端（内存 mock）由调用方跳过。
    fn fetch_resource(&mut self, _guid: &str) -> Result<Option<edam::RemoteEdamResource>> {
        Ok(None)
    }
}

/// 装箱远端同样可作为远端使用（GUI/CLI 组合多个后端时方便）。
impl SyncRemote for Box<dyn SyncRemote> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn service_host(&self) -> &str {
        (**self).service_host()
    }

    fn upload(&mut self, plan: &PushPlan, identities: &mut IdentityResolver) -> Result<UploadAck> {
        (**self).upload(plan, identities)
    }

    fn pull(&mut self, after_usn: u64) -> Result<PullChanges> {
        (**self).pull(after_usn)
    }

    fn fetch_resource(&mut self, guid: &str) -> Result<Option<edam::RemoteEdamResource>> {
        (**self).fetch_resource(guid)
    }
}

// ---------- MockEdamServer：进程内 EDAM 风格服务器 ----------

#[derive(Debug, Clone)]
pub struct ServerNote {
    pub guid: String,
    pub title: String,
    pub content: String,
    pub deleted: bool,
    pub usn: u64,
}

#[derive(Debug, Clone)]
struct ServerExpunged {
    guid: String,
    kind: String,
    usn: u64,
}

/// 内存 EDAM 风格服务器：`update_count` 单调递增，每个对象携带最近一次
/// 变更的 usn，删除进入删除账——与 EDAM 的增量拉取模型一致。
#[derive(Debug, Default)]
pub struct MockEdamServer {
    update_count: u64,
    notes: HashMap<String, ServerNote>,
    notebooks: HashMap<String, (String, u64)>,
    tags: HashMap<String, (String, u64)>,
    expunged: Vec<ServerExpunged>,
}

impl MockEdamServer {
    pub fn new() -> MockEdamServer {
        MockEdamServer::default()
    }

    pub fn update_count(&self) -> u64 {
        self.update_count
    }

    pub fn note_count(&self) -> usize {
        self.notes.values().filter(|n| !n.deleted).count()
    }

    pub fn server_note(&self, guid: &str) -> Option<&ServerNote> {
        self.notes.get(guid)
    }

    pub fn expunged_count(&self) -> usize {
        self.expunged.len()
    }

    fn next_usn(&mut self) -> u64 {
        self.update_count += 1;
        self.update_count
    }

    fn apply_change(&mut self, ch: &Change, guid: &str) {
        match (ch.kind.as_str(), ch.op.as_str()) {
            ("note", "expunge") => {
                let usn = self.next_usn();
                if let Some(note) = self.notes.get_mut(guid) {
                    note.deleted = true;
                    note.usn = usn;
                }
                self.expunged.push(ServerExpunged {
                    guid: guid.to_string(),
                    kind: "note".into(),
                    usn,
                });
            }
            ("note", _) => {
                let usn = self.next_usn();
                let note = self
                    .notes
                    .entry(guid.to_string())
                    .or_insert_with(|| ServerNote {
                        guid: guid.to_string(),
                        title: String::new(),
                        content: String::new(),
                        deleted: false,
                        usn,
                    });
                note.title = ch.title.clone().unwrap_or_default();
                note.content = ch.content.clone().unwrap_or_default();
                note.usn = usn;
                note.deleted = false;
            }
            ("notebook", "expunge") => {
                let usn = self.next_usn();
                self.notebooks.remove(guid);
                self.expunged.push(ServerExpunged {
                    guid: guid.to_string(),
                    kind: "notebook".into(),
                    usn,
                });
            }
            ("notebook", _) => {
                let usn = self.next_usn();
                self.notebooks
                    .insert(guid.to_string(), (ch.name.clone().unwrap_or_default(), usn));
            }
            ("tag", "expunge") => {
                let usn = self.next_usn();
                self.tags.remove(guid);
                self.expunged.push(ServerExpunged {
                    guid: guid.to_string(),
                    kind: "tag".into(),
                    usn,
                });
            }
            ("tag", _) => {
                let usn = self.next_usn();
                self.tags
                    .insert(guid.to_string(), (ch.name.clone().unwrap_or_default(), usn));
            }
            _ => {
                // resource 等未建模种类：仅推进 USN 记账。
                self.next_usn();
            }
        }
    }

    fn upload(&mut self, plan: &PushPlan, identities: &mut IdentityResolver) -> UploadAck {
        let mut ack = UploadAck::default();
        for ch in &plan.changes {
            let guid = identities.get_or_mint(&ch.kind, ch.item_uid);
            self.apply_change(ch, &guid);
        }
        for ex in &plan.expunged {
            let guid = identities.get_or_mint(&ex.kind, ex.item_uid);
            self.apply_change(
                &Change {
                    kind: ex.kind.clone(),
                    item_uid: ex.item_uid,
                    op: "expunge".into(),
                    usn: ex.usn,
                    title: None,
                    content: None,
                    name: None,
                    notebook_uid: None,
                    resource: None,
                    stack: None,
                },
                &guid,
            );
        }
        ack.usn = self.update_count;
        ack
    }

    fn pull(&self, after_usn: u64) -> PullChanges {
        let mut changes = PullChanges {
            usn: self.update_count,
            ..Default::default()
        };
        let mut notes: Vec<&ServerNote> = self
            .notes
            .values()
            .filter(|n| n.usn > after_usn && !n.deleted)
            .collect();
        notes.sort_by_key(|n| n.usn);
        for n in notes {
            changes.notes.push(RemoteNote {
                guid: n.guid.clone(),
                title: n.title.clone(),
                content: n.content.clone(),
                deleted: false,
                updated: n.usn,
                notebook: None,
                resources: Vec::new(),
            });
        }
        // 笔记本/标签按 usn 增量回传（名称 + guid，与 EDAM chunk 语义一致）
        let mut notebooks: Vec<(&String, &(String, u64))> = self
            .notebooks
            .iter()
            .filter(|(_, (_, usn))| *usn > after_usn)
            .collect();
        notebooks.sort_by_key(|(_, (_, usn))| *usn);
        for (guid, (name, _)) in notebooks {
            changes.notebooks.push(RemoteNotebook {
                guid: guid.clone(),
                name: name.clone(),
                stack: None,
            });
        }
        let mut tags: Vec<(&String, &(String, u64))> = self
            .tags
            .iter()
            .filter(|(_, (_, usn))| *usn > after_usn)
            .collect();
        tags.sort_by_key(|(_, (_, usn))| *usn);
        for (guid, (name, _)) in tags {
            changes.tags.push(RemoteTag {
                guid: guid.clone(),
                name: name.clone(),
            });
        }
        for ex in &self.expunged {
            if ex.usn > after_usn {
                changes.expunged.push((ex.guid.clone(), ex.kind.clone()));
            }
        }
        changes
    }
}

/// [`MockEdamServer`] 的客户端实现（共享所有权，供测试同时观察服务器状态）。
#[derive(Clone)]
pub struct LocalEdamRemote {
    server: Rc<RefCell<MockEdamServer>>,
    pub host: String,
}

impl LocalEdamRemote {
    pub fn new(server: Rc<RefCell<MockEdamServer>>) -> LocalEdamRemote {
        LocalEdamRemote {
            server,
            host: "local-edam".into(),
        }
    }
}

impl SyncRemote for LocalEdamRemote {
    fn name(&self) -> &str {
        "local-edam"
    }

    fn service_host(&self) -> &str {
        &self.host
    }

    fn upload(&mut self, plan: &PushPlan, identities: &mut IdentityResolver) -> Result<UploadAck> {
        Ok(self.server.borrow_mut().upload(plan, identities))
    }

    fn pull(&mut self, after_usn: u64) -> Result<PullChanges> {
        Ok(self.server.borrow().pull(after_usn))
    }
}

/// 自有后端远端（biji-server-go，EDAM Thrift over HTTP，`edam` 模块承载传输）。
///
/// 连接即产品形态：`ServerRemote::connect()` 以 `BIJI_SERVER_URL/USER/PASS`
/// （默认 `http://127.0.0.1:8332` + `biji/biji`）经 UserStore.authenticate
/// 换取认证 token；认证或网络未通时构造仍成功，同步调用诚实报错并给联调指引。
pub struct ServerRemote {
    pub service_host: String,
    client: Option<edam::EdamClient>,
    note_store_url: Option<String>,
}

impl ServerRemote {
    /// 免认证握手：对自有后端验证 Thrift 传输与消息封装（无需凭证）。
    pub fn check_version(&self, client_name: &str) -> Result<bool> {
        match &self.client {
            Some(c) => c.check_version(client_name).map_err(SyncError::Edam),
            // checkVersion 免认证：认证未通也能对服务端验证传输层
            None => edam::EdamClient::new(&self.service_host, "")
                .check_version(client_name)
                .map_err(SyncError::Edam),
        }
    }

    /// 连接自有后端（产品唯一同步目标）：地址/账密取环境变量，缺省
    /// `http://127.0.0.1:8332` + `biji/biji`——与服务端默认一致，零配置直连。
    /// 认证失败不 panic：保留未认证句柄，同步调用时给出联调指引。
    pub fn connect() -> ServerRemote {
        let non_empty = |k: &str, default: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let url = non_empty(SERVER_URL_ENV, DEFAULT_SERVER_URL);
        let user = non_empty(SERVER_USER_ENV, "biji");
        let pass = non_empty(SERVER_PASS_ENV, "biji");
        // authenticate 换认证 token（服务端 BIJI_SERVER_USER/PASS 默认 biji/biji）
        match edam::EdamClient::new(&url, "").authenticate(&user, &pass) {
            Ok(t) => ServerRemote::with_token(&url, Some(t)),
            Err(_) => ServerRemote::with_token(&url, None),
        }
    }

    pub fn with_token(host: &str, token: Option<String>) -> ServerRemote {
        let client = token.as_ref().map(|t| edam::EdamClient::new(host, t));
        ServerRemote {
            service_host: host.to_string(),
            client,
            note_store_url: None,
        }
    }

    /// 解析 NoteStore 地址并返回可克隆的客户端句柄；
    /// 认证未通时诚实报错并给出自有后端联调指引。
    fn require_client(&mut self) -> Result<edam::EdamClient> {
        if self.client.is_none() {
            return Err(SyncError::Unsupported(
                "自有后端认证未通：UserStore.authenticate 失败（默认账密 biji/biji，经 BIJI_SERVER_USER/BIJI_SERVER_PASS 覆盖；请确认 biji-server-go 已启动，默认 http://127.0.0.1:8332，经 BIJI_SERVER_URL 覆盖）",
            ));
        }
        if self.note_store_url.is_none() {
            let url = self
                .client
                .as_ref()
                .unwrap()
                .get_note_store_url()
                .map_err(SyncError::Edam)?;
            self.note_store_url = Some(url);
        }
        Ok(self.client.as_ref().unwrap().clone())
    }

    fn note_url(&self) -> &str {
        self.note_store_url.as_deref().unwrap_or("")
    }

    /// 测试钩子：注入内存传输与预设 NoteStore 地址（绕过网络）。
    pub fn set_transport(
        &mut self,
        token: &str,
        transport: std::rc::Rc<edam::TransportFn>,
        note_store_url: String,
    ) {
        self.client = Some(edam::EdamClient::with_transport(
            &self.service_host,
            token,
            transport,
        ));
        self.note_store_url = Some(note_store_url);
    }
}

impl SyncRemote for ServerRemote {
    fn name(&self) -> &str {
        "server-go"
    }

    fn service_host(&self) -> &str {
        &self.service_host
    }

    fn upload(&mut self, plan: &PushPlan, identities: &mut IdentityResolver) -> Result<UploadAck> {
        let client = self.require_client()?;
        let url = self.note_url().to_string();

        for ch in &plan.changes {
            let guid_existing = identities.lookup(&ch.kind, ch.item_uid);
            let op = ch.op.as_str();
            if op == "expunge" {
                // 远端无此对象则无事可做
                if let Some(guid) = &guid_existing {
                    let usn = match ch.kind.as_str() {
                        "notebook" => client.expunge_notebook(&url, guid),
                        "tag" => client.expunge_tag(&url, guid),
                        _ => client.expunge_note(&url, guid),
                    }
                    .map_err(SyncError::Edam)?;
                    let _ = usn;
                }
                continue;
            }
            match ch.kind.as_str() {
                "note" => {
                    let title = ch.title.as_deref().unwrap_or("");
                    let content = ch.content.as_deref().unwrap_or("");
                    // 本地笔记本 uid → 服务器 guid（推送顺序保证笔记本变更先于笔记）
                    let nb_guid = ch
                        .notebook_uid
                        .and_then(|uid| identities.lookup("notebook", uid));
                    let result: edam::RemoteNoteResult = match &guid_existing {
                        Some(guid) => {
                            client.update_note(&url, guid, title, content, nb_guid.as_deref(), &[])
                        }
                        None => client.create_note(&url, title, content, nb_guid.as_deref(), &[]),
                    }
                    .map_err(SyncError::Edam)?;
                    if guid_existing.is_none() {
                        eprintln!("[server-go] record note {} -> {}", ch.item_uid, result.guid);
                        identities.record_external("note", ch.item_uid, &result.guid);
                    }
                }
                "resource" => {
                    // 附件变更优先走 putResource 快路径：该资源已有服务端 guid
                    // （上一轮 pull 经 chunk 元数据建立 identity）时单传字节，
                    // 不重推整条笔记。失败/无身份则回退全量重推。
                    let Some(payload) = &ch.resource else {
                        continue;
                    };
                    let Some(note_guid) = identities.lookup("note", payload.note_uid) else {
                        continue; // 所属笔记尚未上云（journal 序保证先笔记后附件）
                    };
                    if let Some(res_guid) = identities.lookup("resource", ch.item_uid) {
                        if let Some(item) =
                            payload.items.iter().find(|it| it.local_uid == ch.item_uid)
                        {
                            if client
                                .put_resource(
                                    &url,
                                    &res_guid,
                                    &note_guid,
                                    &item.bytes,
                                    &item.mime,
                                    &item.file_name,
                                )
                                .is_ok()
                            {
                                continue; // 快路径成功：跳过整笔记重推
                            }
                        }
                    }
                    // 回退：以所属笔记身份重推（全量附件随笔记上云，整体替换）
                    let nb_guid = payload
                        .notebook_uid
                        .and_then(|uid| identities.lookup("notebook", uid));
                    client
                        .update_note(
                            &url,
                            &note_guid,
                            &payload.title,
                            &payload.content,
                            nb_guid.as_deref(),
                            &payload.items,
                        )
                        .map_err(SyncError::Edam)?;
                }
                "notebook" => {
                    let name = ch.name.as_deref().unwrap_or("");
                    let stack = ch.stack.as_deref();
                    if let Some(guid) = &guid_existing {
                        client
                            .update_notebook(&url, guid, name, stack)
                            .map_err(SyncError::Edam)?;
                    } else {
                        let guid = client
                            .create_notebook(&url, name, stack)
                            .map_err(SyncError::Edam)?;
                        identities.record_external("notebook", ch.item_uid, &guid);
                    }
                }
                "tag" => {
                    let name = ch.name.as_deref().unwrap_or("");
                    if let Some(guid) = &guid_existing {
                        client
                            .update_named(&url, "updateTag", guid, name)
                            .map_err(SyncError::Edam)?;
                    } else {
                        let guid = client
                            .create_named(&url, "createTag", name)
                            .map_err(SyncError::Edam)?;
                        identities.record_external("tag", ch.item_uid, &guid);
                    }
                }
                _ => {}
            }
        }
        let usn = client.get_sync_state(&url).map_err(SyncError::Edam)?;
        // assignments 留给 sync_round 统一 take_minted 回填（与 Mock 约定一致）
        Ok(UploadAck {
            usn,
            assignments: Vec::new(),
        })
    }

    fn pull(&mut self, after_usn: u64) -> Result<PullChanges> {
        let client = self.require_client()?;
        let url = self.note_url().to_string();
        let mut notes = Vec::new();
        let mut chunk_notebooks = Vec::new();
        let mut chunk_tags = Vec::new();
        let mut expunged = Vec::new();
        let mut usn = after_usn;
        let server_usn = client.get_sync_state(&url).map_err(SyncError::Edam)?;
        // 分页拉取：chunkHighUSN 追至 updateCount 为止
        let mut cursor = after_usn as i32;
        loop {
            let chunk = client
                .get_sync_chunk(&url, cursor, 256)
                .map_err(SyncError::Edam)?;
            for n in &chunk.notes {
                notes.push(RemoteNote {
                    guid: n.guid.clone(),
                    title: n.title.clone(),
                    content: n.content.clone(),
                    deleted: !n.active,
                    updated: n.updated.max(0) as u64,
                    notebook: n.notebook.clone(),
                    resources: n.resources.clone(),
                });
            }
            for nb in &chunk.notebooks {
                chunk_notebooks.push(RemoteNotebook {
                    guid: nb.guid.clone(),
                    name: nb.name.clone(),
                    stack: if nb.stack.is_empty() {
                        None
                    } else {
                        Some(nb.stack.clone())
                    },
                });
            }
            for tg in &chunk.tags {
                chunk_tags.push(RemoteTag {
                    guid: tg.guid.clone(),
                    name: tg.name.clone(),
                });
            }
            for guid in &chunk.expunged_note_guids {
                expunged.push((guid.clone(), "note".to_string()));
            }
            for guid in &chunk.expunged_notebook_guids {
                expunged.push((guid.clone(), "notebook".to_string()));
            }
            for guid in &chunk.expunged_tag_guids {
                expunged.push((guid.clone(), "tag".to_string()));
            }
            usn = usn.max(chunk.chunk_high_usn).max(chunk.update_count);
            if chunk.chunk_high_usn >= server_usn
                || chunk.notes.is_empty()
                    && chunk.expunged_note_guids.is_empty()
                    && chunk.expunged_notebook_guids.is_empty()
                    && chunk.expunged_tag_guids.is_empty()
                    && chunk.notebooks.is_empty()
                    && chunk.tags.is_empty()
            {
                break;
            }
            cursor = chunk.chunk_high_usn as i32;
        }
        usn = usn.max(server_usn);
        Ok(PullChanges {
            usn,
            notes,
            notebooks: chunk_notebooks,
            tags: chunk_tags,
            expunged,
        })
    }

    fn fetch_resource(&mut self, guid: &str) -> Result<Option<edam::RemoteEdamResource>> {
        let client = self.require_client()?;
        let url = self.note_url().to_string();
        client
            .get_resource(&url, guid)
            .map(Some)
            .map_err(SyncError::Edam)
    }
}

// ---------- 引擎 ----------

pub struct SyncEngine<'a> {
    store: &'a Store,
}

impl<'a> SyncEngine<'a> {
    pub fn new(store: &'a Store) -> SyncEngine<'a> {
        SyncEngine { store }
    }

    pub fn last_synced_usn(&self) -> i64 {
        self.store
            .meta_get(LAST_USN_KEY)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    /// 推送侧本地 journal 水位（与服务器域的 [`LAST_USN_KEY`] 分离——
    /// 两域计数独立，共用一个游标会在边界 USN 上漏推条目：
    /// 本地 journal usn=6 的删除账被服务器回执 usn=6 的同步点越过）。
    /// 未记录时回退旧语义（服务器同步点，兼容既有库）。
    pub fn journal_push_mark(&self) -> i64 {
        self.store
            .meta_get(JOURNAL_MARK_KEY)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| self.last_synced_usn())
    }

    /// 推送成功后推进本地 journal 水位：取计划内条目的最大本地 USN
    /// （只前进不回退；空计划不推进——未见过更高的条目不能越过）。
    fn mark_journal_pushed(&self, plan: &PushPlan) -> Result<()> {
        let high = plan
            .changes
            .iter()
            .map(|c| c.usn)
            .chain(plan.expunged.iter().map(|e| e.usn))
            .max()
            .unwrap_or(i64::MIN);
        if high == i64::MIN {
            return Ok(());
        }
        let next = high.max(self.journal_push_mark());
        self.store.meta_set(JOURNAL_MARK_KEY, &next.to_string())?;
        Ok(())
    }

    pub fn pending_count(&self) -> Result<usize> {
        Ok(self
            .store
            .sync_journal_since(self.journal_push_mark())?
            .len())
    }

    /// 汇总上次同步点之后的全部变更与删除（含标题/正文/名称快照）。
    /// 推送侧游标用本地 journal 水位（`journal_push_mark`），
    /// 与服务器域同步点分离，避免两域边界 USN 互噬漏推。
    pub fn plan_push(&self) -> Result<PushPlan> {
        let since = self.journal_push_mark();
        let mut changes = Vec::new();
        for e in self.store.sync_journal_since(since)? {
            let (title, content, name, notebook_uid, resource, stack) = match e.kind.as_str() {
                "note" => {
                    let (t, c, nb) = self
                        .store
                        .get_note(e.item_uid)
                        .map(|f| (Some(f.note.title), Some(f.enml), f.note.notebook_uid))
                        .unwrap_or((None, None, None));
                    (t, c, None, nb, None, None)
                }
                "notebook" => (
                    None,
                    None,
                    self.store.list_notebooks().ok().and_then(|nbs| {
                        nbs.into_iter()
                            .find(|n| n.uid == e.item_uid)
                            .map(|n| n.name)
                    }),
                    None,
                    None,
                    // 分组快照：Some("") 清除分组，Some(s) 设置，None 不携带
                    self.store
                        .notebook_stack(e.item_uid)
                        .ok()
                        .flatten()
                        .map(|s| s.unwrap_or_default()),
                ),
                "tag" => (
                    None,
                    None,
                    self.store.list_tags().ok().and_then(|ts| {
                        ts.into_iter().find(|t| t.uid == e.item_uid).map(|t| t.name)
                    }),
                    None,
                    None,
                    None,
                ),
                "resource" => {
                    // 附件变更 = 重推所属笔记（全量附件随笔记上云）
                    let payload = self
                        .store
                        .list_resource_meta(e.item_uid)
                        .ok()
                        .flatten()
                        .and_then(|meta| {
                            let full = self.store.get_note(meta.note_uid).ok()?;
                            let items: Vec<_> = self
                                .store
                                .list_resources(meta.note_uid)
                                .ok()?
                                .into_iter()
                                .filter_map(|r| {
                                    let bytes = self.store.get_resource(r.uid).ok()?.1;
                                    Some(edam::ResourceUploadItem {
                                        local_uid: r.uid,
                                        file_name: r.file_name,
                                        mime: r.mime,
                                        bytes,
                                    })
                                })
                                .collect();
                            Some(ResourceUpload {
                                note_uid: meta.note_uid,
                                title: full.note.title,
                                content: full.enml,
                                notebook_uid: full.note.notebook_uid,
                                items,
                            })
                        });
                    (None, None, None, None, payload, None)
                }
                _ => (None, None, None, None, None, None),
            };
            changes.push(Change {
                kind: e.kind,
                item_uid: e.item_uid,
                op: e.op,
                usn: e.usn,
                title,
                content,
                name,
                notebook_uid,
                resource,
                stack,
            });
        }
        let expunged = self
            .store
            .expunged_since(since)?
            .into_iter()
            .map(|(item_uid, kind, usn)| ExpungedItem {
                item_uid,
                kind,
                usn,
            })
            .collect();
        Ok(PushPlan {
            since_usn: since,
            changes,
            expunged,
        })
    }

    /// 远端确认后推进本地同步点。只前进不回退：
    /// 空轮次时回执 USN 可能小于已推进的同步点（如空服务器）。
    pub fn commit_push(&self, acked_usn: u64) -> Result<()> {
        let next = (acked_usn as i64).max(self.last_synced_usn());
        self.store.meta_set(LAST_USN_KEY, &next.to_string())?;
        Ok(())
    }

    /// 仅推送（兼容旧路径）。返回上传的变更数。
    pub fn push_via(&self, remote: &mut dyn SyncRemote) -> Result<usize> {
        let plan = self.plan_push()?;
        let count = plan.changes.len() + plan.expunged.len();
        if count == 0 {
            return Ok(0);
        }
        let mut identities = IdentityResolver::new(self.store);
        let mut ack = remote.upload(&plan, &mut identities)?;
        ack.assignments = identities.take_minted();
        eprintln!("[push_via] assignments={:?}", ack.assignments);
        for (kind, uid, guid) in &ack.assignments {
            self.store.identity_set(kind, *uid, guid)?;
        }
        self.commit_push(ack.usn)?;
        self.mark_journal_pushed(&plan)?;
        Ok(count)
    }

    /// 完整同步轮次：**pull → apply → push → commit**。
    /// 先拉取他人变更（此时本地未推送，无回声），再推送本地日志，
    /// 最后以服务器确认 USN 作为新同步点。
    pub fn sync_round(&self, remote: &mut dyn SyncRemote) -> Result<SyncReport> {
        // 1. pull & apply
        let mut report = SyncReport::default();
        let pulls = remote.pull(self.last_synced_usn() as u64)?;
        // 先对齐远端笔记本/标签（guid ↔ 本地 uid），笔记归属翻译才有依据
        for nb in &pulls.notebooks {
            self.store
                .apply_remote_notebook(&nb.guid, &nb.name, nb.stack.as_deref())?;
        }
        for tg in &pulls.tags {
            self.store.apply_remote_tag(&tg.guid, &tg.name)?;
            report.pulled_tags += 1;
        }
        for rn in &pulls.notes {
            // 归属双语义：EDAM 远端给服务器笔记本 guid（identity 反查本地名），
            // mock/LocalEdamRemote 直接给名称（本地已有同名笔记本时原样匹配）。
            let notebook_name = rn.notebook.as_deref().and_then(|nb| {
                let known_as_name = self
                    .store
                    .list_notebooks()
                    .ok()?
                    .iter()
                    .any(|n| n.name == nb);
                if known_as_name {
                    return Some(nb.to_string());
                }
                match self.store.identity_get_local("notebook", nb) {
                    Some(uid) => self
                        .store
                        .list_notebooks()
                        .ok()?
                        .into_iter()
                        .find(|n| n.uid == uid)
                        .map(|n| n.name),
                    None => None, // 远端笔记本尚未同步到本地：归属暂缺（不乱建）
                }
            });
            self.store.apply_remote_note(
                &rn.guid,
                &rn.title,
                &rn.content,
                notebook_name.as_deref(),
                rn.updated as f64,
                rn.deleted,
            )?;
            // 附件落库：所属笔记 guid → 本地 uid，逐资源写 blob + identity。
            // chunk 只带元数据：未落库的附件经 getResource 按需取 body
            // （大附件分页拉取），已落库的附件按 guid 幂等跳过重复下载。
            if let Some(note_uid) = self.store.identity_get_local("note", &rn.guid) {
                for r in &rn.resources {
                    let bytes = match &r.data {
                        Some(d) if !d.is_empty() => d.clone(),
                        _ => {
                            if self.store.identity_get_local("resource", &r.guid).is_some() {
                                continue; // 已落库：不重复下载
                            }
                            match remote.fetch_resource(&r.guid)? {
                                Some(full) => full.data.unwrap_or_default(),
                                None => continue, // 远端不支持按需拉取（mock）：留待下轮
                            }
                        }
                    };
                    if bytes.is_empty() {
                        continue;
                    }
                    self.store.apply_remote_resource(
                        note_uid,
                        &r.guid,
                        &r.file_name,
                        &r.mime,
                        &bytes,
                    )?;
                }
            }
            report.pulled_notes += 1;
        }
        for (guid, kind) in &pulls.expunged {
            self.store.apply_remote_expunge(kind, guid)?;
            report.pulled_expunged += 1;
        }

        // 2. push（远端应用过的变更不会进本地日志，天然无回声）
        let plan = self.plan_push()?;
        report.pushed = plan.changes.len() + plan.expunged.len();
        let ack = if report.pushed > 0 {
            let mut identities = IdentityResolver::new(self.store);
            let mut ack = remote.upload(&plan, &mut identities)?;
            ack.assignments = identities.take_minted();
            for (kind, uid, guid) in &ack.assignments {
                self.store.identity_set(kind, *uid, guid)?;
            }
            ack
        } else {
            UploadAck {
                usn: pulls.usn,
                assignments: vec![],
            }
        };
        self.commit_push(ack.usn)?;
        self.mark_journal_pushed(&plan)?;
        report.server_usn = ack.usn;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biji_store::{ListNotes, NoteDraft, Store};

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    fn server() -> Rc<RefCell<MockEdamServer>> {
        Rc::new(RefCell::new(MockEdamServer::new()))
    }

    fn note_draft(title: &str, content: &str) -> NoteDraft {
        NoteDraft {
            title: title.into(),
            enml: format!("<en-note><p>{content}</p></en-note>"),
            ..Default::default()
        }
    }

    #[test]
    fn push_pull_roundtrip_between_two_clients() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());

        let ea = SyncEngine::new(&s_a);
        let uid = s_a.create_note(&note_draft("来自A", "A 的正文")).unwrap();
        let report = ea.sync_round(&mut ra).unwrap();
        assert_eq!(report.pushed, 1);
        assert_eq!(report.pulled_notes, 0);
        assert_eq!(server.borrow().note_count(), 1);

        // B 同步：应拉到 A 的笔记
        let eb = SyncEngine::new(&s_b);
        let report = eb.sync_round(&mut rb).unwrap();
        assert_eq!(report.pulled_notes, 1);
        assert!(report.pushed == 0, "B 没有本地变更，不应推送");

        let notes = s_b.list_notes(ListNotes::All).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "来自A");
        let full = s_b.get_note(notes[0].uid).unwrap();
        assert!(full.enml.contains("A 的正文"));

        // 身份映射已建立：B 本地 uid 与服务器 guid 对齐
        let guid = s_a.identity_get_guid("note", uid).unwrap();
        assert!(s_b.identity_get_local("note", &guid).is_some());
    }

    #[test]
    fn update_reconciles_to_peer() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());
        let ea = SyncEngine::new(&s_a);
        let eb = SyncEngine::new(&s_b);

        let uid = s_a.create_note(&note_draft("v1", "旧内容")).unwrap();
        ea.sync_round(&mut ra).unwrap();
        eb.sync_round(&mut rb).unwrap();

        s_a.update_note_content(uid, "<en-note><p>v2 新内容</p></en-note>")
            .unwrap();
        ea.sync_round(&mut ra).unwrap();
        eb.sync_round(&mut rb).unwrap();

        let full = s_b
            .get_note(
                s_b.identity_get_local("note", &s_a.identity_get_guid("note", uid).unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert!(full.enml.contains("v2 新内容"));
    }

    #[test]
    fn expunge_ledger_reconciles_and_never_resurrects() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());
        let ea = SyncEngine::new(&s_a);
        let eb = SyncEngine::new(&s_b);

        let uid = s_a.create_note(&note_draft("将死", "x")).unwrap();
        ea.sync_round(&mut ra).unwrap();
        eb.sync_round(&mut rb).unwrap();
        assert_eq!(s_b.list_notes(ListNotes::All).unwrap().len(), 1);

        // A 彻底删除 → 删除账
        s_a.expunge_note(uid).unwrap();
        ea.sync_round(&mut ra).unwrap();
        assert!(server.borrow().expunged_count() >= 1);

        // B 对账：本地行移除
        eb.sync_round(&mut rb).unwrap();
        assert!(s_b.list_notes(ListNotes::All).unwrap().is_empty());

        // 新客户端 C 首次同步：绝不应复活已删除笔记
        let s_c = store();
        let mut rc = LocalEdamRemote::new(server.clone());
        let report = SyncEngine::new(&s_c).sync_round(&mut rc).unwrap();
        assert_eq!(report.pulled_notes, 0);
        assert!(report.pulled_expunged >= 1);
        assert!(s_c.list_notes(ListNotes::All).unwrap().is_empty());
    }

    #[test]
    fn bidirectional_changes_both_ways() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());
        let ea = SyncEngine::new(&s_a);
        let eb = SyncEngine::new(&s_b);

        let a_note = s_a.create_note(&note_draft("A 的", "a")).unwrap();
        let b_note = s_b.create_note(&note_draft("B 的", "b")).unwrap();
        ea.sync_round(&mut ra).unwrap();
        eb.sync_round(&mut rb).unwrap();
        ea.sync_round(&mut ra).unwrap(); // A 第二轮拉到 B 的

        assert_eq!(s_a.list_notes(ListNotes::All).unwrap().len(), 2);
        assert_eq!(s_b.list_notes(ListNotes::All).unwrap().len(), 2);
        let _ = (a_note, b_note);
    }

    #[test]
    fn idempotent_no_echo_loop() {
        let server = server();
        let s_a = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let ea = SyncEngine::new(&s_a);
        s_a.create_note(&note_draft("x", "x")).unwrap();
        ea.sync_round(&mut ra).unwrap();
        for _ in 0..3 {
            let report = ea.sync_round(&mut ra).unwrap();
            assert_eq!(report.pushed, 0);
            assert_eq!(report.pulled_notes, 0, "不应把自己的变更拉回来");
        }
    }

    #[test]
    fn sync_point_never_regresses_on_empty_round() {
        let server = server();
        let s = store();
        let mut remote = LocalEdamRemote::new(server.clone());
        let engine = SyncEngine::new(&s);
        s.create_note(&note_draft("x", "x")).unwrap();
        engine.sync_round(&mut remote).unwrap();
        let after_first = engine.last_synced_usn();
        assert!(after_first > 0);

        // 空轮次（新空服务器，回执 USN=0）不得回退同步点
        let mut fresh = LocalEdamRemote::new(Rc::new(RefCell::new(MockEdamServer::new())));
        engine.sync_round(&mut fresh).unwrap();
        assert_eq!(engine.last_synced_usn(), after_first);
    }

    #[test]
    fn notebook_and_tag_sync_as_names() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());
        let ea = SyncEngine::new(&s_a);
        let eb = SyncEngine::new(&s_b);

        s_a.create_notebook("项目", None).unwrap();
        s_a.create_tag("紧急", None).unwrap();
        ea.sync_round(&mut ra).unwrap();
        eb.sync_round(&mut rb).unwrap();

        // 服务器侧已记账（notebook/tag 名称表）
        assert!(!server.borrow().notebooks.is_empty());
        assert!(!server.borrow().tags.is_empty());
        let _ = eb;
    }

    /// 回归：本地 journal usn 与服务器域同步点在边界 USN 碰撞时，
    /// 删除账不得被同步点越过漏推（推送游标与服务器游标分离）。
    #[test]
    fn journal_mark_survives_server_usn_boundary() {
        let server = server();
        let s = store();
        let mut ra = LocalEdamRemote::new(server.clone());

        // 一条笔记上云（journal 水位推进到 1）
        let note_uid = s.create_note(&note_draft("边界", "正文")).unwrap();
        let tag_uid = s.create_tag("边界标签", None).unwrap();
        let _ = note_uid;
        SyncEngine::new(&s).sync_round(&mut ra).unwrap();

        // 外力把服务器域同步点推到 6（对位真实服务器多对象 USN 领先场景）
        s.meta_set("sync.last_usn", "6").unwrap();
        // 本地计数器推进到 5，使下一条 journal（删除账）恰好落在 usn=6
        for i in 0..3 {
            s.create_tag(&format!("计数{i}"), None).unwrap();
        }
        s.delete_tag(tag_uid).unwrap();
        let mark_before = s.meta_get("sync.journal_mark");
        assert_eq!(
            mark_before.as_deref(),
            Some("2"),
            "首轮后 journal 水位应为 2"
        );

        // 修复前：plan_push 以服务器同步点 6 为界，usn=6 的删除账被漏推
        let report = SyncEngine::new(&s).sync_round(&mut ra).unwrap();
        assert!(report.pushed >= 1, "边界 USN 删除账未被推送");

        // B 端对账后标签消失
        let s_b = store();
        let mut rb2 = LocalEdamRemote::new(server.clone());
        SyncEngine::new(&s_b).sync_round(&mut rb2).unwrap();
        assert!(
            !s_b.list_tags()
                .unwrap()
                .iter()
                .any(|t| t.name == "边界标签"),
            "删除账未跨端对账"
        );
    }

    /// 标签与笔记本跨端传播 + 删除账对账：A 建标签/笔记本 → B 拉到；
    /// A 删除 → B 经删除账移除，且绝不复活。
    #[test]
    fn tags_and_notebook_tag_expunge_propagate() {
        let server = server();
        let s_a = store();
        let s_b = store();
        let mut ra = LocalEdamRemote::new(server.clone());
        let mut rb = LocalEdamRemote::new(server.clone());

        // A：建标签 + 笔记本 → 上云
        let tag_uid = s_a.create_tag("重要", None).unwrap();
        let nb_uid = s_a.create_notebook("项目", None).unwrap();
        SyncEngine::new(&s_a).sync_round(&mut ra).unwrap();

        // B：拉取 → 标签与笔记本均已落库且 identity 登记
        let rb_report = SyncEngine::new(&s_b).sync_round(&mut rb).unwrap();
        assert!(rb_report.pulled_tags >= 1, "标签未跨端传播");
        assert!(s_b.list_tags().unwrap().iter().any(|t| t.name == "重要"));
        assert!(s_b
            .list_notebooks()
            .unwrap()
            .iter()
            .any(|nb| nb.name == "项目"));
        let tag_guid = s_a.identity_get_guid("tag", tag_uid).unwrap();
        let nb_guid = s_a.identity_get_guid("notebook", nb_uid).unwrap();
        assert!(s_b.identity_get_local("tag", &tag_guid).is_some());
        assert!(s_b.identity_get_local("notebook", &nb_guid).is_some());

        // A：删标签 + 删笔记本（删除账）→ 上云
        s_a.delete_tag(tag_uid).unwrap();
        s_a.delete_notebook(nb_uid).unwrap();
        SyncEngine::new(&s_a).sync_round(&mut ra).unwrap();

        // B：再同步 → 两对象被删除账移除
        SyncEngine::new(&s_b).sync_round(&mut rb).unwrap();
        assert!(
            !s_b.list_tags().unwrap().iter().any(|t| t.name == "重要"),
            "标签删除未跨端对账"
        );
        assert!(
            !s_b.list_notebooks()
                .unwrap()
                .iter()
                .any(|nb| nb.name == "项目"),
            "笔记本删除未跨端对账"
        );
    }

    #[test]
    fn plan_carries_content_snapshots() {
        let s = store();
        s.create_note(&note_draft("带快照", "正文快照")).unwrap();
        let plan = SyncEngine::new(&s).plan_push().unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].title.as_deref(), Some("带快照"));
        assert!(plan.changes[0]
            .content
            .as_deref()
            .unwrap()
            .contains("正文快照"));
    }

    /// 未认证句柄（authenticate 失败/服务端未启动时）保持诚实：
    /// 同步调用报 Unsupported 并给出自有后端联调指引，不 panic 不误同步。
    #[test]
    fn server_remote_unauthenticated_is_honest() {
        let mut remote = ServerRemote::with_token(DEFAULT_SERVER_URL, None);
        assert_eq!(remote.name(), "server-go");
        assert_eq!(remote.service_host(), DEFAULT_SERVER_URL);
        let s = store();
        let mut ids = IdentityResolver::new(&s);
        assert!(matches!(
            remote.upload(&PushPlan::default(), &mut ids),
            Err(SyncError::Unsupported(_))
        ));
        assert!(matches!(remote.pull(0), Err(SyncError::Unsupported(_))));
    }
}
