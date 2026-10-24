//! 全链路集成：内存 transport 注入 EdamClient，实跑 ServerRemote（自有后端
//! biji-server-go 的客户端）完整 sync_round（请求编码断言 + 响应解析 +
//! SyncRemote 语义）。真实 HTTP 联调即「启动 biji-server-go + 默认零配置」。

use biji_store::{NoteDraft, Store};
use biji_sync::edam::{TWriter, T_STRUCT};
use biji_sync::{ServerRemote, SyncEngine, SyncRemote};
use std::cell::RefCell;
use std::rc::Rc;

const MSG_REPLY: u8 = 2;

fn reply(method: &str, fields: impl FnOnce(&mut TWriter)) -> Vec<u8> {
    let mut w = TWriter::new();
    w.message_begin(method, MSG_REPLY, 1);
    w.field_begin(0, T_STRUCT);
    fields(&mut w);
    w.field_stop();
    w.u8(0);
    w.buf
}

/// 裸顶层字段响应（updateNote/updateNotebook 返回 i32、expunge 返回 i64）。
fn reply_raw(method: &str, fields: impl FnOnce(&mut TWriter)) -> Vec<u8> {
    let mut w = TWriter::new();
    w.message_begin(method, MSG_REPLY, 1);
    fields(&mut w);
    w.field_stop();
    w.u8(0);
    w.buf
}

/// 从 Thrift 请求体提取方法名（版本字 4B + 名长 4B + 名）。
fn request_method_name(body: &[u8]) -> String {
    // 兼容帧传输：0x80 开头为裸消息，否则跳过 4B 帧长
    let msg = if body.first() == Some(&0x80) {
        body
    } else {
        &body[4..]
    };
    let len = i32::from_be_bytes(msg[4..8].try_into().unwrap()) as usize;
    String::from_utf8_lossy(&msg[8..8 + len]).into_owned()
}

#[test]
fn full_sync_round_against_in_memory_edam() {
    // ---- 脚本：按调用序应答；记录收到的请求供断言 ----
    type CallLog = Rc<RefCell<Vec<(String, Vec<u8>)>>>;
    let calls: CallLog = Rc::new(RefCell::new(Vec::new()));
    let chunk_calls = std::cell::Cell::new(0usize);
    let note_url = "http://test/edam/note";
    let calls_in: CallLog = calls.clone();
    let transport = move |url: &str, body: &[u8]| -> Result<Vec<u8>, String> {
        let method = request_method_name(body).to_string();
        calls_in.borrow_mut().push((url.to_string(), body.to_vec()));
        Ok(match method.as_str() {
            "getNoteStoreUrl" => reply("getNoteStoreUrl", |w| {
                w.field_string(0, note_url);
            }),
            "getSyncState" => reply("getSyncState", |w| {
                w.field_i64(3, 6); // updateCount
            }),
            "getFilteredSyncChunk" => {
                // 首个拉取块含远端变更；后续拉取块为空（已追平）
                let n = chunk_calls.get();
                chunk_calls.set(n + 1);
                reply("getFilteredSyncChunk", move |w| {
                    w.field_i32(2, 6); // chunkHighUSN
                    w.field_i64(3, 6); // updateCount
                    if n == 0 {
                        w.field_list_begin(5, T_STRUCT, 1); // notebooks
                        {
                            w.field_string(1, "srv-nb-1");
                            w.field_string(2, "远端笔记本");
                            w.field_i32(5, 1);
                            w.field_stop();
                        }
                        // f6 tags：标签跨端传播（Tag: f1 guid, f2 name）
                        w.field_list_begin(6, T_STRUCT, 1);
                        {
                            w.field_string(1, "srv-tag-1");
                            w.field_string(2, "远端标签");
                            w.field_stop();
                        }
                        // f12 expungedNotebooks：笔记本删除账
                        w.field_list_begin(12, biji_sync::edam::T_STRING, 1);
                        w.string("srv-nb-expunged-1");
                        w.field_list_begin(4, T_STRUCT, 1); // notes
                        {
                            w.field_string(1, "srv-note-1");
                            w.field_string(2, "远端笔记");
                            w.field_string(3, "<en-note>来自云端</en-note>");
                            w.field_bool(9, true); // active
                            w.field_i64(7, 1_700_000_000_000);
                            w.field_string(11, "srv-nb-1");
                            // f13 资源元数据态：Data{f3 size} 无 body（getResource 按需取）
                            w.field_list_begin(13, T_STRUCT, 1);
                            {
                                w.field_string(1, "srv-res-1");
                                w.field_begin(3, T_STRUCT);
                                w.field_i32(3, 7);
                                w.field_stop();
                                w.field_string(4, "application/octet-stream");
                                w.field_begin(14, T_STRUCT);
                                w.field_string(1, "blob.bin");
                                w.field_stop();
                                w.field_stop();
                            }
                            w.field_stop();
                        }
                        w.field_list_begin(11, biji_sync::edam::T_STRING, 1); // expungedNotes
                        w.string("srv-expunged-1");
                        w.field_stop();
                    }
                })
            }
            "getResource" => reply("getResource", |w| {
                // 全量态：Data{f1 body, f3 size}（与 chunk 元数据态互补）
                w.field_string(1, "srv-res-1");
                w.field_begin(3, T_STRUCT);
                w.field_bytes(1, &[0u8, 255, 0, 254, 1, 2, 3]);
                w.field_i32(3, 7);
                w.field_stop();
                w.field_string(4, "application/octet-stream");
                w.field_begin(14, T_STRUCT);
                w.field_string(1, "blob.bin");
                w.field_stop();
                w.field_stop();
            }),
            "createNote" => reply("createNote", |w| {
                w.field_string(1, "srv-new-1");
                w.field_i32(10, 6); // updateSequenceNum
            }),
            "createNotebook" => reply("createNotebook", |w| {
                w.field_string(1, "srv-nb-cloud");
                w.field_i32(5, 6);
            }),
            "updateNotebook" | "updateTag" => reply_raw("updateNotebook", |w| {
                w.field_i32(0, 7); // updateSequenceNum
            }),
            "expungeNotebook" | "expungeTag" | "expungeNote" => {
                reply_raw("expungeNote", |w| {
                    w.field_i64(0, 7);
                })
            }
            "createTag" => reply("createTag", |w| {
                w.field_string(1, "srv-tag-x");
            }),
            other => {
                let stripped = other;
                return Err(format!("unexpected call: {stripped}"));
            }
        })
    };
    let dbg_transport = transport; // 保留闭包供注入
    let transport_log =
        { move |url: &str, body: &[u8]| -> Result<Vec<u8>, String> { dbg_transport(url, body) } };

    let mut remote = ServerRemote::with_token(
        biji_sync::DEFAULT_SERVER_URL,
        Some("TEST-TOKEN".into()),
    );
    remote.set_transport("TEST-TOKEN", Rc::new(transport_log), note_url.to_string());

    // ---- 本地库：一条待推送笔记 ----
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("it.exb");
    let store = Store::open(&db).expect("open");
    store.create_notebook("云端", None).unwrap();
    let uid = store
        .create_note(&NoteDraft {
            title: "本地笔记".into(),
            notebook_uid: Some(1),
            enml: "<en-note>本地</en-note>".into(),
            ..Default::default()
        })
        .unwrap();

    // ---- 第一轮：拉取远端 + 推送本地 ----
    let engine = SyncEngine::new(&store);
    let report = engine.sync_round(&mut remote).expect("round1");
    assert_eq!(report.pushed, 2, "应推送笔记本+笔记两条变更");
    assert_eq!(report.pulled_notes, 1, "应拉取 1 条远端笔记");
    assert_eq!(report.server_usn, 6);

    // 服务器 guid 已回填 identity 表
    assert_eq!(
        store.identity_get_guid("note", uid).as_deref(),
        Some("srv-new-1")
    );
    // 远端笔记已落库（标题可检索）
    let hits = store.search_notes("来自云端").unwrap();
    assert_eq!(hits.len(), 1);
    // chunk 回传的笔记本已按 guid 对齐为本地笔记本，笔记归属正确
    let nb = store
        .list_notebooks()
        .unwrap()
        .into_iter()
        .find(|nb| nb.name == "远端笔记本")
        .expect("远端笔记本未对齐");
    // f6 标签已跨端传播并登记 identity
    let tg = store
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "远端标签")
        .expect("远端标签未落库");
    assert_eq!(
        store.identity_get_local("tag", "srv-tag-1"),
        Some(tg.uid),
        "标签 guid 映射缺失"
    );
    // f12 笔记本删除账已对账（未知 guid 幂等无操作，此处验证通路不 panic）
    assert!(store.identity_get_local("notebook", "srv-nb-expunged-1").is_none());
    assert_eq!(
        store.identity_get_local("notebook", "srv-nb-1"),
        Some(nb.uid),
        "笔记本 guid 映射缺失"
    );
    let note_uid = store.identity_get_local("note", "srv-note-1").expect("笔记映射缺失");
    let full = store.get_note(note_uid).expect("笔记缺失");
    assert_eq!(full.note.notebook_uid, Some(nb.uid), "笔记归属未落到远端笔记本");

    // 附件经 getResource 按需拉取并落库（chunk 只带元数据）
    let res_uid = store
        .identity_get_local("resource", "srv-res-1")
        .expect("资源映射缺失");
    let (meta, bytes) = store.get_resource(res_uid).expect("资源缺失");
    assert_eq!(meta.file_name, "blob.bin");
    assert_eq!(bytes, vec![0u8, 255, 0, 254, 1, 2, 3], "按需拉取字节保真");

    // ---- 第二轮：无变更，幂等空轮 ----
    let report2 = engine.sync_round(&mut remote).expect("round2");
    assert_eq!(report2.pushed, 0);
    assert_eq!(report2.pulled_notes, 0);
    assert_eq!(report2.server_usn, 6);

    // ---- 请求序列断言：URL 与方法名 ----
    let calls = calls.borrow();
    // 独立解析器交叉验证 createNote 请求：服务器将看到的 wire 内容
    // wire 层级：message f1 = args 容器（内含 f1 token + f2 note 结构体）
    {
        // 调用序：getSyncState → chunk → getResource(按需取附件) → createNotebook
        //        → createNote → getSyncState(上传收尾)
        let (url, body) = &calls[4]; // 第 5 个调用 = createNote
        assert_eq!(url, note_url);
        assert_eq!(
            i32::from_be_bytes(body[..4].try_into().unwrap()) as usize,
            body.len() - 4
        ); // 帧长与体一致
        let mut p = independent::Parser {
            b: &body[4..],
            p: 0,
        };
        let (name, mtype) = p.message().expect("解析失败");
        assert_eq!(name, "createNote");
        assert_eq!(mtype, 1); // CALL
        let args_container = p.fields().expect("args 解析失败");
        let args_container = args_container
            .iter()
            .find(|f| f.id == 1)
            .expect("缺 args 容器");
        let inner = match &args_container.val {
            independent::TVal::Struct(v) => v,
            other => panic!("args 容器值非 struct: {other:?}"),
        };
        let token = inner.iter().find(|f| f.id == 1).expect("缺 token");
        assert_eq!(token.val, independent::TVal::Str("TEST-TOKEN".into()));
        let note_struct = inner.iter().find(|f| f.id == 2).expect("缺 note");
        let note_fields = match &note_struct.val {
            independent::TVal::Struct(v) => v,
            other => panic!("note 字段值非 struct: {other:?}"),
        };
        let title = note_fields.iter().find(|f| f.id == 2).expect("缺 title");
        assert_eq!(title.val, independent::TVal::Str("本地笔记".into()));
        // 笔记本归属上云：note.notebookGuid（f11）= 先推笔记本时登记的服务器 guid
        let nb = note_fields.iter().find(|f| f.id == 11).expect("缺 notebookGuid");
        assert_eq!(nb.val, independent::TVal::Str("srv-nb-cloud".into()));
        let content = note_fields.iter().find(|f| f.id == 3).expect("缺 content");
        if let independent::TVal::Str(c) = &content.val {
            assert!(c.contains("<en-note>本地</en-note>"));
        } else {
            panic!("content 不是字符串");
        }
        let active = note_fields.iter().find(|f| f.id == 9).expect("缺 active");
        assert!(matches!(active.val, independent::TVal::Bool(true)));
    }

    // getFilteredSyncChunk 请求含 token 与 afterUSN=6（第二轮拉取游标）
    {
        let (url, body) = &calls[7]; // 第 8 个调用 = round2 的 getFilteredSyncChunk
        assert_eq!(url, note_url);
        assert_eq!(
            i32::from_be_bytes(body[..4].try_into().unwrap()) as usize,
            body.len() - 4
        );
        let mut p = independent::Parser {
            b: &body[4..],
            p: 0,
        };
        let (name, _) = p.message().expect("解析失败");
        assert_eq!(name, "getFilteredSyncChunk");
        let args_container = p.fields().expect("args 解析失败");
        let args_container = args_container
            .iter()
            .find(|f| f.id == 1)
            .expect("缺 args 容器");
        let inner = match &args_container.val {
            independent::TVal::Struct(v) => v,
            other => panic!("args 容器值非 struct: {other:?}"),
        };
        let token = inner.iter().find(|f| f.id == 1).expect("缺 token");
        assert_eq!(token.val, independent::TVal::Str("TEST-TOKEN".into()));
        let after = inner.iter().find(|f| f.id == 2).expect("缺 afterUSN");
        assert_eq!(after.val, independent::TVal::I32(6));
    }

    // getResource 请求：guid 随行 + withBinary/withData 均真；
    // 两轮合计仅一次（已落库附件不重复下载）
    {
        let get_res: Vec<&(String, Vec<u8>)> = calls
            .iter()
            .filter(|(_, b)| request_method_name(b).contains("getResource"))
            .collect();
        assert_eq!(get_res.len(), 1, "第二轮不应重复拉取已落库附件");
        let (url, body) = get_res[0];
        assert_eq!(url, note_url);
        let mut p = independent::Parser {
            b: &body[4..],
            p: 0,
        };
        let (name, _) = p.message().expect("解析失败");
        assert_eq!(name, "getResource");
        let args_container = p.fields().expect("args 解析失败");
        let args_container = args_container
            .iter()
            .find(|f| f.id == 1)
            .expect("缺 args 容器");
        let inner = match &args_container.val {
            independent::TVal::Struct(v) => v,
            other => panic!("args 容器值非 struct: {other:?}"),
        };
        let guid = inner.iter().find(|f| f.id == 2).expect("缺 guid");
        assert_eq!(guid.val, independent::TVal::Str("srv-res-1".into()));
        assert!(matches!(
            inner.iter().find(|f| f.id == 3).map(|f| &f.val),
            Some(independent::TVal::Bool(true))
        ));
        assert!(matches!(
            inner.iter().find(|f| f.id == 4).map(|f| &f.val),
            Some(independent::TVal::Bool(true))
        ));
    }
}
#[test]
fn missing_token_is_honest_error() {
    // authenticate 失败（如服务端未启动）时 connect() 保留未认证句柄：
    // 同步调用报 Unsupported，错误文案给出自有后端联调指引。
    let mut remote = ServerRemote::with_token(biji_sync::DEFAULT_SERVER_URL, None);
    let err = remote.pull(0).unwrap_err();
    assert!(
        matches!(err, biji_sync::SyncError::Unsupported(msg) if msg.contains("BIJI_SERVER_USER"))
    );
    let store = Store::open_memory().expect("mem");
    let mut ident = biji_sync::IdentityResolver::new(&store);
    let err = remote
        .upload(&biji_sync::PushPlan::default(), &mut ident)
        .unwrap_err();
    assert!(
        matches!(err, biji_sync::SyncError::Unsupported(msg) if msg.contains("BIJI_SERVER_URL"))
    );
}

// ---- 独立服务端解析器：与 edam.rs 编解码零共享代码，交叉验证线格式 ----

/// 按 Thrift 二进制规范独立解析消息（递归下降，与 read_struct 无共享实现）。
mod independent {
    #[derive(Debug, PartialEq, Clone)]
    pub enum TVal {
        Bool(bool),
        I32(i32),
        I64(i64),
        Str(String),
        List(Vec<TVal>),
        Struct(Vec<Field>),
    }

    #[derive(Debug, PartialEq, Clone)]
    pub struct Field {
        pub id: i16,
        pub val: TVal,
    }

    pub struct Parser<'a> {
        pub b: &'a [u8],
        pub p: usize,
    }

    impl<'a> Parser<'a> {
        pub fn u8(&mut self) -> Option<u8> {
            let v = *self.b.get(self.p)?;
            self.p += 1;
            Some(v)
        }
        pub fn be(&mut self, n: usize) -> Option<Vec<u8>> {
            let v = self.b.get(self.p..self.p + n)?.to_vec();
            self.p += n;
            Some(v)
        }
        pub fn i32v(&mut self) -> Option<i32> {
            Some(i32::from_be_bytes(self.be(4)?.try_into().unwrap()))
        }
        pub fn i64v(&mut self) -> Option<i64> {
            Some(i64::from_be_bytes(self.be(8)?.try_into().unwrap()))
        }
        pub fn strv(&mut self) -> Option<String> {
            let len = self.i32v()? as usize;
            let b = self.be(len)?;
            Some(String::from_utf8_lossy(&b).into_owned())
        }
        /// 消息头：严格版本字 i32 + 名长 i32 + 名 + 序号 i32
        pub fn message(&mut self) -> Option<(String, u8)> {
            let ver = self.i32v()?;
            if ver & 0xffff_0000u32 as i32 != (0x8001u32 << 16) as i32 {
                return None;
            }
            let mtype = (ver & 0xff) as u8;
            let name = self.strv()?;
            let _seq = self.i32v()?;
            Some((name, mtype))
        }
        /// struct 字段序列（直到 STOP）
        pub fn fields(&mut self) -> Option<Vec<Field>> {
            let mut out = Vec::new();
            loop {
                let t = self.u8()?;
                if t == 0 {
                    return Some(out);
                }
                let id = i16::from_be_bytes(self.be(2)?.try_into().unwrap());
                let val = self.value(t)?;
                out.push(Field { id, val });
            }
        }
        fn value(&mut self, t: u8) -> Option<TVal> {
            match t {
                2 => Some(TVal::Bool(self.u8()? == 1)),
                8 => Some(TVal::I32(self.i32v()?)),
                10 => Some(TVal::I64(self.i64v()?)),
                11 | 16 => Some(TVal::Str(self.strv()?)),
                12 => Some(TVal::Struct(self.fields()?)),
                15 => {
                    let _et = self.u8()?;
                    let n = self.i32v()? as usize;
                    let mut v = Vec::new();
                    for _ in 0..n {
                        let et = self.u8()?;
                        let id = i16::from_be_bytes(self.be(2)?.try_into().unwrap());
                        let val = self.value(et)?;
                        v.push(TVal::Struct(vec![Field { id, val }]));
                    }
                    Some(TVal::List(v))
                }
                _ => None,
            }
        }
    }
}

// ---- S1/S2/S3 服务端补齐对应的客户端锚定测试 ----

/// 请求体解码到顶层字段（剥帧 + 消息头 + args 壳 f1 → 内层字段）。
fn request_args_fields(body: &[u8]) -> Vec<independent::Field> {
    let msg: &[u8] = if body.first() == Some(&0x80) {
        body
    } else {
        &body[4..]
    };
    let mut p = independent::Parser { b: msg, p: 0 };
    let _ = p.message().expect("消息头解析失败");
    let fields = p.fields().expect("args 解析失败");
    // args 容器：message f1 = struct
    fields
        .iter()
        .find(|f| f.id == 1)
        .map(|f| match &f.val {
            independent::TVal::Struct(inner) => inner.clone(),
            _ => vec![],
        })
        .unwrap_or_default()
}

fn tval_str(v: &independent::TVal) -> Option<&str> {
    match v {
        independent::TVal::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

type CallLog = std::rc::Rc<std::cell::RefCell<Vec<(String, Vec<u8>)>>>;

/// S1：createNotebook 请求携带 f7 stack；updateNotebook 空串清除。
#[test]
fn notebook_stack_round_trips_on_wire() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("st.exb");
    let store = Store::open(&db).unwrap();
    store.create_notebook("工作", Some("公司")).unwrap();

    let calls: CallLog = Default::default();
    let calls_in = calls.clone();
    let transport = move |_url: &str, body: &[u8]| -> Result<Vec<u8>, String> {
        let method = request_method_name(body);
        calls_in.borrow_mut().push((method.clone(), body.to_vec()));
        Ok(match method.as_str() {
            "getNoteStoreUrl" => reply("getNoteStoreUrl", |w| {
                w.field_string(0, "http://test/edam/note");
            }),
            "getSyncState" => reply("getSyncState", |w| {
                w.field_i64(3, 1);
            }),
            "getFilteredSyncChunk" => reply("getFilteredSyncChunk", |w| {
                w.field_i32(2, 1);
                w.field_i64(3, 1);
                w.field_stop();
            }),
            "createNotebook" => reply("createNotebook", |w| {
                w.field_string(1, "nb-stack-1");
                w.field_i32(5, 1);
            }),
            other => return Err(format!("unexpected: {other}")),
        })
    };
    let mut remote = ServerRemote::with_token("http://t", Some("T".into()));
    remote.set_transport("T", std::rc::Rc::new(transport), "http://test/edam/note".into());
    SyncEngine::new(&store).sync_round(&mut remote).unwrap();

    // 请求 wire 断言：createNotebook 的 notebook 结构含 f2 name + f7 stack
    let create = calls
        .borrow()
        .iter()
        .find(|(m, _)| m == "createNotebook")
        .map(|(_, b)| b.clone())
        .expect("缺 createNotebook 调用");
    let args = request_args_fields(&create);
    let nb = args
        .iter()
        .find(|f| f.id == 2)
        .map(|f| match &f.val {
            independent::TVal::Struct(inner) => inner.clone(),
            _ => vec![],
        })
        .unwrap_or_default();
    assert!(nb
        .iter()
        .any(|f| f.id == 2 && tval_str(&f.val) == Some("工作")));
    assert!(nb
        .iter()
        .any(|f| f.id == 7 && tval_str(&f.val) == Some("公司")));
}

/// S1：pull 方向 chunk f5 notebooks 带 f7 → 本地 notebook_attr.stack 落库。
#[test]
fn pulled_notebook_stack_lands_in_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("st2.exb");
    let store = Store::open(&db).unwrap();
    let note_url = "http://test/edam/note";
    let transport = move |_url: &str, body: &[u8]| -> Result<Vec<u8>, String> {
        Ok(match request_method_name(body).as_str() {
            "getNoteStoreUrl" => reply("getNoteStoreUrl", |w| {
                w.field_string(0, note_url);
            }),
            "getSyncState" => reply("getSyncState", |w| {
                w.field_i64(3, 2);
            }),
            "getFilteredSyncChunk" => {
                let mut w = TWriter::new();
                w.message_begin("getFilteredSyncChunk", MSG_REPLY, 1);
                w.field_begin(0, T_STRUCT);
                w.field_i32(2, 2);
                w.field_i64(3, 2);
                // f5 notebooks：一条带 stack
                w.field_list_begin(5, T_STRUCT, 1);
                w.field_string(1, "nb-remote-1");
                w.field_string(2, "远端本");
                w.field_string(7, "远端分组");
                w.field_i32(5, 1);
                w.field_stop();
                w.field_stop();
                w.u8(0);
                w.buf
            }
            other => return Err(format!("unexpected: {other}")),
        })
    };
    let mut remote = ServerRemote::with_token("http://t", Some("T".into()));
    remote.set_transport("T", std::rc::Rc::new(transport), note_url.into());
    SyncEngine::new(&store).sync_round(&mut remote).unwrap();

    let nbs = store.list_notebooks().unwrap();
    let nb = nbs.iter().find(|n| n.name == "远端本").expect("笔记本未落库");
    assert_eq!(store.notebook_stack(nb.uid).unwrap().flatten(), Some("远端分组".to_string()));
}

/// S3：分页拉取循环——两页截断 chunk + 终页，全部笔记落库。
#[test]
fn paginated_chunks_are_looped_until_caught_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pg.exb");
    let store = Store::open(&db).unwrap();
    let note_url = "http://test/edam/note";
    let page = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let page_in = page.clone();
    let note_at = |w: &mut TWriter, guid: &str, title: &str| {
        w.field_string(1, guid);
        w.field_string(2, title);
        w.field_string(3, "<en-note>x</en-note>");
        w.field_bool(9, true);
        w.field_i64(7, 5);
        w.field_stop();
    };
    let transport = move |_url: &str, body: &[u8]| -> Result<Vec<u8>, String> {
        Ok(match request_method_name(body).as_str() {
            "getNoteStoreUrl" => reply("getNoteStoreUrl", |w| {
                w.field_string(0, note_url);
            }),
            "getSyncState" => reply("getSyncState", |w| {
                w.field_i64(3, 10);
            }),
            "getFilteredSyncChunk" => {
                page_in.set(page_in.get() + 1);
                let mut w = TWriter::new();
                w.message_begin("getFilteredSyncChunk", MSG_REPLY, 1);
                w.field_begin(0, T_STRUCT);
                match page_in.get() {
                    1 => {
                        // 截断页：high=5（< updateCount=10），notes n1 n2
                        w.field_i32(2, 5);
                        w.field_i64(3, 10);
                        w.field_list_begin(4, T_STRUCT, 2);
                        note_at(&mut w, "n1", "第一页一");
                        note_at(&mut w, "n2", "第一页二");
                    }
                    _ => {
                        // 终页：high=10 = updateCount
                        w.field_i32(2, 10);
                        w.field_i64(3, 10);
                        w.field_list_begin(4, T_STRUCT, 1);
                        note_at(&mut w, "n3", "第二页三");
                    }
                }
                w.field_stop();
                w.u8(0);
                w.buf
            }
            other => return Err(format!("unexpected: {other}")),
        })
    };
    let mut remote = ServerRemote::with_token("http://t", Some("T".into()));
    remote.set_transport("T", std::rc::Rc::new(transport), note_url.into());
    let report = SyncEngine::new(&store).sync_round(&mut remote).unwrap();

    assert_eq!(page.get(), 2, "应恰好两轮拉取");
    assert_eq!(report.pulled_notes, 3);
    assert!(store.search_notes("第一页").unwrap().len() == 2);
    assert!(store.search_notes("第二页").unwrap().len() == 1);
}

/// S2：资源变更走 putResource 快路径（有资源身份时不重推整笔记）。
#[test]
fn resource_change_prefers_put_resource_fast_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pr.exb");
    let store = Store::open(&db).unwrap();
    // 造一条已有远端身份的笔记 + 资源（模拟上一轮 pull 后的状态）
    store
        .apply_remote_note(
            "note-g-1",
            "带附件",
            "<en-note>带附件</en-note>",
            Some("云端"),
            0.0,
            false,
        )
        .unwrap();
    let uid = store.search_notes("带附件").unwrap()[0].uid;
    store
        .apply_remote_resource(uid, "res-g-1", "a.bin", "application/octet-stream", b"OLD")
        .unwrap();
    let res_uid = store.list_resources(uid).unwrap()[0].uid;
    store.replace_resource_bytes(res_uid, b"NEWBYTES").unwrap();

    let note_url = "http://test/edam/note";
    let saw_put = std::rc::Rc::new(std::cell::Cell::new(false));
    let saw_update_note = std::rc::Rc::new(std::cell::Cell::new(false));
    let saw_put_in = saw_put.clone();
    let saw_update_in = saw_update_note.clone();
    let transport = move |_url: &str, body: &[u8]| -> Result<Vec<u8>, String> {
        Ok(match request_method_name(body).as_str() {
            "getNoteStoreUrl" => reply("getNoteStoreUrl", |w| {
                w.field_string(0, note_url);
            }),
            "getSyncState" => reply("getSyncState", |w| {
                w.field_i64(3, 5);
            }),
            "getFilteredSyncChunk" => reply("getFilteredSyncChunk", |w| {
                w.field_i32(2, 5);
                w.field_i64(3, 5);
                w.field_stop();
            }),
            "putResource" => {
                saw_put_in.set(true);
                reply_raw("putResource", |w| {
                    w.field_i64(0, 6);
                })
            }
            "updateNote" => {
                saw_update_in.set(true);
                reply_raw("updateNote", |w| {
                    w.field_i32(0, 6);
                })
            }
            other => return Err(format!("unexpected: {other}")),
        })
    };
    let mut remote = ServerRemote::with_token("http://t", Some("T".into()));
    remote.set_transport("T", std::rc::Rc::new(transport), note_url.into());
    SyncEngine::new(&store).sync_round(&mut remote).unwrap();

    assert!(saw_put.get(), "应调用 putResource 快路径");
    assert!(!saw_update_note.get(), "有资源身份时不应整笔记重推");
}
