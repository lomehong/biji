# biji 笔记

[![桌面客户端构建](https://github.com/lomehong/biji/actions/workflows/desktop.yml/badge.svg)](https://github.com/lomehong/biji/actions/workflows/desktop.yml)
[![Release](https://img.shields.io/github/v/release/lomehong/biji)](https://github.com/lomehong/biji/releases/latest)

独立实现的跨平台笔记应用：**Rust 桌面客户端**（egui/eframe）+ **自研 Go 同步服务端**。
同步协议为 EDAM 风格（Thrift 二进制 over HTTP），客户端只连接本仓库的
`biji-server-go`，不依赖任何第三方云服务。

## 下载

| 平台 | 产物 |
|---|---|
| Windows x64 | `biji-windows-x86_64.zip`（`biji-gui.exe` GUI + `biji.exe` CLI） |
| macOS Apple Silicon | `biji-macos-aarch64.zip`（`biji.app` + `biji` CLI） |
| macOS Intel | `biji-macos-x86_64.zip`（同上） |

到 [Releases](https://github.com/lomehong/biji/releases/latest) 下载对应平台 zip 解压即用
（macOS 的 `biji.app` 为 ad-hoc 签名，首次打开需在「系统设置 → 隐私与安全性」放行）。

## 功能总览

**桌面客户端**（`desktop/`，Rust workspace，详见 [desktop/README.md](desktop/README.md)）：

- 三栏界面：笔记本/标签/回收站侧栏 ↔ 笔记列表（标题+摘要+日期+标签）↔ 编辑器
- 富文本：ENML 2.0 存储（严格/宽松解析 + 白名单校验）；源码编辑格式工具栏
  （B/I/U/S/H1/H2/代码/待办/链接）；**渲染态直接选区操作**——预览态拖选文字
  直接施加 B/I/U/S/代码/链接并写回 ENML（所见即所得）
- 图注标注画布：箭头/矩形/文字/自由画笔，五色三档线宽，选择/移动/缩放/旋转，
  撤销/清空；标注写回图片资源（PNG/JPEG 原格式），PDF 按页标注另存
- 全文搜索、回收站（还原/彻底删除+删除账）、标签管理、附件导出、
  演示模式（全屏放映）、聊天/素材/企业空间面板（P3 功能）
- CLI（`biji`）：笔记/笔记本/标签/附件/同步/ENML 校验全套子命令，
  与 GUI 共用同一服务层

**同步服务端**（`biji-server-go/`，Go，详见 [biji-server-go/README.md](biji-server-go/README.md)）：

- EDAM 风格 Thrift over HTTP：UserStore（authenticate/checkVersion）+
  NoteStore（getSyncState/getFilteredSyncChunk 增量拉取/笔记·笔记本·标签
  CRUD 与删除账/getResource 附件按需取体）
- USN 记账（每对象独立 USN，updateCount 单调递增）、JSON 落盘、重启恢复
- 默认 `127.0.0.1:8332` + `biji/biji`，客户端零配置直连

**同步语义**（`biji-sync`）：完整轮次 pull → apply → push → commit；
guid ↔ 本地 uid 双向身份解析；删除账跨端对账（笔记/笔记本/标签）；
附件字节保真（sha256 校验）+ 大附件按需分页拉取；多端互见已实证。

## 快速开始

```bash
# 1) 启动同步服务端（默认 127.0.0.1:8332，账密 biji/biji）
cd biji-server-go && go build -o biji-server-go ./cmd/biji-server-go
./biji-server-go                      # 或 -addr/-data 自定义

# 2) 桌面客户端（源码构建）
cd desktop && cargo build --release -p biji-gui -p biji-cli
./target/release/biji-gui.exe         # GUI（工具栏「同步」直连服务端）
./target/release/biji.exe sync ping   # CLI：免认证握手验证连通
./target/release/biji.exe sync round  # CLI：完整同步轮次
```

源码构建要求：Rust stable（含 `cargo clippy`）与 Go ≥ 1.24。
Windows 渲染 PDF 标注需 poppler（`pdftoppm` 入 PATH 或设 `BIJI_PDF_RENDER_CMD`），
无渲染器时其余功能不受影响。

## 仓库结构

```
desktop/                 Rust workspace（桌面端）
├── crates/biji-enml/    ENML 2.0 解析/校验/清洗/序列化
├── crates/biji-store/   .exb 主库（SQLite，核心表 DDL 逐字段对位原客户端）
├── crates/biji-service/ DesktopService 28 方法 + NodeWebkitService 15 方法
├── crates/biji-sync/    USN 同步引擎 + EDAM Thrift 客户端（ServerRemote）
├── crates/biji-cli/     CLI（bin: biji）
└── crates/biji-gui/     GUI（egui/eframe：三栏/编辑/标注/演示/主题）
biji-server-go/          Go 同步服务端（Thrift 编解码 + HTTP 路由 + 存储）
.github/workflows/       CI：双平台构建 + tag 发布
```

## CI 与发布

推送到 `main` / PR 自动构建 Windows + macOS（arm64 原生、x86_64 交叉）客户端并
上传 artifact，原生目标跑全量测试；Go 服务端在 Linux 作业构建测试。

推送 `v*` tag 自动创建 GitHub Release 并挂载三平台 zip：

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## 质量与测试

- Rust：103 项单元/集成测试（ENML 往返、存储三处缓存一致、同步双客户端对账、
  标注几何/栅格化、渲染态选区映射等），`cargo clippy --all-targets` 零警告
- Go：Thrift 编解码、EDAM 路由/会话、存储记账测试全绿
- 端到端：真实服务端上验证过双客户端互见、删除账对账、附件 sha256 保真、
  getResource 恰好一次、幂等空轮

## 法律边界

本项目为独立实现的笔记应用，依据对协议行为与数据格式的静态分析结论重新实现，
未复制、未反编译引用任何第三方二进制代码、资源文件或明文 JS 产物。
ENML/DTD 为公开格式规范；所有第三方商标归其权利方所有。
