//! 本地存储层：复刻印象笔记 Windows 客户端 `.exb` 主库的核心数据模型。
//!
//! 对应分析文档 §7（本地数据存储）：
//! - 核心四表 `notebook_attr` / `note_attr` / `tag_attr` / `resource_attr`
//!   的 DDL 逐字段取自 `.exb` 字节层恢复的真实 schema；
//! - 应用自定义排序规则 `NOCASEUTF8` 在打开连接时注册（原库文本列
//!   `COLLATE NOCASEUTF8` 依赖应用注册，标准 SQLite 不内置）；
//! - `note_content` / `note_tag` / `sync_journal` / `meta` 为复刻扩展表
//!   （原库正文与同步账目的存放点未能完整恢复，见分析文档 §12）；
//! - 附件按原客户端布局落盘：`<db>.exb.attachments/`，按内容哈希两级寻址。

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("未找到 uid={0} 的记录")]
    NotFound(i64),
}

/// 当前时间（epoch 秒，REAL 列语义与原库一致）。
pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}


/// 打开附件目录布局的推导：`notes.exb` → `notes.exb.attachments`（镜像原客户端）。
pub fn attachments_dir_for(db_path: &Path) -> PathBuf {
    let mut name = db_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".attachments");
    db_path.with_file_name(name)
}

/// P3 企业空间行（`list_groupspaces` 投影）。
#[derive(Debug, Clone)]
pub struct GroupspaceRow {
    pub uid: i64,
    pub name: String,
    pub description: Option<String>,
    pub member_count: i64,
}

#[derive(Debug, Clone)]
pub struct Notebook {
    pub uid: i64,
    pub name: String,
    pub stack: Option<String>,
    pub date_created: f64,
    pub date_updated: f64,
    pub note_count: i64,
    pub is_local: bool,
    pub is_linked: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NoteDraft {
    pub title: String,
    pub notebook_uid: Option<i64>,
    pub enml: String,
    pub author: Option<String>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub content_class: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub uid: i64,
    pub title: String,
    pub notebook_uid: Option<i64>,
    /// 派生缓存列（原库 `note_attr.tags` 文本），真实关系在 `note_tag` 表。
    pub tags_text: Option<String>,
    pub date_created: f64,
    pub date_updated: f64,
    pub date_deleted: Option<f64>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub author: Option<String>,
    pub content_class: Option<String>,
    pub is_deleted: bool,
    pub is_shared: bool,
    pub is_published: bool,
    pub size: i64,
}

#[derive(Debug, Clone)]
pub struct NoteFull {
    pub note: Note,
    pub enml: String,
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub uid: i64,
    pub parent_uid: Option<i64>,
    pub name: String,
    pub date_created: f64,
    pub date_updated: f64,
    pub note_count: i64,
}

#[derive(Debug, Clone)]
pub struct ResourceMeta {
    pub uid: i64,
    pub note_uid: i64,
    pub mime: String,
    pub file_name: String,
    pub size: i64,
    pub hash: String,
    pub date_created: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub seq: i64,
    pub kind: String,
    pub item_uid: i64,
    pub op: String,
    pub usn: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum ListNotes {
    All,
    ByNotebook(i64),
    ByTag(i64),
    Trash,
}

const SCHEMA: &str = r#"
-- ===== 核心表：DDL 取自印象笔记 .exb 真实 schema（分析文档 §7.3）=====
CREATE TABLE IF NOT EXISTS notebook_attr (uid INTEGER PRIMARY KEY, name TEXT DEFAULT NULL COLLATE NOCASEUTF8, stack TEXT DEFAULT NULL COLLATE NOCASEUTF8, restrictions INTEGER DEFAULT NULL, can_move_to_container_status INTEGER DEFAULT NULL, flags INTEGER DEFAULT NULL, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL, date_last_chosen REAL DEFAULT NULL, item_color INTEGER DEFAULT NULL, item_style INTEGER DEFAULT NULL, note_count INTEGER DEFAULT NULL, is_local INTEGER DEFAULT NULL, is_linked INTEGER DEFAULT NULL, is_offline INTEGER DEFAULT NULL, is_accessible INTEGER DEFAULT NULL, business_id INTEGER DEFAULT NULL, shared_notebook_guid TEXT DEFAULT NULL, business_notebook_owner TEXT DEFAULT NULL COLLATE NOCASEUTF8, linked_notebook_username TEXT DEFAULT NULL COLLATE NOCASEUTF8, linked_notebook_userid INTEGER DEFAULT NULL, workspace TEXT DEFAULT NULL COLLATE NOCASEUTF8, workspace_uid INTEGER DEFAULT NULL, is_workspace_backing INTEGER DEFAULT NULL, in_workspace INTEGER DEFAULT NULL, groupspace TEXT DEFAULT NULL COLLATE NOCASEUTF8, groupspace_uid INTEGER DEFAULT NULL, is_groupspace_backing INTEGER DEFAULT NULL, in_groupspace INTEGER DEFAULT NULL, is_privacy INTEGER DEFAULT NULL);
CREATE TABLE IF NOT EXISTS note_attr (uid INTEGER PRIMARY KEY, title TEXT DEFAULT NULL COLLATE NOCASEUTF8, author TEXT DEFAULT NULL COLLATE NOCASEUTF8, creator_id INTEGER DEFAULT NULL, last_edited_by TEXT DEFAULT NULL COLLATE NOCASEUTF8, last_editor_id INTEGER DEFAULT NULL, notebook TEXT DEFAULT NULL COLLATE NOCASEUTF8, notebook_uid INTEGER DEFAULT NULL, tags TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL, date_deleted REAL DEFAULT NULL, date_subject REAL DEFAULT NULL, date_share REAL DEFAULT NULL, source TEXT DEFAULT NULL COLLATE NOCASEUTF8, source_url TEXT DEFAULT NULL COLLATE NOCASEUTF8, source_app TEXT DEFAULT NULL COLLATE NOCASEUTF8, latitude REAL DEFAULT NULL, longitude REAL DEFAULT NULL, altitude REAL DEFAULT NULL, geo_address TEXT DEFAULT NULL COLLATE NOCASEUTF8, geo_country TEXT DEFAULT NULL COLLATE NOCASEUTF8, geo_adminlevel1 TEXT DEFAULT NULL COLLATE NOCASEUTF8, geo_adminlevel2 TEXT DEFAULT NULL COLLATE NOCASEUTF8, geo_adminlevel3 TEXT DEFAULT NULL COLLATE NOCASEUTF8, reminder_order REAL DEFAULT NULL, reminder_time REAL DEFAULT NULL, reminder_done_time REAL DEFAULT NULL, place_name TEXT DEFAULT NULL COLLATE NOCASEUTF8, content_class TEXT DEFAULT NULL COLLATE NOCASEUTF8, title_quality INTEGER DEFAULT NULL, restrictions INTEGER DEFAULT NULL, flags INTEGER DEFAULT NULL, local_flags INTEGER DEFAULT NULL, has_encryption INTEGER DEFAULT NULL, has_todo INTEGER DEFAULT NULL, is_deleted INTEGER DEFAULT NULL, is_linked INTEGER DEFAULT NULL, is_shared INTEGER DEFAULT NULL, is_published INTEGER DEFAULT NULL, shared_with_business INTEGER DEFAULT NULL, shared_via_messaging INTEGER DEFAULT NULL, partial_sync INTEGER DEFAULT NULL, sync_required INTEGER DEFAULT NULL, has_sync_error_info INTEGER DEFAULT NULL, size INTEGER DEFAULT NULL);
CREATE TABLE IF NOT EXISTS tag_attr (uid INTEGER PRIMARY KEY, parent_uid INTEGER DEFAULT NULL, name TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL, item_color INTEGER DEFAULT NULL, item_style INTEGER DEFAULT NULL, local_flags INTEGER DEFAULT NULL, subtag_count INTEGER DEFAULT NULL, note_count INTEGER DEFAULT NULL, is_linked INTEGER DEFAULT NULL, business_id INTEGER DEFAULT NULL);
CREATE TABLE IF NOT EXISTS resource_attr (uid INTEGER PRIMARY KEY, note INTEGER DEFAULT 0, original INTEGER DEFAULT 0, mime TEXT DEFAULT NULL COLLATE NOCASEUTF8, width INTEGER DEFAULT 0, height INTEGER DEFAULT 0, duration INTEGER DEFAULT 0, source_url TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL, timestamp REAL DEFAULT NULL, latitude REAL DEFAULT NULL, longitude REAL DEFAULT NULL, altitude REAL DEFAULT NULL, camera_make TEXT DEFAULT NULL COLLATE NOCASEUTF8, camera_model TEXT DEFAULT NULL COLLATE NOCASEUTF8, reco_type TEXT DEFAULT NULL COLLATE NOCASEUTF8, file_name TEXT DEFAULT NULL COLLATE NOCASEUTF8, partial_sync INTEGER DEFAULT NULL, is_attachment INTEGER DEFAULT NULL, has_search_text INTEGER DEFAULT NULL, size INTEGER DEFAULT 0, hash TEXT DEFAULT NULL, is_deleted INTEGER DEFAULT 0);
-- ===== 复刻扩展表（原库对应存放点未恢复，见分析文档 §12）=====
CREATE TABLE IF NOT EXISTS note_content (note_uid INTEGER PRIMARY KEY REFERENCES note_attr(uid) ON DELETE CASCADE, enml TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS note_tag (note_uid INTEGER NOT NULL REFERENCES note_attr(uid) ON DELETE CASCADE, tag_uid INTEGER NOT NULL REFERENCES tag_attr(uid) ON DELETE CASCADE, PRIMARY KEY (note_uid, tag_uid));
CREATE TABLE IF NOT EXISTS expunged_items (record_id INTEGER PRIMARY KEY AUTOINCREMENT, item_uid INTEGER NOT NULL, kind TEXT NOT NULL, usn INTEGER NOT NULL, date REAL NOT NULL);
CREATE TABLE IF NOT EXISTS sync_journal (seq INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, item_uid INTEGER NOT NULL, op TEXT NOT NULL, usn INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_resource_note ON resource_attr(note);
-- ===== 同步身份映射（EDAM guid ↔ 本地 uid，复刻扩展）=====
CREATE TABLE IF NOT EXISTS sync_identity (kind TEXT NOT NULL, local_uid INTEGER NOT NULL, guid TEXT NOT NULL, PRIMARY KEY (kind, local_uid));
CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_identity_guid ON sync_identity(guid);
-- ===== P3 域表（企业空间/聊天/素材库，表名取自 .exb 清单，列为本复刻设计）=====
CREATE TABLE IF NOT EXISTS groupspace_attr (uid INTEGER PRIMARY KEY, name TEXT DEFAULT NULL COLLATE NOCASEUTF8, description TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL, member_count INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS groupspacemembership_attr (uid INTEGER PRIMARY KEY, groupspace_uid INTEGER DEFAULT NULL, member TEXT DEFAULT NULL COLLATE NOCASEUTF8, role INTEGER DEFAULT 0, date_joined REAL DEFAULT NULL);
CREATE TABLE IF NOT EXISTS thread_attr (uid INTEGER PRIMARY KEY, title TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_created REAL DEFAULT NULL, date_updated REAL DEFAULT NULL);
CREATE TABLE IF NOT EXISTS message_attr (uid INTEGER PRIMARY KEY, thread_uid INTEGER DEFAULT NULL, sender TEXT DEFAULT NULL COLLATE NOCASEUTF8, body TEXT DEFAULT NULL COLLATE NOCASEUTF8, date_sent REAL DEFAULT NULL);
CREATE TABLE IF NOT EXISTS material_attr (uid INTEGER PRIMARY KEY, title TEXT DEFAULT NULL COLLATE NOCASEUTF8, mime TEXT DEFAULT NULL COLLATE NOCASEUTF8, size INTEGER DEFAULT 0, hash TEXT DEFAULT NULL, date_created REAL DEFAULT NULL, note INTEGER DEFAULT 0);
"#;

/// 原库文本列的自定义排序规则：UTF-8 大小写折叠比较。
pub fn nocase_utf8(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(char::to_lowercase);
    let mut bi = b.chars().flat_map(char::to_lowercase);
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

pub struct Store {
    conn: Connection,
    attachments: PathBuf,
    /// 仅 open_memory 使用：保住临时目录生命周期。
    _tempdir: Option<tempfile::TempDir>,
}

impl Store {
    /// 打开（或创建）文件库。附件目录按原客户端布局自动推导。
    pub fn open(db_path: &Path) -> Result<Store> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let attachments = attachments_dir_for(db_path);
        fs::create_dir_all(&attachments)?;
        let conn = Connection::open(db_path)?;
        Self::init(conn, attachments, None)
    }

    /// 内存库 + 临时附件目录（测试与预览用）。
    pub fn open_memory() -> Result<Store> {
        let tempdir = tempfile::tempdir()?;
        let attachments = tempdir.path().join("biji.exb.attachments");
        fs::create_dir_all(&attachments)?;
        let conn = Connection::open_in_memory()?;
        Self::init(conn, attachments, Some(tempdir))
    }

    fn init(
        conn: Connection,
        attachments: PathBuf,
        tempdir: Option<tempfile::TempDir>,
    ) -> Result<Store> {
        conn.create_collation("NOCASEUTF8", nocase_utf8)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1'), ('usn', '0'), ('pdf_trials', '0'), ('pres_trials', '0')",
            [],
        )?;
        Ok(Store {
            conn,
            attachments,
            _tempdir: tempdir,
        })
    }

    pub fn attachments_dir(&self) -> &Path {
        &self.attachments
    }

    // ---------- meta / usn ----------

    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    fn next_usn(&self) -> Result<i64> {
        let usn: i64 = self.conn.query_row(
            "UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'usn' RETURNING CAST(value AS INTEGER)",
            [],
            |r| r.get(0),
        )?;
        Ok(usn)
    }

    fn journal(&self, kind: &str, item_uid: i64, op: &str) -> Result<i64> {
        let usn = self.next_usn()?;
        self.conn.execute(
            "INSERT INTO sync_journal(kind, item_uid, op, usn) VALUES (?1, ?2, ?3, ?4)",
            params![kind, item_uid, op, usn],
        )?;
        Ok(usn)
    }

    pub fn sync_journal_since(&self, usn: i64) -> Result<Vec<JournalEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, kind, item_uid, op, usn FROM sync_journal WHERE usn > ?1 ORDER BY usn")?;
        let rows = stmt
            .query_map(params![usn], |r| {
                Ok(JournalEntry {
                    seq: r.get(0)?,
                    kind: r.get(1)?,
                    item_uid: r.get(2)?,
                    op: r.get(3)?,
                    usn: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------- 同步身份映射（EDAM guid ↔ 本地 uid） ----------

    pub fn identity_get_guid(&self, kind: &str, local_uid: i64) -> Option<String> {
        self.conn
            .query_row(
                "SELECT guid FROM sync_identity WHERE kind = ?1 AND local_uid = ?2",
                params![kind, local_uid],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn identity_get_local(&self, kind: &str, guid: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT local_uid FROM sync_identity WHERE kind = ?1 AND guid = ?2",
                params![kind, guid],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn identity_set(&self, kind: &str, local_uid: i64, guid: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_identity(kind, local_uid, guid) VALUES (?1, ?2, ?3)              ON CONFLICT(kind, local_uid) DO UPDATE SET guid = ?3",
            params![kind, local_uid, guid],
        )?;
        Ok(())
    }

    // ---------- 远端变更应用（pull 方向；不写本地日志，服务器是该变更的事实源） ----------

    /// 按 guid 应用远端笔记本：本地已有 identity 映射则改名对齐，
    /// 否则按名建库（远端创建，非本地）并登记映射。幂等。
    pub fn apply_remote_notebook(&self, guid: &str, name: &str) -> Result<()> {
        if let Some(uid) = self.identity_get_local("notebook", guid) {
            self.conn.execute(
                "UPDATE notebook_attr SET name = ?2, date_updated = date_updated WHERE uid = ?1",
                params![uid, name],
            )?;
            return Ok(());
        }
        // 同名本地笔记本直接认领（避免重名重复建库）
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT uid FROM notebook_attr WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        let uid = match existing {
            Some(u) => u,
            None => {
                self.conn.execute(
                    "INSERT INTO notebook_attr(name, date_created, date_updated, note_count, is_local) VALUES (?1, strftime('%s','now'), strftime('%s','now'), 0, 0)",
                    params![name],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        self.identity_set("notebook", uid, guid)?;
        Ok(())
    }

    /// 按 guid 应用远端标签：本地已有 identity 映射则改名对齐，否则按名
    /// 建库（远端创建）并登记映射；同名本地标签直接认领。幂等。
    pub fn apply_remote_tag(&self, guid: &str, name: &str) -> Result<()> {
        if let Some(uid) = self.identity_get_local("tag", guid) {
            self.conn.execute(
                "UPDATE tag_attr SET name = ?2, date_updated = date_updated WHERE uid = ?1",
                params![uid, name],
            )?;
            return Ok(());
        }
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT uid FROM tag_attr WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        let uid = match existing {
            Some(u) => u,
            None => {
                self.conn.execute(
                    "INSERT INTO tag_attr(parent_uid, name, date_created, date_updated, note_count) VALUES (NULL, ?1, strftime('%s','now'), strftime('%s','now'), 0)",
                    params![name],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        self.identity_set("tag", uid, guid)?;
        Ok(())
    }

    /// 按 guid upsert 远端笔记。`notebook` 提供名称时自动落到同名笔记本（缺失则创建）。
    pub fn apply_remote_note(
        &self,
        guid: &str,
        title: &str,
        content: &str,
        notebook: Option<&str>,
        updated: f64,
        deleted: bool,
    ) -> Result<()> {
        let local = self.identity_get_local("note", guid);
        match (local, deleted) {
            (Some(uid), true) => {
                self.conn
                    .execute("DELETE FROM note_attr WHERE uid = ?1", params![uid])?;
                self.conn.execute(
                    "DELETE FROM sync_identity WHERE kind = 'note' AND local_uid = ?1",
                    params![uid],
                )?;
            }
            (Some(uid), false) => {
                self.conn.execute(
                    "UPDATE note_attr SET title = ?2, size = ?3, date_updated = ?4, is_deleted = 0, date_deleted = NULL WHERE uid = ?1",
                    params![uid, title, content.len() as i64, updated],
                )?;
                self.conn.execute(
                    "UPDATE note_content SET enml = ?2 WHERE note_uid = ?1",
                    params![uid, content],
                )?;
            }
            (None, false) => {
                let notebook_uid: Option<i64> = match notebook {
                    Some(name) => {
                        let existing: Option<i64> = self
                            .conn
                            .query_row(
                                "SELECT uid FROM notebook_attr WHERE name = ?1",
                                params![name],
                                |r| r.get(0),
                            )
                            .optional()?;
                        match existing {
                            Some(u) => Some(u),
                            None => {
                                self.conn.execute(
                                    "INSERT INTO notebook_attr(name, date_created, date_updated, note_count, is_local) VALUES (?1, ?2, ?2, 0, 0)",
                                    params![name, updated],
                                )?;
                                Some(self.conn.last_insert_rowid())
                            }
                        }
                    }
                    None => None,
                };
                self.conn.execute(
                    "INSERT INTO note_attr(title, notebook_uid, date_created, date_updated, size, is_deleted) VALUES (?1, ?2, ?3, ?3, ?4, 0)",
                    params![title, notebook_uid, updated, content.len() as i64],
                )?;
                let uid = self.conn.last_insert_rowid();
                self.conn.execute(
                    "INSERT INTO note_content(note_uid, enml) VALUES (?1, ?2)",
                    params![uid, content],
                )?;
                self.identity_set("note", uid, guid)?;
                self.touch_notebook(notebook_uid)?;
            }
            (None, true) => {}
        }
        Ok(())
    }

    /// 按 guid 应用远端删除（删除账对账：本地行移除，不另记删除账）。
    pub fn apply_remote_expunge(&self, kind: &str, guid: &str) -> Result<()> {
        let Some(uid) = self.identity_get_local(kind, guid) else {
            return Ok(()); // 本地没有该对象，幂等
        };
        match kind {
            "note" => {
                self.conn
                    .execute("DELETE FROM note_attr WHERE uid = ?1", params![uid])?;
            }
            "notebook" => {
                self.conn
                    .execute("DELETE FROM notebook_attr WHERE uid = ?1", params![uid])?;
            }
            "tag" => {
                self.conn.execute(
                    "DELETE FROM tag_attr WHERE uid = ?1",
                    params![uid],
                )?;
            }
            _ => {}
        }
        self.conn.execute(
            "DELETE FROM sync_identity WHERE kind = ?1 AND local_uid = ?2",
            params![kind, uid],
        )?;
        Ok(())
    }

    /// 删除账查询（同步推送/对账用）：`(item_uid, kind, usn)`。
    pub fn expunged_since(&self, usn: i64) -> Result<Vec<(i64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_uid, kind, usn FROM expunged_items WHERE usn > ?1 ORDER BY usn",
        )?;
        let rows = stmt
            .query_map(params![usn], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------- notebook ----------

    pub fn create_notebook(&self, name: &str, stack: Option<&str>) -> Result<i64> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO notebook_attr(name, stack, date_created, date_updated, note_count, is_local) VALUES (?1, ?2, ?3, ?3, 0, 1)",
            params![name, stack, now],
        )?;
        let uid = self.conn.last_insert_rowid();
        self.journal("notebook", uid, "create")?;
        Ok(uid)
    }

    pub fn list_notebooks(&self) -> Result<Vec<Notebook>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, name, stack, date_created, date_updated, note_count, is_local, is_linked FROM notebook_attr ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Notebook {
                    uid: r.get(0)?,
                    name: r.get(1)?,
                    stack: r.get(2)?,
                    date_created: r.get(3)?,
                    date_updated: r.get(4)?,
                    note_count: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    is_local: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                    is_linked: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 删除笔记本：其笔记进回收站，笔记本本体进删除账。
    pub fn delete_notebook(&self, uid: i64) -> Result<()> {
        let note_ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT uid FROM note_attr WHERE notebook_uid = ?1 AND is_deleted = 0")?;
            let rows = stmt.query_map(params![uid], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for note_uid in note_ids {
            self.trash_note(note_uid)?;
        }
        self.conn
            .execute("DELETE FROM notebook_attr WHERE uid = ?1", params![uid])?;
        self.expunge_record(uid, "notebook")?;
        Ok(())
    }

    // ---------- tag ----------

    pub fn create_tag(&self, name: &str, parent_uid: Option<i64>) -> Result<i64> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO tag_attr(parent_uid, name, date_created, date_updated, note_count) VALUES (?1, ?2, ?3, ?3, 0)",
            params![parent_uid, name, now],
        )?;
        let uid = self.conn.last_insert_rowid();
        self.journal("tag", uid, "create")?;
        Ok(uid)
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, parent_uid, name, date_created, date_updated, note_count FROM tag_attr ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Tag {
                    uid: r.get(0)?,
                    parent_uid: r.get(1)?,
                    name: r.get(2)?,
                    date_created: r.get(3)?,
                    date_updated: r.get(4)?,
                    note_count: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 某条笔记的标签（uid, name），按 `note_tag` 关系表实时读取
    /// （编辑器「添加标签」行使用；列表摘要走 `tags` 文本缓存）。
    pub fn note_tags(&self, note_uid: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.uid, t.name FROM note_tag nt JOIN tag_attr t ON t.uid = nt.tag_uid \
             WHERE nt.note_uid = ?1 ORDER BY t.name",
        )?;
        let rows = stmt
            .query_map([note_uid], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 打标签：维护 `note_tag` 关系、`tag_attr.note_count` 与 `note_attr.tags`
    /// 文本缓存列（三处与原库语义对齐）。
    pub fn attach_tag(&self, note_uid: i64, tag_uid: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO note_tag(note_uid, tag_uid) VALUES (?1, ?2)",
            params![note_uid, tag_uid],
        )?;
        self.conn.execute(
            "UPDATE tag_attr SET note_count = (SELECT COUNT(*) FROM note_tag WHERE tag_uid = ?1), date_updated = ?2 WHERE uid = ?1",
            params![tag_uid, now_epoch()],
        )?;
        self.refresh_note_tags_text(note_uid)?;
        self.touch_note(note_uid)?;
        self.journal("tag", tag_uid, "attach")?;
        Ok(())
    }

    pub fn detach_tag(&self, note_uid: i64, tag_uid: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM note_tag WHERE note_uid = ?1 AND tag_uid = ?2",
            params![note_uid, tag_uid],
        )?;
        self.conn.execute(
            "UPDATE tag_attr SET note_count = (SELECT COUNT(*) FROM note_tag WHERE tag_uid = ?1), date_updated = ?2 WHERE uid = ?1",
            params![tag_uid, now_epoch()],
        )?;
        self.refresh_note_tags_text(note_uid)?;
        self.touch_note(note_uid)?;
        self.journal("tag", tag_uid, "detach")?;
        Ok(())
    }

    /// 删除标签：解除全部笔记关联并刷新 tags 文本缓存，移除标签行，
    /// 写删除账（对位 delete_notebook 的删除账语义）。
    pub fn delete_tag(&self, uid: i64) -> Result<()> {
        let note_ids: Vec<i64> = {
            let mut stmt =
                self.conn
                    .prepare("SELECT note_uid FROM note_tag WHERE tag_uid = ?1")?;
            let rows = stmt.query_map(params![uid], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        self.conn
            .execute("DELETE FROM note_tag WHERE tag_uid = ?1", params![uid])?;
        for note_uid in note_ids {
            self.refresh_note_tags_text(note_uid)?;
        }
        self.conn
            .execute("DELETE FROM tag_attr WHERE uid = ?1", params![uid])?;
        self.expunge_record(uid, "tag")?;
        Ok(())
    }

    fn refresh_note_tags_text(&self, note_uid: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE note_attr SET tags = (SELECT GROUP_CONCAT(t.name, ', ') FROM note_tag nt JOIN tag_attr t ON t.uid = nt.tag_uid WHERE nt.note_uid = ?1) WHERE uid = ?1",
            params![note_uid],
        )?;
        Ok(())
    }

    // ---------- note ----------

    pub fn create_note(&self, draft: &NoteDraft) -> Result<i64> {
        let now = now_epoch();
        let notebook_uid = draft.notebook_uid;
        self.conn.execute(
            "INSERT INTO note_attr(title, author, notebook_uid, date_created, date_updated, source, source_url, content_class, size, is_deleted) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                draft.title,
                draft.author,
                notebook_uid,
                now,
                draft.source,
                draft.source_url,
                draft.content_class,
                draft.enml.len() as i64,
            ],
        )?;
        let uid = self.conn.last_insert_rowid();
        if let Some(nb) = notebook_uid {
            let name: Option<String> = self
                .conn
                .query_row(
                    "SELECT name FROM notebook_attr WHERE uid = ?1",
                    params![nb],
                    |r| r.get(0),
                )
                .optional()?;
            self.conn.execute(
                "UPDATE note_attr SET notebook = ?2 WHERE uid = ?1",
                params![uid, name],
            )?;
        }
        self.conn.execute(
            "INSERT INTO note_content(note_uid, enml) VALUES (?1, ?2)",
            params![uid, draft.enml],
        )?;
        self.touch_notebook(notebook_uid)?;
        self.journal("note", uid, "create")?;
        Ok(uid)
    }

    pub fn get_note(&self, uid: i64) -> Result<NoteFull> {
        let note = self.get_note_meta(uid)?;
        let enml: String = self.conn.query_row(
            "SELECT enml FROM note_content WHERE note_uid = ?1",
            params![uid],
            |r| r.get(0),
        )?;
        Ok(NoteFull { note, enml })
    }

    pub fn get_note_meta(&self, uid: i64) -> Result<Note> {
        self.conn
            .query_row(
                "SELECT uid, title, notebook_uid, tags, date_created, date_updated, date_deleted, source, source_url, author, content_class, is_deleted, is_shared, is_published, size FROM note_attr WHERE uid = ?1",
                params![uid],
                |r| {
                    Ok(Note {
                        uid: r.get(0)?,
                        title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        notebook_uid: r.get(2)?,
                        tags_text: r.get(3)?,
                        date_created: r.get(4)?,
                        date_updated: r.get(5)?,
                        date_deleted: r.get(6)?,
                        source: r.get(7)?,
                        source_url: r.get(8)?,
                        author: r.get(9)?,
                        content_class: r.get(10)?,
                        is_deleted: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                        is_shared: r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                        is_published: r.get::<_, Option<i64>>(13)?.unwrap_or(0) != 0,
                        size: r.get::<_, Option<i64>>(14)?.unwrap_or(0),
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound(uid))
    }

    pub fn list_notes(&self, filter: ListNotes) -> Result<Vec<Note>> {
        let (sql, bind): (String, Vec<i64>) = match filter {
            ListNotes::All => (
                "SELECT uid, title, notebook_uid, tags, date_created, date_updated, date_deleted, source, source_url, author, content_class, is_deleted, is_shared, is_published, size FROM note_attr WHERE is_deleted = 0 ORDER BY date_updated DESC".into(),
                vec![],
            ),
            ListNotes::ByNotebook(nb) => (
                "SELECT uid, title, notebook_uid, tags, date_created, date_updated, date_deleted, source, source_url, author, content_class, is_deleted, is_shared, is_published, size FROM note_attr WHERE is_deleted = 0 AND notebook_uid = ?1 ORDER BY date_updated DESC".into(),
                vec![nb],
            ),
            ListNotes::ByTag(tag) => (
                "SELECT n.uid, n.title, n.notebook_uid, n.tags, n.date_created, n.date_updated, n.date_deleted, n.source, n.source_url, n.author, n.content_class, n.is_deleted, n.is_shared, n.is_published, n.size FROM note_attr n JOIN note_tag nt ON nt.note_uid = n.uid WHERE n.is_deleted = 0 AND nt.tag_uid = ?1 ORDER BY n.date_updated DESC".into(),
                vec![tag],
            ),
            ListNotes::Trash => (
                "SELECT uid, title, notebook_uid, tags, date_created, date_updated, date_deleted, source, source_url, author, content_class, is_deleted, is_shared, is_published, size FROM note_attr WHERE is_deleted = 1 ORDER BY date_deleted DESC".into(),
                vec![],
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind), |r| {
                Ok(Note {
                    uid: r.get(0)?,
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    notebook_uid: r.get(2)?,
                    tags_text: r.get(3)?,
                    date_created: r.get(4)?,
                    date_updated: r.get(5)?,
                    date_deleted: r.get(6)?,
                    source: r.get(7)?,
                    source_url: r.get(8)?,
                    author: r.get(9)?,
                    content_class: r.get(10)?,
                    is_deleted: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                    is_shared: r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                    is_published: r.get::<_, Option<i64>>(13)?.unwrap_or(0) != 0,
                    size: r.get::<_, Option<i64>>(14)?.unwrap_or(0),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 更新标题等元数据；`title=None` 表示不修改。
    pub fn update_note_meta(
        &self,
        uid: i64,
        title: Option<&str>,
        author: Option<&str>,
        source_url: Option<&str>,
    ) -> Result<()> {
        self.get_note_meta(uid)?;
        self.conn.execute(
            "UPDATE note_attr SET title = COALESCE(?2, title), author = COALESCE(?3, author), source_url = COALESCE(?4, source_url) WHERE uid = ?1",
            params![uid, title, author, source_url],
        )?;
        self.touch_note(uid)?;
        self.journal("note", uid, "update")?;
        Ok(())
    }

    /// 写回 ENML 正文（含 size 维护与 journal 记账）。
    pub fn update_note_content(&self, uid: i64, enml: &str) -> Result<()> {
        self.get_note_meta(uid)?;
        self.conn.execute(
            "UPDATE note_content SET enml = ?2 WHERE note_uid = ?1",
            params![uid, enml],
        )?;
        self.conn.execute(
            "UPDATE note_attr SET size = ?2 WHERE uid = ?1",
            params![uid, enml.len() as i64],
        )?;
        self.touch_note(uid)?;
        self.journal("note", uid, "update")?;
        Ok(())
    }

    /// 移入回收站（原库 `is_deleted`/`date_deleted` 语义）。
    pub fn trash_note(&self, uid: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE note_attr SET is_deleted = 1, date_deleted = ?2 WHERE uid = ?1",
            params![uid, now_epoch()],
        )?;
        self.journal("note", uid, "trash")?;
        Ok(())
    }

    pub fn restore_note(&self, uid: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE note_attr SET is_deleted = 0, date_deleted = NULL WHERE uid = ?1",
            params![uid],
        )?;
        self.journal("note", uid, "restore")?;
        Ok(())
    }

    /// 彻底删除：移除行 + 写删除账（`expunged_items`，同步必需）。
    pub fn expunge_note(&self, uid: i64) -> Result<()> {
        self.get_note_meta(uid)?;
        self.conn
            .execute("DELETE FROM note_attr WHERE uid = ?1", params![uid])?;
        self.expunge_record(uid, "note")?;
        Ok(())
    }

    fn expunge_record(&self, item_uid: i64, kind: &str) -> Result<()> {
        let usn = self.next_usn()?;
        self.conn.execute(
            "INSERT INTO expunged_items(item_uid, kind, usn, date) VALUES (?1, ?2, ?3, ?4)",
            params![item_uid, kind, usn, now_epoch()],
        )?;
        self.conn.execute(
            "INSERT INTO sync_journal(kind, item_uid, op, usn) VALUES (?1, ?2, 'expunge', ?3)",
            params![kind, item_uid, usn],
        )?;
        Ok(())
    }

    fn touch_note(&self, uid: i64) -> Result<()> {
        let notebook_uid: Option<i64> = self.conn.query_row(
            "UPDATE note_attr SET date_updated = ?2 WHERE uid = ?1 RETURNING notebook_uid",
            params![uid, now_epoch()],
            |r| r.get(0),
        )?;
        self.touch_notebook(notebook_uid)
    }

    fn touch_notebook(&self, notebook_uid: Option<i64>) -> Result<()> {
        if let Some(nb) = notebook_uid {
            self.conn.execute(
                "UPDATE notebook_attr SET date_updated = ?2, note_count = (SELECT COUNT(*) FROM note_attr WHERE notebook_uid = ?1 AND is_deleted = 0) WHERE uid = ?1",
                params![nb, now_epoch()],
            )?;
        }
        Ok(())
    }

    /// 子串检索（v0 用 LIKE：对中文友好；FTS5 分词升级路径见 README）。
    pub fn search_notes(&self, query: &str) -> Result<Vec<Note>> {
        let pattern = format!("%{}%", query.replace('%', ""));
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT n.uid, n.title, n.notebook_uid, n.tags, n.date_created, n.date_updated, n.date_deleted, n.source, n.source_url, n.author, n.content_class, n.is_deleted, n.is_shared, n.is_published, n.size \
             FROM note_attr n LEFT JOIN note_content c ON c.note_uid = n.uid \
             WHERE n.is_deleted = 0 AND (n.title LIKE ?1 OR c.enml LIKE ?1) \
             ORDER BY n.date_updated DESC",
        )?;
        let rows = stmt
            .query_map(params![pattern], |r| {
                Ok(Note {
                    uid: r.get(0)?,
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    notebook_uid: r.get(2)?,
                    tags_text: r.get(3)?,
                    date_created: r.get(4)?,
                    date_updated: r.get(5)?,
                    date_deleted: r.get(6)?,
                    source: r.get(7)?,
                    source_url: r.get(8)?,
                    author: r.get(9)?,
                    content_class: r.get(10)?,
                    is_deleted: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                    is_shared: r.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                    is_published: r.get::<_, Option<i64>>(13)?.unwrap_or(0) != 0,
                    size: r.get::<_, Option<i64>>(14)?.unwrap_or(0),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------- resource（附件） ----------

    pub fn add_resource(
        &self,
        note_uid: i64,
        file_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<i64> {
        let hash = format!("{:x}", Sha256::digest(bytes));
        self.write_blob(&hash, bytes)?;
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO resource_attr(note, mime, file_name, size, hash, date_created, date_updated, is_attachment) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1)",
            params![note_uid, mime, file_name, bytes.len() as i64, hash, now],
        )?;
        let uid = self.conn.last_insert_rowid();
        self.touch_note(note_uid)?;
        self.journal("resource", uid, "create")?;
        Ok(uid)
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.attachments.join(&hash[..2]).join(hash)
    }

    fn write_blob(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        let path = self.blob_path(hash);
        if path.exists() {
            return Ok(()); // 内容寻址去重
        }
        fs::create_dir_all(path.parent().expect("两级哈希路径必有父目录"))?;
        fs::write(&path, bytes)?;
        Ok(())
    }

    pub fn list_resources(&self, note_uid: i64) -> Result<Vec<ResourceMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, note, mime, file_name, size, hash, date_created FROM resource_attr WHERE note = ?1 AND is_deleted = 0 ORDER BY uid",
        )?;
        let rows = stmt
            .query_map(params![note_uid], |r| {
                Ok(ResourceMeta {
                    uid: r.get(0)?,
                    note_uid: r.get(1)?,
                    mime: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    file_name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    size: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    hash: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    date_created: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 按 guid 应用远端附件：identity 已映射则更新元数据（字节按内容寻址
    /// 去重），否则新建。幂等；不记 journal（远端来源，天然无回声）。
    pub fn apply_remote_resource(
        &self,
        note_uid: i64,
        guid: &str,
        file_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let hash = format!("{:x}", Sha256::digest(bytes));
        self.write_blob(&hash, bytes)?;
        if let Some(uid) = self.identity_get_local("resource", guid) {
            self.conn.execute(
                "UPDATE resource_attr SET mime = ?2, file_name = ?3, size = ?4, hash = ?5 WHERE uid = ?1",
                params![uid, mime, file_name, bytes.len() as i64, hash],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO resource_attr(note, mime, file_name, size, hash, date_created, date_updated, is_attachment)              VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), strftime('%s','now'), 1)",
            params![note_uid, mime, file_name, bytes.len() as i64, hash],
        )?;
        let uid = self.conn.last_insert_rowid();
        self.identity_set("resource", uid, guid)?;
        Ok(())
    }

    pub fn get_resource(&self, uid: i64) -> Result<(ResourceMeta, Vec<u8>)> {
        let meta = self
            .list_resource_meta(uid)?
            .ok_or(StoreError::NotFound(uid))?;
        let bytes = fs::read(self.blob_path(&meta.hash))?;
        Ok((meta, bytes))
    }

    pub fn list_resource_meta(&self, uid: i64) -> Result<Option<ResourceMeta>> {
        self.conn
            .query_row(
                "SELECT uid, note, mime, file_name, size, hash, date_created FROM resource_attr WHERE uid = ?1",
                params![uid],
                |r| {
                    Ok(ResourceMeta {
                        uid: r.get(0)?,
                        note_uid: r.get(1)?,
                        mime: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        file_name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        size: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        hash: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        date_created: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 整体替换附件内容（内容寻址换新 blob，size/hash 同步维护）。
    pub fn replace_resource_bytes(&self, uid: i64, bytes: &[u8]) -> Result<()> {
        let old_hash: Option<String> = self
            .conn
            .query_row(
                "UPDATE resource_attr SET hash = ?2, size = ?3, date_updated = ?4 WHERE uid = ?1 RETURNING hash",
                params![uid, format!("{:x}", Sha256::digest(bytes)), bytes.len() as i64, now_epoch()],
                |r| r.get(0),
            )
            .optional()?;
        if old_hash.is_none() {
            return Err(StoreError::NotFound(uid));
        }
        self.write_blob(
            &format!("{:x}", Sha256::digest(bytes)),
            bytes,
        )?;
        Ok(())
    }

    // ---------- P3：企业空间 / 聊天 / 素材库 ----------
    // 表名取自 .exb 清单（分析文档 §7.3），列为本复刻设计。

    pub fn create_groupspace(&self, name: &str, description: Option<&str>) -> Result<i64> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO groupspace_attr(name, description, date_created, date_updated, member_count) VALUES (?1, ?2, ?3, ?3, 1)",
            params![name, description, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_groupspaces(&self) -> Result<Vec<GroupspaceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, name, description, member_count FROM groupspace_attr ORDER BY date_created",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GroupspaceRow {
                    uid: r.get(0)?,
                    name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    description: r.get(2)?,
                    member_count: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 加入企业空间：写成员关系并维护 member_count。
    pub fn join_groupspace(&self, gs_uid: i64, member: &str, role: i64) -> Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO groupspacemembership_attr(groupspace_uid, member, role, date_joined) VALUES (?1, ?2, ?3, ?4)",
            params![gs_uid, member, role, now],
        )?;
        self.conn.execute(
            "UPDATE groupspace_attr SET member_count = (SELECT COUNT(*) FROM groupspacemembership_attr WHERE groupspace_uid = ?1) WHERE uid = ?1",
            params![gs_uid],
        )?;
        Ok(())
    }

    pub fn create_thread(&self, title: &str) -> Result<i64> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO thread_attr(title, date_created, date_updated) VALUES (?1, ?2, ?2)",
            params![title, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `(uid, title, date_updated)`，按最近更新倒序。
    pub fn list_threads(&self) -> Result<Vec<(i64, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, title, date_updated FROM thread_attr ORDER BY date_updated DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn post_message(&self, thread_uid: i64, sender: &str, body: &str) -> Result<i64> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO message_attr(thread_uid, sender, body, date_sent) VALUES (?1, ?2, ?3, ?4)",
            params![thread_uid, sender, body, now],
        )?;
        self.conn.execute(
            "UPDATE thread_attr SET date_updated = ?2 WHERE uid = ?1",
            params![thread_uid, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `(uid, sender, body, date_sent)`，按时间正序。
    pub fn list_messages(
        &self,
        thread_uid: i64,
    ) -> Result<Vec<(i64, String, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, sender, body, date_sent FROM message_attr WHERE thread_uid = ?1 ORDER BY date_sent, uid",
        )?;
        let rows = stmt
            .query_map(params![thread_uid], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_material(
        &self,
        title: &str,
        mime: &str,
        size: i64,
        hash: &str,
        note_uid: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO material_attr(title, mime, size, hash, date_created, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![title, mime, size, hash, now_epoch(), note_uid],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_materials(&self) -> Result<Vec<(i64, String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, title, mime, size FROM material_attr ORDER BY date_created DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------- 试用计数（对应 DesktopService.pdfTrialStarted / presTrialStarted） ----------

    pub fn bump_trial(&self, kind: &str) -> Result<i64> {
        let key = format!("{kind}_trials");
        self.conn.execute(
            "UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key = ?1",
            params![key],
        )?;
        let v: i64 = self
            .conn
            .query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1", params![key], |r| r.get(0))?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn collation_case_insensitive_utf8() {
        let s = store();
        s.create_notebook("Apple", None).unwrap();
        s.create_notebook("apple 玛丽", None).unwrap();
        s.create_notebook("Banana", None).unwrap();
        let names: Vec<String> = s
            .list_notebooks()
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        // NOCASEUTF8 下 "apple 玛丽" 应与 "Apple" 视为同序起点，排在 "Banana" 前。
        assert_eq!(names[0], "Apple");
        assert_eq!(names[1], "apple 玛丽");
        assert_eq!(names[2], "Banana");
    }

    #[test]
    fn notebook_crud_and_cascade_trash() {
        let s = store();
        let nb = s.create_notebook("工作", Some("公司")).unwrap();
        let note = s
            .create_note(&NoteDraft {
                title: "周报".into(),
                notebook_uid: Some(nb),
                enml: "<en-note><p>内容</p></en-note>".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.list_notebooks().unwrap()[0].note_count, 1);
        s.delete_notebook(nb).unwrap();
        let trashed = s.list_notes(ListNotes::Trash).unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].uid, note);
        assert!(s.list_notes(ListNotes::All).unwrap().is_empty());
    }

    #[test]
    fn note_crud_with_content() {
        let s = store();
        let uid = s
            .create_note(&NoteDraft {
                title: "测试".into(),
                enml: "<en-note><p>v1</p></en-note>".into(),
                ..Default::default()
            })
            .unwrap();
        let full = s.get_note(uid).unwrap();
        assert_eq!(full.note.title, "测试");
        assert!(full.enml.contains("v1"));
        assert_eq!(full.note.size, full.enml.len() as i64);

        s.update_note_content(uid, "<en-note><p>v2 很长一点</p></en-note>")
            .unwrap();
        assert!(s.get_note(uid).unwrap().enml.contains("v2"));

        s.update_note_meta(uid, Some("新标题"), None, Some("https://example.com"))
            .unwrap();
        let meta = s.get_note_meta(uid).unwrap();
        assert_eq!(meta.title, "新标题");
        assert_eq!(meta.source_url.as_deref(), Some("https://example.com"));

        s.trash_note(uid).unwrap();
        assert_eq!(s.list_notes(ListNotes::Trash).unwrap().len(), 1);
        s.restore_note(uid).unwrap();
        assert_eq!(s.list_notes(ListNotes::All).unwrap().len(), 1);
        s.expunge_note(uid).unwrap();
        assert!(matches!(
            s.get_note(uid),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn tag_attach_updates_all_three_places() {
        let s = store();
        let nb = s.create_notebook("默认", None).unwrap();
        let n1 = s
            .create_note(&NoteDraft {
                title: "n1".into(),
                notebook_uid: Some(nb),
                enml: "<en-note/>".into(),
                ..Default::default()
            })
            .unwrap();
        let n2 = s
            .create_note(&NoteDraft {
                title: "n2".into(),
                notebook_uid: Some(nb),
                enml: "<en-note/>".into(),
                ..Default::default()
            })
            .unwrap();
        let t = s.create_tag("重要", None).unwrap();
        s.attach_tag(n1, t).unwrap();
        s.attach_tag(n2, t).unwrap();

        let tag = s.list_tags().unwrap().into_iter().next().unwrap();
        assert_eq!(tag.note_count, 2);

        let note = s.get_note_meta(n1).unwrap();
        assert_eq!(note.tags_text.as_deref(), Some("重要"));

        let by_tag = s.list_notes(ListNotes::ByTag(t)).unwrap();
        assert_eq!(by_tag.len(), 2);

        s.detach_tag(n1, t).unwrap();
        let tag = s.list_tags().unwrap().into_iter().next().unwrap();
        assert_eq!(tag.note_count, 1);
        assert_eq!(s.get_note_meta(n1).unwrap().tags_text, None);

        // note_tags：关系表实时读取（编辑器「添加标签」行数据源）
        s.attach_tag(n1, t).unwrap();
        assert_eq!(s.note_tags(n1).unwrap(), vec![(t, "重要".to_string())]);
        assert_eq!(s.note_tags(n2).unwrap(), vec![(t, "重要".to_string())]);
        let other = s.create_note(&NoteDraft {
            title: "n3".into(),
            notebook_uid: Some(nb),
            enml: "<en-note/>".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(s.note_tags(other).unwrap().is_empty());
    }

    #[test]
    fn search_finds_title_and_content() {
        let s = store();
        s.create_note(&NoteDraft {
            title: "Rust 学习笔记".into(),
            enml: "<en-note><p>所有权与借用</p></en-note>".into(),
            ..Default::default()
        })
        .unwrap();
        s.create_note(&NoteDraft {
            title: "购物清单".into(),
            enml: "<en-note><p>牛奶 鸡蛋 rust清洁剂</p></en-note>".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.search_notes("Rust").unwrap().len(), 2);
        assert_eq!(s.search_notes("所有权").unwrap().len(), 1);
        assert_eq!(s.search_notes("不存在的词").unwrap().len(), 0);
    }

    #[test]
    fn resource_blob_roundtrip_dedup() {
        let s = store();
        let nb = s.create_notebook("nb", None).unwrap();
        let note = s
            .create_note(&NoteDraft {
                title: "附件".into(),
                notebook_uid: Some(nb),
                enml: "<en-note/>".into(),
                ..Default::default()
            })
            .unwrap();
        let data = b"hello attachment".to_vec();
        let r1 = s
            .add_resource(note, "a.txt", "text/plain", &data)
            .unwrap();
        let r2 = s
            .add_resource(note, "b.txt", "text/plain", &data)
            .unwrap();
        assert_ne!(r1, r2, "两条 resource 记录，但共享同一 blob");
        let (meta, bytes) = s.get_resource(r2).unwrap();
        assert_eq!(bytes, data);
        assert_eq!(meta.size as usize, data.len());
        assert_eq!(s.list_resources(note).unwrap().len(), 2);
    }

    #[test]
    fn sync_journal_and_expunge_ledger() {
        let s = store();
        let uid = s
            .create_note(&NoteDraft {
                title: "x".into(),
                enml: "<en-note/>".into(),
                ..Default::default()
            })
            .unwrap();
        let entries = s.sync_journal_since(0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].op, "create");
        s.expunge_note(uid).unwrap();
        let entries = s.sync_journal_since(0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].op, "expunge");
        let count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM expunged_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn trial_counters_increment() {
        let s = store();
        assert_eq!(s.bump_trial("pdf").unwrap(), 1);
        assert_eq!(s.bump_trial("pdf").unwrap(), 2);
        assert_eq!(s.bump_trial("pres").unwrap(), 1);
    }

    #[test]
    fn groupspace_membership_and_count() {
        let s = store();
        let gs = s.create_groupspace("产品团队", Some("迭代协作")).unwrap();
        s.join_groupspace(gs, "alice#app.yinxiang.com", 1).unwrap();
        s.join_groupspace(gs, "bob#app.yinxiang.com", 0).unwrap();
        let list = s.list_groupspaces().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "产品团队");
        assert_eq!(list[0].member_count, 2, "member_count 应随加入递增");
    }

    #[test]
    fn thread_messages_roundtrip() {
        let s = store();
        let th = s.create_thread("复刻讨论").unwrap();
        s.post_message(th, "alice", "数据模型已完成").unwrap();
        s.post_message(th, "bob", "GUI 走查通过").unwrap();
        let msgs = s.list_messages(th).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].1, "alice");
        assert!(msgs[1].2.contains("GUI"));
        let threads = s.list_threads().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].1, "复刻讨论");
    }

    #[test]
    fn material_registry() {
        let s = store();
        s.add_material("设计稿", "image/png", 2048, "abc", 0).unwrap();
        s.add_material("规格文档", "application/pdf", 4096, "def", 0).unwrap();
        let list = s.list_materials().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].1, "规格文档", "按创建时间倒序");
    }

    #[test]
    fn attachments_dir_mirrors_original_layout() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.exb");
        let s = Store::open(&db).unwrap();
        assert_eq!(
            s.attachments_dir(),
            dir.path().join("notes.exb.attachments")
        );
    }
}
