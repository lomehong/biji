//! EDAM/Thrift 传输——**自研后端 biji-server-go**（本仓库 `biji-server-go/`）。
//!
//! EDAM 风格 Thrift 二进制协议 over HTTP 的最小子集实现
//! （连接目标是自研后端 biji-server-go），覆盖同步引擎
//! 所需的调用：getNoteStoreUrl / getSyncState / getFilteredSyncChunk /
//! createNote / updateNote / expungeNote / createNotebook / updateNotebook /
//! expungeNotebook / createTag / updateTag / expungeTag。
//!
//! 凭证经 UserStore.authenticate（BIJI_SERVER_USER/PASS，默认 biji/biji）
//! 换取认证 token，只存内存，永不写库、永不写日志。

use ureq::{Agent, AgentBuilder};

/// HTTP 传输闭包类型：url + thrift 请求体 → thrift 响应体。
pub type TransportFn = dyn Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String>;

// ---------- Thrift 二进制协议编解码 ----------

pub const T_STOP: u8 = 0;
pub const T_BOOL: u8 = 2;
pub const T_I16: u8 = 6;
const T_I32: u8 = 8;
pub const T_I64: u8 = 10;
pub const T_STRING: u8 = 11;
pub const T_STRUCT: u8 = 12;
const T_MAP: u8 = 13;
const T_SET: u8 = 14;
pub const T_LIST: u8 = 15;
const T_UTF7: u8 = 16; // 旧称，等价 STRING

const MSG_CALL: u8 = 1;
const VERSION_1: i32 = i32::from_be_bytes([0x80, 0x01, 0x00, 0x00]); // 严格版本字（含符号位）

#[derive(Debug, Clone, PartialEq)]
pub enum TVal {
    Bool(bool),
    I32(i32),
    I64(i64),
    /// 文本字段。解析层 STRING 与 BINARY 同类型码，统一读原始字节后
    /// 由消费方决定语义：附件走 [`TVal::Bytes`]，文本走本变体。
    Str(String),
    /// 二进制字段（附件/资源 body），保真不转码。
    Bytes(Vec<u8>),
    List(Vec<TVal>),
    Struct(Vec<(i16, TVal)>),
}

impl TVal {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            TVal::I32(v) => Some(*v as i64),
            TVal::I64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            TVal::Str(s) => Some(s),
            // 文本消费点对 Bytes 无感（正常 wire 上文本不会解析成 Bytes；
            // 此兜底保证混用时字符串读取不 panic）
            TVal::Bytes(b) => Some(std::str::from_utf8(b).ok()?),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            TVal::Bytes(b) => Some(b),
            TVal::Str(s) => Some(s.as_bytes()),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[TVal]> {
        match self {
            TVal::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn field(&self, id: i16) -> Option<&TVal> {
        match self {
            TVal::Struct(fields) => fields.iter().find(|(fid, _)| *fid == id).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Thrift 二进制协议写入器。
pub struct TWriter {
    pub buf: Vec<u8>,
}

impl Default for TWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl TWriter {
    pub fn new() -> TWriter {
        TWriter { buf: Vec::new() }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn string(&mut self, s: &str) {
        self.i32(s.len() as i32);
        self.buf.extend_from_slice(s.as_bytes());
    }

    /// 消息头：严格二进制协议 = 版本字(i32) + 方法名 + 序号(i32)。
    pub fn message_begin(&mut self, name: &str, msg_type: u8, seqid: i32) {
        self.i32(VERSION_1 | msg_type as i32);
        self.string(name);
        self.i32(seqid);
    }

    pub fn field_begin(&mut self, id: i16, ttype: u8) {
        self.u8(ttype);
        self.i16(id);
    }

    pub fn field_stop(&mut self) {
        self.u8(T_STOP);
    }

    pub fn list_begin(&mut self, elem_type: u8, size: usize) {
        self.u8(elem_type);
        self.i32(size as i32);
    }

    pub fn field_bool(&mut self, id: i16, v: bool) {
        self.field_begin(id, T_BOOL);
        self.u8(if v { 1 } else { 0 });
    }

    pub fn field_i32(&mut self, id: i16, v: i32) {
        self.field_begin(id, T_I32);
        self.i32(v);
    }

    pub fn field_i64(&mut self, id: i16, v: i64) {
        self.field_begin(id, T_I64);
        self.i64(v);
    }

    pub fn field_string(&mut self, id: i16, v: &str) {
        self.field_begin(id, T_STRING);
        self.string(v);
    }

    /// 二进制字段（附件 body），与 STRING 同类型码。
    pub fn field_bytes(&mut self, id: i16, v: &[u8]) {
        self.field_begin(id, T_STRING);
        self.i32(v.len() as i32);
        self.buf.extend_from_slice(v);
    }

    pub fn field_struct_begin(&mut self, id: i16) {
        self.field_begin(id, T_STRUCT);
    }

    pub fn field_list_begin(&mut self, id: i16, elem_type: u8, size: usize) {
        self.field_begin(id, T_LIST);
        self.list_begin(elem_type, size);
    }

    /// 完整 RPC 消息：message_begin + 参数 struct + stop。
    /// 方法名直用裸名（自有后端 biji-server-go 按裸名路由）。
    pub fn call(&mut self, method: &str, seqid: i32, args: impl FnOnce(&mut TWriter)) {
        self.message_begin(method, MSG_CALL, seqid);
        self.field_begin(1, T_STRUCT); // args
        args(self);
        self.field_stop(); // args struct
        self.u8(T_STOP); // message
    }
}

// ---------- 读取器 ----------

fn u8_at(buf: &[u8], pos: &mut usize) -> Option<u8> {
    let v = *buf.get(*pos)?;
    *pos += 1;
    Some(v)
}

fn i16_at(buf: &[u8], pos: &mut usize) -> Option<i16> {
    let b = buf.get(*pos..*pos + 2)?;
    *pos += 2;
    Some(i16::from_be_bytes(b.try_into().ok()?))
}

fn i32_at(buf: &[u8], pos: &mut usize) -> Option<i32> {
    let b = buf.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(i32::from_be_bytes(b.try_into().ok()?))
}

fn i64_at(buf: &[u8], pos: &mut usize) -> Option<i64> {
    let b = buf.get(*pos..*pos + 8)?;
    *pos += 8;
    Some(i64::from_be_bytes(b.try_into().ok()?))
}

/// 读长度前缀的原始字节（STRING/BINARY 同类型码，不转码）。
fn bytes_at(buf: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let len = i32_at(buf, pos)? as usize;
    let b = buf.get(*pos..*pos + len)?.to_vec();
    *pos += len;
    Some(b)
}

fn string_at(buf: &[u8], pos: &mut usize) -> Option<String> {
    let len = i32_at(buf, pos)? as usize;
    let b = buf.get(*pos..*pos + len)?;
    *pos += len;
    Some(String::from_utf8_lossy(b).into_owned())
}

/// 按类型跳过一个值（未知字段的旁路）。
fn skip(buf: &[u8], pos: &mut usize, ttype: u8) -> Option<()> {
    match ttype {
        T_BOOL | T_BYTE_LIKE => {
            u8_at(buf, pos)?;
            Some(())
        }
        T_I32 => {
            i32_at(buf, pos)?;
            Some(())
        }
        T_I64 => {
            i64_at(buf, pos)?;
            Some(())
        }
        T_STRING | T_UTF7 => {
            string_at(buf, pos)?;
            Some(())
        }
        T_STRUCT => read_struct(buf, pos).map(|_| ()),
        T_LIST | T_SET => {
            let etype = u8_at(buf, pos)?;
            let size = i32_at(buf, pos)? as usize;
            for _ in 0..size {
                skip(buf, pos, etype)?;
            }
            Some(())
        }
        T_MAP => {
            let ktype = u8_at(buf, pos)?;
            let vtype = u8_at(buf, pos)?;
            let size = i32_at(buf, pos)? as usize;
            for _ in 0..size {
                skip(buf, pos, ktype)?;
                skip(buf, pos, vtype)?;
            }
            Some(())
        }
        _ => None,
    }
}

const T_BYTE_LIKE: u8 = 3;

/// 读取一个 struct 的全部字段（直到 STOP）。
pub fn read_struct(buf: &[u8], pos: &mut usize) -> Option<Vec<(i16, TVal)>> {
    let mut fields = Vec::new();
    loop {
        let ttype = u8_at(buf, pos)?;
        if ttype == T_STOP {
            return Some(fields);
        }
        let id = i16_at(buf, pos)?;
        let val = read_value(buf, pos, ttype)?;
        fields.push((id, val));
    }
}

fn read_value(buf: &[u8], pos: &mut usize, ttype: u8) -> Option<TVal> {
    match ttype {
        T_BOOL => Some(TVal::Bool(u8_at(buf, pos)? == 1)),
        T_I32 => Some(TVal::I32(i32_at(buf, pos)?)),
        T_I64 => Some(TVal::I64(i64_at(buf, pos)?)),
        T_STRING | T_UTF7 => Some(TVal::Bytes(bytes_at(buf, pos)?)),
        T_STRUCT => Some(TVal::Struct(read_struct(buf, pos)?)),
        T_LIST | T_SET => {
            let etype = u8_at(buf, pos)?;
            let size = i32_at(buf, pos)? as usize;
            let mut items = Vec::with_capacity(size.min(1024));
            for _ in 0..size {
                items.push(read_value(buf, pos, etype)?);
            }
            Some(TVal::List(items))
        }
        _ => {
            skip(buf, pos, ttype)?;
            Some(TVal::Str(String::new()))
        }
    }
}

// ---------- EDAM 字段号（公开 IDL 规范定义的同步子集） ----------

mod note_fields {
    pub const GUID: i16 = 1;
    pub const TITLE: i16 = 2;
    pub const CONTENT: i16 = 3;
    pub const UPDATED: i16 = 7;
    pub const ACTIVE: i16 = 9;
    pub const UPDATE_SEQ: i16 = 10;
    pub const NOTEBOOK: i16 = 11; // Note.notebookGuid
}

mod sync_state_fields {
    pub const UPDATE_COUNT: i16 = 3;
}

mod sync_chunk_fields {
    pub const CHUNK_HIGH_USN: i16 = 2;
    pub const UPDATE_COUNT: i16 = 3;
    pub const NOTES: i16 = 4;
    pub const NOTEBOOKS: i16 = 5; // SyncChunk.notebooks
    pub const TAGS: i16 = 6; // SyncChunk.tags（镜像本仓库 notebooks=f5 的布局约定）
    pub const EXPUNGED_NOTES: i16 = 11;
    pub const EXPUNGED_NOTEBOOKS: i16 = 12;
    pub const EXPUNGED_TAGS: i16 = 13;
}

mod sync_chunk_filter_fields {
    pub const INCLUDE_NOTES: i16 = 2;
    pub const INCLUDE_EXPUNGED: i16 = 16;
}

mod update_result_fields {
    pub const UPDATE_SEQ: i16 = 0; // updateNote 返回裸 i32（result 字段 0）
    pub const NOTE: i16 = 2;
}

// ---------- 客户端 ----------

/// HTTP 传输接口：url + 请求体 → 响应体。真实实现走 ureq（HTTPS），
/// 测试注入内存实现以覆盖「请求编码 → 响应解析 → SyncRemote 语义」全链路。
pub type Transport<'a> = dyn Fn(&str, &[u8]) -> Result<Vec<u8>, String> + 'a;

/// EDAM 客户端：HTTPS + Thrift 二进制（Clone 仅为借用句柄；Agent 内部 Arc 共享）。
#[derive(Clone)]
pub struct EdamClient {
    pub host: String,
    pub token: String,
    agent: Agent,
    /// 测试注入的内存传输（Some 时绕过网络）；Rc 共享以支持 Clone 句柄。
    transport: Option<std::rc::Rc<TransportFn>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSyncChunk {
    pub chunk_high_usn: u64,
    pub update_count: u64,
    /// (guid, title, content, active, updated)
    pub notes: Vec<RemoteEdamNote>,
    /// 远端笔记本（guid + 名称）。
    pub notebooks: Vec<RemoteEdamNotebook>,
    /// 远端标签（guid + 名称）。
    pub tags: Vec<RemoteEdamTag>,
    pub expunged_note_guids: Vec<String>,
    pub expunged_notebook_guids: Vec<String>,
    pub expunged_tag_guids: Vec<String>,
}

/// 远端笔记本（wire 层；stack 为分组，对位 EDAM Notebook.stack f7）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEdamNotebook {
    pub guid: String,
    pub name: String,
    pub stack: String,
}

/// 远端标签（wire 层）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEdamTag {
    pub guid: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEdamNote {
    pub guid: String,
    pub title: String,
    pub content: String,
    pub active: bool,
    pub updated: i64,
    pub notebook: Option<String>,
    /// 附件资源（chunk 只带元数据，body 经 getResource 按需拉取）。
    pub resources: Vec<RemoteEdamResource>,
}

/// 远端附件资源（wire 层）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEdamResource {
    pub guid: String,
    pub mime: String,
    pub file_name: String,
    /// `None` = chunk 元数据态（body 未随行，需 getResource 拉取）；
    /// `Some(bytes)` = 全量态（getResource 回执或兼容随行携带的旧布局）。
    pub data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteNoteResult {
    pub guid: String,
    pub usn: u64,
}

impl EdamClient {
    pub fn new(host: &str, token: &str) -> EdamClient {
        // 禁用连接池：EDAM 每请求一条新连接，避免池化连接与对端
        // 服务器 accept 时序错位（本地联调实测踩坑）。
        let agent = AgentBuilder::new()
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .build();
        EdamClient {
            host: host.to_string(),
            token: token.to_string(),
            agent,
            transport: None,
        }
    }

    /// 内存传输注入（测试）：url + thrift 请求体 → thrift 响应体。
    pub fn with_transport(
        host: &str,
        token: &str,
        transport: std::rc::Rc<TransportFn>,
    ) -> EdamClient {
        EdamClient {
            host: host.to_string(),
            token: token.to_string(),
            agent: Agent::new(),
            transport: Some(transport),
        }
    }

    fn user_store_url(&self) -> String {
        // 以 http(s):// 开头的 host 视为完整基址（本地联调/私有部署钩子）；
        // 其余按域名拼 https。
        if self.host.starts_with("http://") || self.host.starts_with("https://") {
            format!("{}/edam/user", self.host.trim_end_matches('/'))
        } else {
            format!("https://{}/edam/user", self.host)
        }
    }

    /// 单次 RPC：编码消息 → HTTPS POST → 解析 REPLY。
    /// 返回结果 struct（字段 0）；服务端异常（字段 1+）映射为 Edam 错误。
    fn rpc(
        &self,
        url: &str,
        method: &str,
        seqid: i32,
        args: impl FnOnce(&mut TWriter),
    ) -> Result<Vec<(i16, TVal)>, String> {
        let mut w = TWriter::new();
        w.call(method, seqid, args);
        // EDAM 传输格式开关：默认 TFramed（4B 大端帧长 + 消息）；
        // BIJI_SERVER_UNFRAMED=1 时发裸消息（协议调试回退开关）。
        let unframed = std::env::var("BIJI_SERVER_UNFRAMED").ok().as_deref() == Some("1");
        let mut payload = Vec::with_capacity(w.buf.len() + 4);
        if !unframed {
            payload.extend_from_slice(&(w.buf.len() as i32).to_be_bytes());
        }
        payload.extend_from_slice(&w.buf);
        let body_b = match &self.transport {
            Some(t) => t(url, &payload)?,
            None => {
                let resp = self
                    .agent
                    .post(url)
                    .set("Content-Type", "application/x-thrift")
                    .set("User-Agent", "biji/0.1 (EDAM Thrift client)")
                    .send_bytes(&w.buf)
                    .map_err(|e| match e {
                        ureq::Error::Status(code, resp) => {
                            format!("HTTP {code}: {}", resp.status_text())
                        }
                        other => format!("{other}"),
                    })?;
                let mut body_b = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body_b)
                    .map_err(|e| format!("读取响应失败: {e}"))?;
                // 响应自适应：0x80 开头 = 裸 Thrift 消息；否则按 4B 帧长截取
                if body_b.first() == Some(&0x80) {
                } else if body_b.len() >= 4 {
                    let flen = i32::from_be_bytes(body_b[..4].try_into().unwrap()) as usize;

                    if flen > 0 && flen <= body_b.len() - 4 {
                        body_b = body_b[4..4 + flen].to_vec();
                    }
                }
                body_b
            }
        };
        let mut pos = 0usize;
        // 消息头
        let version = i32_at(&body_b, &mut pos).ok_or("响应过短")?;
        if version & 0xFFFF_0000u32 as i32 != VERSION_1 & 0xFFFF_0000u32 as i32 {
            return Err(format!("Thrift 版本字不匹配: {version:#x}"));
        }
        let _name = string_at(&body_b, &mut pos).ok_or("响应缺方法名")?;
        let _seq = i32_at(&body_b, &mut pos).ok_or("响应缺序号")?;
        let msg_type = (version & 0xFF) as u8; // 低字节即消息类型（REPLY=2 / EXCEPTION=3）
        let fields = read_struct(&body_b, &mut pos).ok_or("响应 struct 解析失败")?;
        if msg_type == 3 {
            // EXCEPTION 消息：字段 1 = string message
            let msg = fields
                .iter()
                .find(|(id, _)| *id == 1)
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("未知异常");
            return Err(format!("TApplicationException: {msg}"));
        }
        // 服务端业务异常（字段 ≥1）优先于成功值（字段 0）
        if let Some((_, v)) = fields.iter().find(|(id, _)| *id >= 1) {
            let msg = v
                .field(1)
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .or_else(|| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{v:?}"));
            return Err(format!("EDAM 异常: {msg}"));
        }
        Ok(fields
            .into_iter()
            .find(|(id, _)| *id == 0)
            .map(|(_, v)| match v {
                TVal::Struct(f) => f,
                other => vec![(0, other)],
            })
            .unwrap_or_default())
    }

    /// UserStore.checkVersion（**免认证**握手）：对真实服务端验证
    /// Thrift 消息封装/解析全链路。返回服务端声明的兼容性。
    pub fn check_version(&self, client_name: &str) -> Result<bool, String> {
        let fields = self.rpc(&self.user_store_url(), "checkVersion", 1, |w| {
            w.field_string(1, client_name);
            w.field_begin(2, T_I16); // EDAM_VERSION_MAJOR（IDL 为 i16）
            w.i16(1);
            w.field_begin(3, T_I16); // EDAM_VERSION_MINOR
            w.i16(28);
            w.field_stop();
        })?;
        Ok(fields
            .iter()
            .find(|(id, _)| *id == 0)
            .map(|(_, v)| matches!(v, TVal::Bool(true)))
            .unwrap_or(false))
    }

    /// UserStore.getNoteStoreUrl → 分片专属的 NoteStore 地址。
    pub fn get_note_store_url(&self) -> Result<String, String> {
        let fields = self.rpc(&self.user_store_url(), "getNoteStoreUrl", 1, |w| {
            w.field_string(1, &self.token);
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == 0)
            .and_then(|(_, v)| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "getNoteStoreUrl 响应缺成功值".into())
    }

    /// UserStore.authenticate（账密认证，EDAM 经典路径；中国区老版服务端可用）。
    /// 返回 AuthenticationResult.authenticationToken（字段 2）。
    pub fn authenticate(&self, user: &str, pass: &str) -> Result<String, String> {
        let fields = self.rpc(&self.user_store_url(), "authenticate", 1, |w| {
            w.field_string(1, user);
            w.field_string(2, pass);
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == 2) // AuthenticationResult.authenticationToken
            .and_then(|(_, v)| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "authenticate 响应缺 authenticationToken".into())
    }

    /// NoteStore.getSyncState → 服务器 updateCount。
    pub fn get_sync_state(&self, note_url: &str) -> Result<u64, String> {
        let fields = self.rpc(note_url, "getSyncState", 1, |w| {
            w.field_string(1, &self.token);
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == sync_state_fields::UPDATE_COUNT)
            .and_then(|(_, v)| v.as_i64())
            .map(|v| v as u64)
            .ok_or_else(|| "getSyncState 响应缺 updateCount".into())
    }

    /// NoteStore.getFilteredSyncChunk（含笔记与删除账）。
    pub fn get_sync_chunk(
        &self,
        note_url: &str,
        after_usn: i32,
        max_entries: i32,
    ) -> Result<RemoteSyncChunk, String> {
        let fields = self.rpc(note_url, "getFilteredSyncChunk", 1, |w| {
            w.field_string(1, &self.token);
            w.field_i32(2, after_usn);
            w.field_i32(3, max_entries);
            w.field_struct_begin(4); // SyncChunkFilter
            {
                w.field_bool(sync_chunk_filter_fields::INCLUDE_NOTES, true);
                w.field_bool(sync_chunk_filter_fields::INCLUDE_EXPUNGED, true);
                w.field_stop();
            }
            w.field_stop();
        })?;
        Ok(parse_sync_chunk(&fields))
    }

    /// NoteStore.getResource → 附件全量（guid + body + mime + fileName）。
    /// chunk 只带资源元数据，大附件 body 经此按需拉取（EDAM 分页语义）；
    /// 参数序对齐 EDAM IDL：f2 guid / f3 withBinary / f4 withData。
    pub fn get_resource(&self, note_url: &str, guid: &str) -> Result<RemoteEdamResource, String> {
        let fields = self.rpc(note_url, "getResource", 1, |w| {
            w.field_string(1, &self.token);
            w.field_string(2, guid);
            w.field_bool(3, true); // withBinary
            w.field_bool(4, true); // withData
            w.field_stop();
        })?;
        parse_resource_struct(&TVal::Struct(fields))
            .ok_or_else(|| "getResource 响应缺 guid".to_string())
    }
    /// NoteStore.createNote → (guid, usn)。
    pub fn create_note(
        &self,
        note_url: &str,
        title: &str,
        content: &str,
        notebook_guid: Option<&str>,
        resources: &[ResourceUploadItem],
    ) -> Result<RemoteNoteResult, String> {
        let fields = self.rpc(note_url, "createNote", 1, |w| {
            w.field_string(1, &self.token);
            w.field_struct_begin(2);
            {
                w.field_string(note_fields::TITLE, title);
                w.field_string(note_fields::CONTENT, content);
                if let Some(nb) = notebook_guid {
                    w.field_string(note_fields::NOTEBOOK, nb);
                }
                if !resources.is_empty() {
                    write_resources(w, resources);
                }
                w.field_bool(note_fields::ACTIVE, true);
                w.field_stop();
            }
            w.field_stop();
        })?;
        parse_note_result(&fields)
    }

    /// NoteStore.updateNote → (guid, usn)。
    pub fn update_note(
        &self,
        note_url: &str,
        guid: &str,
        title: &str,
        content: &str,
        notebook_guid: Option<&str>,
        resources: &[ResourceUploadItem],
    ) -> Result<RemoteNoteResult, String> {
        let fields = self.rpc(note_url, "updateNote", 1, |w| {
            w.field_string(1, &self.token);
            w.field_struct_begin(2);
            {
                w.field_string(note_fields::GUID, guid);
                w.field_string(note_fields::TITLE, title);
                w.field_string(note_fields::CONTENT, content);
                if let Some(nb) = notebook_guid {
                    w.field_string(note_fields::NOTEBOOK, nb);
                }
                if !resources.is_empty() {
                    write_resources(w, resources);
                }
                w.field_bool(note_fields::ACTIVE, true);
                w.field_stop();
            }
            w.field_stop();
        })?;
        // UpdateResult：usn=1；若带 note=2 则取其 guid
        let usn = fields
            .iter()
            .find(|(id, _)| *id == update_result_fields::UPDATE_SEQ)
            .and_then(|(_, v)| v.as_i64())
            .unwrap_or(0);
        let guid_out = fields
            .iter()
            .find(|(id, _)| *id == update_result_fields::NOTE)
            .and_then(|(_, n)| n.field(note_fields::GUID))
            .and_then(|g| g.as_str())
            .unwrap_or(guid)
            .to_string();
        Ok(RemoteNoteResult {
            guid: guid_out,
            usn: usn as u64,
        })
    }

    /// NoteStore.expungeNote → usn。
    pub fn expunge_note(&self, note_url: &str, guid: &str) -> Result<u64, String> {
        self.expunge_named(note_url, "expungeNote", guid)
    }

    pub fn expunge_notebook(&self, note_url: &str, guid: &str) -> Result<u64, String> {
        self.expunge_named(note_url, "expungeNotebook", guid)
    }

    pub fn expunge_tag(&self, note_url: &str, guid: &str) -> Result<u64, String> {
        self.expunge_named(note_url, "expungeTag", guid)
    }

    fn expunge_named(&self, note_url: &str, method: &str, guid: &str) -> Result<u64, String> {
        let fields = self.rpc(note_url, method, 1, |w| {
            w.field_string(1, &self.token);
            w.field_string(2, guid);
            w.field_stop();
        })?;
        Ok(fields
            .iter()
            .find(|(id, _)| *id == 0)
            .and_then(|(_, v)| v.as_i64())
            .unwrap_or(0) as u64)
    }

    /// createNotebook / createTag → guid；updateNotebook / updateTag → usn。
    /// NoteStore.createNotebook → 新笔记本 guid；stack Some 时携带分组（f7）。
    pub fn create_notebook(
        &self,
        note_url: &str,
        name: &str,
        stack: Option<&str>,
    ) -> Result<String, String> {
        let fields = self.rpc(note_url, "createNotebook", 1, |w| {
            w.field_string(1, &self.token);
            w.field_struct_begin(2);
            {
                w.field_string(2, name);
                if let Some(s) = stack {
                    w.field_string(7, s);
                }
                w.field_stop();
            }
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == 1)
            .and_then(|(_, g)| g.as_str())
            .map(|g| g.to_string())
            .ok_or_else(|| "createNotebook 响应缺 guid".into())
    }

    /// NoteStore.updateNotebook → usn；stack Some 时更新分组（空串清除），
    /// None 不携带 f7（服务端保持原分组）。
    pub fn update_notebook(
        &self,
        note_url: &str,
        guid: &str,
        name: &str,
        stack: Option<&str>,
    ) -> Result<u64, String> {
        let fields = self.rpc(note_url, "updateNotebook", 1, |w| {
            w.field_string(1, &self.token);
            w.field_string(2, guid);
            w.field_struct_begin(3);
            {
                w.field_string(1, guid);
                w.field_string(2, name);
                if let Some(s) = stack {
                    w.field_string(7, s);
                }
                w.field_stop();
            }
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == 0)
            .and_then(|(_, v)| v.as_i64())
            .map(|v| v as u64)
            .ok_or_else(|| "updateNotebook 响应缺 usn".into())
    }

    /// NoteStore.putResource（本仓库协议扩展）：单附件上传，不动笔记其余
    /// 附件。客户端以既有资源 guid 上传（首推经整笔记替换，guid 由 pull 建立）。
    /// 参数：f2 guid / f3 noteGuid / f4 Data{f1 body} / f5 mime / f6 fileName
    /// → 裸 f0 i64 usn。
    pub fn put_resource(
        &self,
        note_url: &str,
        guid: &str,
        note_guid: &str,
        bytes: &[u8],
        mime: &str,
        file_name: &str,
    ) -> Result<u64, String> {
        let fields = self.rpc(note_url, "putResource", 1, |w| {
            w.field_string(1, &self.token);
            w.field_string(2, guid);
            w.field_string(3, note_guid);
            w.field_struct_begin(4); // Data{f1 body, f3 size}
            w.field_bytes(1, bytes);
            w.field_i32(3, bytes.len() as i32);
            w.field_stop();
            w.field_string(5, mime);
            w.field_string(6, file_name);
            w.field_stop();
        })?;
        fields
            .iter()
            .find(|(id, _)| *id == 0)
            .and_then(|(_, v)| v.as_i64())
            .map(|v| v as u64)
            .ok_or_else(|| "putResource 响应缺 usn".into())
    }

    pub fn create_named(&self, note_url: &str, method: &str, name: &str) -> Result<String, String> {
        let fields = self.rpc(note_url, method, 1, |w| {
            w.field_string(1, &self.token);
            w.field_struct_begin(2);
            {
                w.field_string(2, name); // Notebook.name / Tag.name 均为字段 2
                w.field_stop();
            }
            w.field_stop();
        })?;
        // 剥壳后即 Notebook/Tag 字段：guid=1
        fields
            .iter()
            .find(|(id, _)| *id == note_fields::GUID)
            .and_then(|(_, g)| g.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("{method} 响应缺 guid"))
    }

    pub fn update_named(
        &self,
        note_url: &str,
        method: &str,
        guid: &str,
        name: &str,
    ) -> Result<u64, String> {
        let fields = self.rpc(note_url, method, 1, |w| {
            w.field_string(1, &self.token);
            w.field_string(2, guid);
            w.field_struct_begin(3);
            {
                w.field_string(1, guid);
                w.field_string(2, name);
                w.field_stop();
            }
            w.field_stop();
        })?;
        Ok(fields
            .iter()
            .find(|(id, _)| *id == 0)
            .and_then(|(_, v)| v.as_i64())
            .unwrap_or(0) as u64)
    }
}

/// 上行附件资源（createNote/updateNote 的 f13 元素）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceUploadItem {
    /// 本地资源 uid（putResource 快路径按变更资源定位条目）。
    pub local_uid: i64,
    pub file_name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// 按 EDAM Resource 布局写 f13 resources list（无 guid：新建由服务端分配）。
fn write_resources(w: &mut TWriter, items: &[ResourceUploadItem]) {
    w.field_list_begin(13, T_STRUCT, items.len());
    for r in items {
        w.field_struct_begin(3); // Data{ body=1, size=3, mime=4 }
        w.field_bytes(1, &r.bytes);
        w.field_i32(3, r.bytes.len() as i32);
        w.field_string(4, &r.mime);
        w.field_stop();
        w.field_string(4, &r.mime);
        w.field_struct_begin(14); // attributes{ fileName=1 }
        w.field_string(1, &r.file_name);
        w.field_stop();
        w.field_stop();
    }
}

/// 从 getFilteredSyncChunk 结果 struct 解析同步块（纯函数，单测直接覆盖）。
/// 解析 Note STRUCT 内 f13 resources list（Resource: f1 guid, f3 Data{f1 body},
/// f4 mime, f14 attributes{f1 fileName}；与 EDAM IDL（公开规范）字段号一致）。
fn parse_note_resources(note: &TVal) -> Vec<RemoteEdamResource> {
    let mut out = Vec::new();
    if let Some(list) = note.field(13).and_then(|v| v.as_list()) {
        for r in list {
            if let Some(res) = parse_resource_struct(r) {
                out.push(res);
            }
        }
    }
    out
}

/// 解析单个 Resource STRUCT（chunk 元数据态与 getResource 全量态共用）：
/// f1 guid / f3 Data{f1 body, f3 size} / f4 mime / f14 attributes{f1 fileName}。
/// Data 内 f1 body 缺席时 data 为 None（元数据态，body 待按需拉取）。
fn parse_resource_struct(r: &TVal) -> Option<RemoteEdamResource> {
    let guid = r
        .field(1)
        .and_then(|g| g.as_str())
        .unwrap_or_default()
        .to_string();
    if guid.is_empty() {
        return None;
    }
    Some(RemoteEdamResource {
        guid,
        mime: r
            .field(4)
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
        file_name: r
            .field(14)
            .and_then(|a| a.field(1))
            .and_then(|f| f.as_str())
            .unwrap_or_default()
            .to_string(),
        data: r
            .field(3)
            .and_then(|d| d.field(1))
            .map(|b| b.as_bytes().unwrap_or_default().to_vec()),
    })
}

pub fn parse_sync_chunk(fields: &[(i16, TVal)]) -> RemoteSyncChunk {
    let fields_vec = fields.to_vec();
    let container = TVal::Struct(fields_vec);
    let mut chunk = RemoteSyncChunk {
        chunk_high_usn: 0,
        update_count: 0,
        notes: Vec::new(),
        notebooks: Vec::new(),
        tags: Vec::new(),
        expunged_note_guids: Vec::new(),
        expunged_notebook_guids: Vec::new(),
        expunged_tag_guids: Vec::new(),
    };
    {
        let fields = &container;
        let v_fields = match fields {
            TVal::Struct(f) => f.clone(),
            _ => Vec::new(),
        };
        for (id, v) in &v_fields {
            match *id {
                sync_chunk_fields::CHUNK_HIGH_USN => {
                    chunk.chunk_high_usn = v.as_i64().unwrap_or(0) as u64
                }
                sync_chunk_fields::UPDATE_COUNT => {
                    chunk.update_count = v.as_i64().unwrap_or(0) as u64
                }
                sync_chunk_fields::NOTES => {
                    if let Some(notes) = v.as_list() {
                        for n in notes {
                            let guid = n
                                .field(note_fields::GUID)
                                .and_then(|g| g.as_str())
                                .unwrap_or_default();
                            if guid.is_empty() {
                                continue;
                            }
                            chunk.notes.push(RemoteEdamNote {
                                guid: guid.to_string(),
                                title: n
                                    .field(note_fields::TITLE)
                                    .and_then(|t| t.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                content: n
                                    .field(note_fields::CONTENT)
                                    .and_then(|c| c.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                active: matches!(
                                    n.field(note_fields::ACTIVE),
                                    Some(TVal::Bool(true))
                                ),
                                notebook: n
                                    .field(11) // Note.notebookGuid
                                    .and_then(|g| g.as_str())
                                    .map(|s| s.to_string()),
                                updated: n
                                    .field(note_fields::UPDATED)
                                    .and_then(|u| u.as_i64())
                                    .unwrap_or(0),
                                resources: parse_note_resources(n),
                            });
                        }
                    }
                }
                sync_chunk_fields::NOTEBOOKS => {
                    if let Some(nbs) = v.as_list() {
                        for nb in nbs {
                            let guid = nb
                                .field(1) // Notebook.guid
                                .and_then(|g| g.as_str())
                                .unwrap_or_default();
                            if guid.is_empty() {
                                continue;
                            }
                            chunk.notebooks.push(RemoteEdamNotebook {
                                guid: guid.to_string(),
                                name: nb
                                    .field(2) // Notebook.name
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                stack: nb
                                    .field(7) // Notebook.stack（分组）
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            });
                        }
                    }
                }
                sync_chunk_fields::TAGS => {
                    if let Some(tgs) = v.as_list() {
                        for tg in tgs {
                            let guid = tg
                                .field(1) // Tag.guid
                                .and_then(|g| g.as_str())
                                .unwrap_or_default();
                            if guid.is_empty() {
                                continue;
                            }
                            chunk.tags.push(RemoteEdamTag {
                                guid: guid.to_string(),
                                name: tg
                                    .field(2) // Tag.name
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            });
                        }
                    }
                }
                sync_chunk_fields::EXPUNGED_NOTES => {
                    if let Some(guids) = v.as_list() {
                        for g in guids {
                            if let Some(guid) = g.as_str() {
                                chunk.expunged_note_guids.push(guid.to_string());
                            }
                        }
                    }
                }
                sync_chunk_fields::EXPUNGED_NOTEBOOKS => {
                    if let Some(guids) = v.as_list() {
                        for g in guids {
                            if let Some(guid) = g.as_str() {
                                chunk.expunged_notebook_guids.push(guid.to_string());
                            }
                        }
                    }
                }
                sync_chunk_fields::EXPUNGED_TAGS => {
                    if let Some(guids) = v.as_list() {
                        for g in guids {
                            if let Some(guid) = g.as_str() {
                                chunk.expunged_tag_guids.push(guid.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    chunk
}

fn parse_note_result(fields: &[(i16, TVal)]) -> Result<RemoteNoteResult, String> {
    let guid = fields
        .iter()
        .find(|(id, _)| *id == note_fields::GUID)
        .and_then(|(_, g)| g.as_str())
        .ok_or("createNote 响应缺 guid")?
        .to_string();
    let usn = fields
        .iter()
        .find(|(id, _)| *id == note_fields::UPDATE_SEQ)
        .and_then(|(_, v)| v.as_i64())
        .unwrap_or(0) as u64;
    Ok(RemoteNoteResult { guid, usn })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdentityResolver;
    use crate::PushPlan;
    use crate::SyncError;
    use crate::SyncRemote;

    /// 镜像 biji-server-go chunk 的 note STRUCT 布局（f13 资源元数据态：
    /// Data{f3 size} 无 body），断言解析出 guid/mime/fileName 且 data 为 None
    /// （body 待 getResource 按需拉取）。
    #[test]
    fn parse_chunk_note_resources_metadata_only() {
        let mut w = TWriter::new();
        // note 元素（list 元素直接字段序列，无字段头）
        w.field_string(1, "srv-note-1");
        w.field_string(2, "标题");
        w.field_string(3, "<en-note/>");
        w.field_bool(9, true);
        w.field_i64(7, 123);
        w.field_string(11, "srv-nb-1");
        w.field_list_begin(13, T_STRUCT, 1);
        w.field_string(1, "srv-res-1");
        w.field_begin(3, T_STRUCT); // Data{f3 size}（无 f1 body）
        w.field_i32(3, 7);
        w.field_stop();
        w.field_string(4, "application/octet-stream");
        w.field_begin(14, T_STRUCT); // attributes{fileName}
        w.field_string(1, "blob.bin");
        w.field_stop();
        w.field_stop(); // 资源 STRUCT
        w.field_stop(); // note STRUCT

        // 包进 chunk 容器：f4 notes list 首元素
        let note_fields = read_struct(&w.buf, &mut 0usize).expect("note 解析失败");
        let chunk_fields = vec![(4i16, TVal::List(vec![TVal::Struct(note_fields)]))];
        let chunk = parse_sync_chunk(&chunk_fields);
        assert_eq!(chunk.notes.len(), 1);
        let rs = &chunk.notes[0].resources;
        assert_eq!(rs.len(), 1, "resources 未解析出");
        assert_eq!(rs[0].guid, "srv-res-1");
        assert_eq!(rs[0].file_name, "blob.bin");
        assert_eq!(rs[0].mime, "application/octet-stream");
        assert_eq!(rs[0].data, None, "chunk 元数据态不应携带 body");
    }

    /// getResource 回执布局（Data{f1 body} 全量态）：字节级保真。
    #[test]
    fn parse_resource_full_body_layout() {
        let mut w = TWriter::new();
        w.field_string(1, "srv-res-1");
        w.field_begin(3, T_STRUCT); // Data{f1 body, f3 size}
        w.field_bytes(1, &[0u8, 255, 0, 254, 1, 2, 3]);
        w.field_i32(3, 7);
        w.field_stop();
        w.field_string(4, "application/octet-stream");
        w.field_begin(14, T_STRUCT);
        w.field_string(1, "blob.bin");
        w.field_stop();
        w.field_stop();
        let fields = read_struct(&w.buf, &mut 0usize).expect("resource 解析失败");
        let res = parse_resource_struct(&TVal::Struct(fields)).expect("缺 guid");
        assert_eq!(res.guid, "srv-res-1");
        assert_eq!(res.file_name, "blob.bin");
        assert_eq!(
            res.data,
            Some(vec![0u8, 255, 0, 254, 1, 2, 3]),
            "getResource 全量态字节保真"
        );
    }
    use crate::ServerRemote;

    #[test]
    fn message_header_layout() {
        let mut w = TWriter::new();
        w.message_begin("getSyncState", MSG_CALL, 1);
        // 严格头：版本字(4) + 名长(4) + 名(12) + 序号(4)
        assert_eq!(&w.buf[..4], &[0x80, 0x01, 0x00, 0x01]);
        assert_eq!(&w.buf[4..8], &[0, 0, 0, 12]); // 名长
        assert_eq!(&w.buf[8..20], b"getSyncState");
        assert_eq!(&w.buf[20..24], &[0, 0, 0, 1]); // seqid
    }

    #[test]
    fn field_roundtrip_string_and_i64() {
        let mut w = TWriter::new();
        w.field_string(1, "token-abc");
        w.field_i64(3, 4096);
        w.field_stop();
        let mut pos = 0usize;
        let fields = read_struct(&w.buf, &mut pos).expect("解析失败");
        assert_eq!(pos, w.buf.len()); // 恰好读完
        assert_eq!(fields[0].0, 1);
        assert_eq!(fields[0].1.as_str(), Some("token-abc"));
        assert_eq!(fields[1].0, 3);
        assert_eq!(fields[1].1.as_i64(), Some(4096));
    }

    #[test]
    fn sync_chunk_roundtrip() {
        // 手工构造 SyncChunk 响应 struct：
        // chunkHighUSN=7, updateCount=9, notes=[{guid="g1",title="t",content="c",active=true,updated=5}],
        // expungedNotes=["g9"]
        let mut note = TWriter::new();
        note.field_string(1, "g1");
        note.field_string(2, "t");
        note.field_string(3, "c");
        note.field_bool(9, true);
        note.field_i64(7, 5);
        note.field_stop();

        let mut w = TWriter::new();
        w.field_i32(2, 7);
        w.field_i32(3, 9);
        w.field_list_begin(4, T_STRUCT, 1);
        w.buf.extend_from_slice(&note.buf);
        w.field_list_begin(11, T_STRING, 1);
        w.string("g9");
        w.field_stop();

        let mut pos = 0usize;
        let fields = read_struct(&w.buf, &mut pos).expect("解析失败");
        let chunk = parse_sync_chunk(&fields);
        assert_eq!(chunk.chunk_high_usn, 7);
        assert_eq!(chunk.update_count, 9);
        assert_eq!(chunk.notes.len(), 1);
        assert_eq!(chunk.notes[0].guid, "g1");
        assert_eq!(chunk.notes[0].title, "t");
        assert!(chunk.notes[0].active);
        assert_eq!(chunk.notes[0].updated, 5);
        assert_eq!(chunk.expunged_note_guids, vec!["g9".to_string()]);
    }

    #[test]
    fn unknown_fields_skipped() {
        // 未知类型字段（map）夹在已知字段之间：读取器应跳过而非失败
        let mut w = TWriter::new();
        w.field_string(1, "keep");
        w.field_begin(5, T_MAP);
        w.u8(T_STRING);
        w.u8(T_I32);
        w.i32(1); // 一对
        w.string("k");
        w.i32(2);
        w.field_stop();
        // 读取一遍：map 字段被跳过（占位），但游标恰好走到缓冲区末尾
        let mut pos = 0usize;
        let fields = read_struct(&w.buf, &mut pos).expect("解析失败");
        assert_eq!(pos, w.buf.len()); // map 被完整跳过
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].1.as_str(), Some("keep"));
        assert_eq!(fields[1].0, 5); // 跳过后以占位值记录字段 id
    }

    #[test]
    fn missing_token_is_honest_error() {
        let mut remote = ServerRemote::with_token(crate::DEFAULT_SERVER_URL, None);
        let err = remote.pull(0).unwrap_err();
        assert!(matches!(err, SyncError::Unsupported(msg) if msg.contains("BIJI_SERVER_USER")));
        let store = biji_store::Store::open_memory().expect("mem");
        let mut ident = IdentityResolver::new(&store);
        let err = remote.upload(&PushPlan::default(), &mut ident).unwrap_err();
        assert!(matches!(err, SyncError::Unsupported(msg) if msg.contains("BIJI_SERVER_URL")));
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn framed_request_roundtrip() {
        // rpc 发送格式：4B 帧长 + 消息
        let mut w = TWriter::new();
        w.message_begin("getNoteStoreUrl", 1, 1);
        w.field_begin(1, T_STRUCT);
        w.field_stop();
        w.field_stop();
        w.u8(0);
        let mut framed = Vec::with_capacity(w.buf.len() + 4);
        framed.extend_from_slice(&(w.buf.len() as i32).to_be_bytes());
        framed.extend_from_slice(&w.buf);

        // 服务器侧按帧读：帧长 + 消息
        let flen = i32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
        assert_eq!(flen, w.buf.len());
        // 跳过 4B 帧头 + 消息头（版本4 + 名长4 + 名 + 序号4）后再读字段
        let name_len = i32::from_be_bytes(framed[8..12].try_into().unwrap()) as usize;
        let mut pos = 4 + 4 + 4 + name_len + 4;
        let fields = match read_struct(&framed, &mut pos) {
            Some(f) => f,
            None => {
                panic!("解析失败");
            }
        };
        assert!(pos <= framed.len()); // 字段解析完整消费（精确偏移由消息头长度决定）
        assert_eq!(fields[0].0, 1);
        assert!(matches!(fields[0].1, TVal::Struct(_)));
    }

    #[test]
    fn bare_exception_response_parses() {
        // 真实服务器实测：异常响应为裸消息（无帧头）
        // 80 01 00 03 | len=41 | "TApplicationException" | seq | f1 STR "EDAM processing error: Message length exceeded: 4" | STOP | STOP
        let mut w = TWriter::new();
        w.message_begin("TApplicationException", 3, 0);
        w.field_string(1, "EDAM processing error: Message length exceeded: 4");
        w.field_stop();
        w.u8(0);
        let mut pos = 0usize;
        let version = i32_at(&w.buf, &mut pos).unwrap();
        assert_eq!((version & 0xFF) as u8, 3); // EXCEPTION
        let _name = string_at(&w.buf, &mut pos).unwrap();
        let _seq = i32_at(&w.buf, &mut pos).unwrap();
        let fields = read_struct(&w.buf, &mut pos).unwrap();
        let msg = fields
            .iter()
            .find(|(id, _)| *id == 1)
            .and_then(|(_, v)| v.as_str())
            .unwrap();
        assert!(msg.contains("Message length exceeded"));
    }
}
