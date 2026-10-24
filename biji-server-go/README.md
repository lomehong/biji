# biji-server-go

自研笔记服务端：用 Golang 实现 EDAM（Thrift 二进制 over HTTP）同步子集，
对接 Rust 复刻客户端（`biji-cli` / `biji-gui` 的 `biji-sync::edam`），
**是本产品唯一的同步后端——客户端不连接任何第三方云服务**。

## 架构

```
Rust 客户端 biji sync round
   │  默认 http://127.0.0.1:8332 + biji/biji（零环境变量开箱直连）
   │  UserStore.authenticate 换认证 token
   │  （BIJI_SERVER_URL / BIJI_SERVER_USER / BIJI_SERVER_PASS 覆盖）
   ▼
/edam/user   UserStore：checkVersion / authenticate / getNoteStoreUrl
/edam/note   NoteStore：getSyncState / getFilteredSyncChunk（分页）
                       createNote / updateNote / expungeNote
                       createNotebook / updateNotebook / expungeNotebook
                       createTag / updateTag / expungeTag
                       getResource / putResource（附件按需取体 / 独立上传）
                       getNote / findNotes / getNotebook / getTag
                       listNotebooks / listTags（读取 API）
                       listNoteVersions / getNoteVersion（版本历史）
   ▼
internal/store  内存模型 + JSON 落盘（updateCount 单调、每对象 USN、删除账）
```

- `internal/thrift`：Thrift 二进制协议编解码（严格版本头 `0x8001xxxx`、
  字段头 type(u8)+id(i16)、TFramed 请求自适응裸消息响应）。
- `internal/edam`：HTTP 路由与会话（账密 → token → NoteStore 鉴权）；
  `getNoteStoreUrl` 按请求 Host 返回绝对地址。
- `internal/store`：USN 记账与删除账，写操作后原子落盘。

## 线格式约定（与客户端镜像）

- 请求：`message f1 = args 容器 STRUCT`，容器内 `f1 = authenticationToken`，
  业务参数从 `f2` 起（如 createNote `f2 = Note{f2 title, f3 content, f9 active}`）。
- 响应：单层 result struct（`message f0 = STRUCT{业务字段}`）；
  updateNote / updateNotebook / updateTag / putResource 返回裸 `f0 i32/i64`，
  expunge 系列返回裸 `f0 i64`（与公开 EDAM IDL 规范一致）。
- getFilteredSyncChunk：`f2 afterUSN(i32) / f3 maxEntries(i32, 0=不限)` →
  `f2 chunkHighUSN(i32) / f3 updateCount(i64) /
  f4 notes list<STRUCT{f1 guid, f2 title, f3 content, f7 updated, f9 active}> /
  f5 notebooks（Notebook: f1 guid, f2 name, f5 usn, **f7 stack**）/ f6 tags（Tag: f1 guid, f2 name）/
  f11 expungedNotes / f12 expungedNotebooks / f13 expungedTags（list<string>）；
  **分页**：notes 按 USN 升序截断到 maxEntries，截断时 chunkHighUSN =
  最后一条已含笔记的 USN（< updateCount），客户端以该值游标续拉直至
  chunkHighUSN == updateCount；其余集合按 (afterUSN, chunkHighUSN] 过滤。
  list 元素不带字段头。
- 笔记本 stack（f7）：createNotebook / updateNotebook 携带 `f7 stack`——
  请求缺省 f7 = 保留现值，空串 = 清除分组；chunk 与 getNotebook 均回传。
- putResource（本后端扩展，独立上传附件，免去整笔记重推）：
  `f2 guid / f3 noteGuid / f4 Data{f1 body, f3 size} / f5 mime / f6 fileName`
  → 裸 `f0 i64 usn`。guid 与既有资源冲突且 noteGuid 不同时拒绝（返回 0）；
  客户端在资源已有服务器 guid 身份时走此快路径，否则回退整笔记重推。
- 读取 API：getNote（`f2 guid / f3 withContent / f4 withResourcesData`）、
  findNotes（`f3 filter{f1 words, f3 offset, f4 maxNotes}` →
  `f0 STRUCT{f1 totalNotes i32, f2 list{f1 guid, f2 title, f3 usn}}`，词间 AND）、
  listNotebooks / listTags（`f2` 缺省全量；list 直接在 `f0`）、
  getNotebook / getTag（`f2 guid`）、
  listNoteVersions（`f2 guid` → `f0 STRUCT{f1 totalVersions, f2 list{
  f1 usn, f2 updated, f3 title}}`）与 getNoteVersion（`f2 guid / f3 usn` →
  含正文的历史快照）。服务器保留每笔记最近 10 个版本快照。
- getResource：`f2 guid / f3 withBinary / f4 withData`（EDAM IDL 参数序）→
  `f0 STRUCT{f1 guid, f3 Data{f1 body, f3 size}, f4 mime, f14 attributes}`；
  `withBinary` 或 `withData` 任一为真时携带 body。

## 运行

```bash
go build -o biji-server-go.exe ./cmd/biji-server-go
./biji-server-go.exe -addr 127.0.0.1:8332 -data state.json
# 凭证经环境变量注入（默认仅供本机联调）：
#   BIJI_SERVER_USER / BIJI_SERVER_PASS（默认 biji/biji）
# 多用户隔离（逗号分隔，各用户独立 updateCount / 笔记空间 / 状态文件
# state.<user>.json，单用户时仍为 state.json）：
#   BIJI_SERVER_USERS="alice:a1,bob:b2"
# TLS（自签或正式证书均可，缺省明文 HTTP）：
#   ./biji-server-go.exe -cert cert.pem -key key.pem
```

客户端联调（默认值与服务端一致，**无需任何环境变量**）：

```bash
biji sync ping            # 免认证握手
biji sync round --db demo.exb   # 完整同步轮次（连接本服务端）
```

**GUI 同步入口**：`biji-gui.exe demo.exb` 启动后点工具栏「同步」按钮——
GUI 与 CLI 共用同一 `SyncEngine + ServerRemote::connect()` 链路，
直连本服务端；服务端未启动时状态栏给出联调指引（先启动 biji-server-go）。

UI 层实测（2026-10，Windows UIAutomation Invoke 点击）：未同步库启动 GUI →
点「同步(3)」→ 状态栏「同步完成：推送 3 条，拉取 0 条（服务器 USN 4）」、
角标「待同步 0」；服务器 state.json 收到笔记本「GUI验证」、笔记（带归属）、
附件 12799B（sha256 与本地一致）。

实测记录（2026-10）：全新库四轮验证——首轮推送 13 条（笔记本/标签/笔记，identity
回填服务器 guid）；次轮消化首轮收尾回声 2 条；第三、四轮幂等空轮。服务器重启后
从 `state.json` 恢复，新库 `sync round` 全量拉回笔记并可检索命中。

**笔记本归属闭环**：createNote/updateNote 携带 `Note.notebookGuid`（f11），
服务端记账并落盘；`getFilteredSyncChunk` 回传 `f4 notes`（含 f11 归属）与
`f5 notebooks`（笔记本本体），新端先对齐笔记本（guid ↔ 本地 uid，identity 登记）
再落笔记——多端同步后笔记本归属不丢。

**附件资源闭环**：createNote/updateNote 携带 `Note.resources`（f13，
Resource = f1 guid / f3 Data{f1 body, f3 size, f4 mime} / f4 mime /
f14 attributes{f1 fileName}，与公开 EDAM IDL 字段号一致）；资源随所属
笔记整体替换（请求不带 f13 时服务端保留现有资源集）；chunk 的 notes 内
回传 f13 **元数据态**（guid/mime/fileName + Data.size，无 body），大附件
body 经 `getResource` 按需拉取（EDAM 分页语义），客户端按 resource guid
幂等跳过已落库附件。附件变更（本地 journal 记
resource 条目）触发以所属笔记身份整体重推。Thrift STRING/BINARY 同类型
码，客户端解析层读原始字节（`TVal::Bytes`）保证二进制保真。

实测记录（2026-10）：12799B 任意字节二进制（含非法 UTF-8 序列）经
`res add` → `sync round` 上云，sha256 与本地一致；服务器重启后新库拉取，
`res extract` 导出 sha256 逐字节一致；次轮幂等空轮。

**大附件分页拉取实测（2026-10）**：A 端推送 32KB 二进制（0x00-0xFF 全字节
序列 ×128）→ B 端全新库 `sync round` 拉取——chunk 只带元数据，服务端日志
实证 `getResource` 恰好 1 次（B 端按需取体），B 端次轮幂等空轮且**不再发
getResource**（已落库附件按 guid 跳过重复下载）；`res extract` 导出 sha256
`e1136025…` 与 A 端原始逐字节一致。

**标签与删除账跨端闭环（2026-10）**：chunk 回传 f6 tags 与 f12/f13
笔记本/标签删除账——A 端 `tag add 重要` → C 端全新库 `sync round` 拉取
「标签 1 条」清单可见；A 端 `tag rm 重要`（删除账）→ C 端对账「删除 1 条」
标签移除。配套修复客户端**推送游标与服务器游标分离**（本地 journal usn 与
服务器回执 usn 在边界值碰撞时删除账曾漏推，现以独立 journal 水位推进，
回归测试锁定）。`renderPage`/`renderPages` 已在客户端 biji-service 落地
（渲染器接缝产出 PNG，CLI `biji res render <uid> --page N` 实测 poppler
25.07 渲染 928×1200 合法 PNG）。

**服务端补齐实测（2026-10，Go 7 项特性测试 + Rust 4 项锚定测试 + 真实进程 E2E）**：
笔记本 stack 经 createNotebook f7 上云、chunk 回传落库（缺省保留/空串清除）；
putResource 独立上传（含跨笔记 guid 冲突拒绝）；getFilteredSyncChunk
maxEntries=3 分 3 页拉全 7 条；alice/bob 双账号各自独立 updateCount 与
状态文件、互不可见；token 跨重启有效；findNotes/getNote/listNotebooks/
listTags/getNotebook 与笔记版本历史（含旧版本正文回读）全通；原子落盘无
`.tmp` 残留。真实进程 E2E：零配置启动 → `sync ping` 握手 → 推送笔记+附件 →
新库拉回附件逐字节一致；alice 推送 1 条后 bob 拉取 0 条（隔离）、alice
第二台新库拉回自己的 1 条；自签证书 `-cert/-key` 启动 TLSv1.3 握手成功；
推送 260 条后新库跨页拉取 261 条仅 0.77s。

## 安全与可靠性

凭证只从环境变量读（`BIJI_SERVER_USER/PASS`、多用户 `BIJI_SERVER_USERS`），
永不写入代码、日志或文档；状态文件只含笔记数据与**认证 token**，
不含任何密码——token 按用户持久化（重启不失效，客户端免重复登录），
重启后按用户索引恢复。落盘为**原子写**（先写 `*.tmp` 再 rename），
进程中断不会留下半截状态文件。
