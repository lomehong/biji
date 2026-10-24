# biji-server-go

自研笔记服务端：用 Golang 实现 EDAM（Thrift 二进制 over HTTP）同步子集，
对接 Rust 复刻客户端（`biji-cli` / `biji-gui` 的 `biji-sync::edam`），
**替代对印象笔记服务端与开发者 token 的依赖**。

## 架构

```
Rust 客户端 biji sync round
   │  BIJI_EDAM_HOST=http://127.0.0.1:8332
   │  BIJI_EDAM_USER / BIJI_EDAM_PASS（authenticate 换 token）
   ▼
/edam/user   UserStore：checkVersion / authenticate / getNoteStoreUrl
/edam/note   NoteStore：getSyncState / getFilteredSyncChunk
                       createNote / updateNote / expungeNote
                       createNotebook / updateNotebook / expungeNotebook
                       createTag / updateTag / expungeTag
                       getResource（附件按需取 body）
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
  updateNote / updateNotebook / updateTag 返回裸 `f0 i32`，
  expunge 系列返回裸 `f0 i64`（与印象笔记 EDAM IDL 一致）。
- getFilteredSyncChunk：`f2 chunkHighUSN(i32) / f3 updateCount(i64) /
  f4 notes list<STRUCT{f1 guid, f2 title, f3 content, f7 updated, f9 active}> /
  f11 expungedNotes list<string>`；list 元素不带字段头。
- getResource：`f2 guid / f3 withBinary / f4 withData`（EDAM IDL 参数序）→
  `f0 STRUCT{f1 guid, f3 Data{f1 body, f3 size}, f4 mime, f14 attributes}`；
  `withBinary` 或 `withData` 任一为真时携带 body。

## 运行

```bash
go build -o biji-server-go.exe ./cmd/biji-server-go
./biji-server-go.exe -addr 127.0.0.1:8332 -data state.json
# 凭证经环境变量注入（默认仅供本机联调）：
#   BIJI_SERVER_USER / BIJI_SERVER_PASS（默认 biji/biji）
```

客户端联调：

```bash
BIJI_EDAM_HOST=http://127.0.0.1:8332 \
BIJI_EDAM_USER=biji BIJI_EDAM_PASS=biji \
biji sync round --db demo.exb
```

**GUI 同步入口**：`biji-gui.exe demo.exb` 启动后点工具栏「同步」按钮——
客户端按环境凭证自动选择远端（`BIJI_EDAM_TOKEN` 或 `BIJI_EDAM_USER/PASS`
齐备走本服务端，否则内存 mock 演示闭环）；GUI 与 CLI 共用同一
`SyncEngine + remote_from_env` 链路。

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
f14 attributes{f1 fileName}，与印象笔记 EDAM IDL 字段号一致）；资源随所属
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

## 安全

凭证只从环境变量读（`BIJI_SERVER_USER/PASS`），token 仅存内存会话表，
永不写入代码、日志或文档；state.json 只含笔记数据，不含凭证。
