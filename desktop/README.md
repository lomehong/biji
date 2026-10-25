# biji — 自研笔记应用（Rust 桌面端 + Go 同步服务端）

依据对原 Windows 笔记客户端（v7.4.30.9439）的静态逆向分析报告（下称**分析文档**）独立实现的笔记应用工程。目标不是像素级仿制 UI，而是按分析文档 §9 路线图**从数据模型向上重建**：协议语义、本地存储、正文格式、同步记账全部可溯源到分析证据。

## 模块布局

```
crates/
├─ biji-enml      ENML 2.0 正文格式（解析/校验/清洗/纯文本/序列化）
├─ biji-store     本地存储（复刻 .exb 核心 schema + NOCASEUTF8 + 附件 blob 库 + USN 日志）
├─ biji-service   双服务语义（DesktopService 28 方法 + NodeWebkitService 15 命令）
├─ biji-sync      同步闭环（USN 记账 + 删除账 + EDAM 风格本地服务器 + 双向对账引擎）
├─ biji-cli       验收入口（bin: biji）
└─ biji-gui       桌面 GUI（bin: biji-gui，egui/eframe 纯 Rust 单壳）
```

## GUI（biji-gui）

分析文档 §9.1 的选型映射落地：原"WPF 外壳 + CEF + NW.js 三引擎"收敛为**单一 egui 壳**。GUI 同时扮演原架构两个角色——调用 `NoteService`（原生外壳职责），并以 `MarkupFrontend` 身份消费 `NodeWebkitCommand`（原 NW.js 编辑器职责，进程内通道替代命名管道）。

```bash
cargo run -p biji-gui -- [可选：库文件路径，默认 ./biji.exb]
```

已实现（截图实测验证）：
- **三栏布局**（对位原客户端）：笔记本/标签/回收站侧栏 ↔ 笔记列表（标题+摘要+日期+标签，摘要缓存按 USN 失效）↔ 编辑器
- **列表头排序与视图**（对位原客户端列表头，实机核验）：条数旁排序下拉（更新时间/创建时间/标题）+ 升降序切换 + 卡片/紧凑视图切换（紧凑 = 单行「● 标题 …… 日期」，查看菜单同入口）；排序维度计入列表签名（切换即重排并重置无限滚动窗口）
- **表格列宽拖拽调整**（对位原客户端表格列宽调节）：预览态表格各列右边界 5px 拖拽把手（Sense::click_and_drag），拖动实时更新列宽（最小 30px）并经 `request_widths` → `set_attr("widths","w1,w2,...")` 回写 ENML 源置脏；`widths` 属性在渲染时解析为 Grid 各列 `min_col_width`；把手矩形经 `PreviewSession.col_drag_rects` 透出供测试定位
- **编辑器撤销/重做**（对位原客户端 Ctrl+Z/Ctrl+Y）：EditorState 与 FloatNote 各自持有独立 undo/redo 栈（容量 50 封顶丢最旧）；源码模式经 prev_content 逐帧追踪——TextEdit changed 时旧值入栈、无变化时基线同步，撤销/重做恢复后基线自动更新（不会把恢复值再次入栈）；Ctrl+Z 撤销 / Ctrl+Y 或 Ctrl+Shift+Z 重做，主编辑器优先、栈空回落浮动窗口；纯函数 undo_push/undo_pop/redo_pop 单测覆盖容量封顶/互转/清空/栈空
- **粘贴图片自动落附件**（对位原客户端 Ctrl+V 截图落附件）：编辑器打开时 Ctrl+V → arboard 读取剪贴板图像 → RGBA→PNG 编码 → 复用拖放导入同一链路（add_resource 内容寻址 blob + journal + `<en-media hash type>` 占位入源置脏）；剪贴板为纯文本时不拦截（TextEdit 的 Paste 事件正常工作）；同 sha256 附件幂等去重
- **多窗口编辑器**（对位原客户端「在新窗口打开」双窗口对照，headless egui 集成核验）：笔记卡右键「🗔 在浮动窗口打开」——独立第二编辑会话（egui::Window：标题 + ENML 源码编辑 + 显式保存/关闭，脏标记），与主编辑器并行互不干扰（同一笔记已在主编辑器时提示不重复开）；保存走同一 update_note 链路各自落库
- **表格可视化编辑**（对位原客户端表格编辑，headless egui 集成核验）：预览态表格以 Grid 交互渲染——单元格点击就地编辑（缓冲聚焦输入、Enter/✓ 按钮提交写格）、行首 ✕ 删行、操作条「＋行/＋列/−列/−行」（行列删作用于末尾）；单元格与操作均为真实分配控件（Sense::click——网格内 Label+interact 叠加不参与命中测试，实测）；编辑会话经 EditorState 跨帧保持；写格/结构操作落 ENML 源（set_table_cell/apply_table_op 路径索引实现，裸 tr 与 thead/tbody 分节双形态统一），置脏走保存链路；交互矩形经 EditorState 透出供确定性测试
- **笔记内链接跳转**（对位原客户端 evernote:// 内部导航，headless egui 集成核验）：预览态 `<a href="biji://note/N">` 链接可点击——按屏幕矩形精确命中（单字链接也可靠，光标边界法对单字失真）、悬停显示手型；点击即打开目标笔记（编辑器同帧载入，状态栏反馈）；链接指向不存在的笔记时诚实提示且不跳转；链接区间随片段表在 InlineCtx 收集（char 域与渲染选区共用）
- **导出 PDF**（对位原客户端「导出 PDF」，零新依赖）：文件菜单「🖨 导出 PDF」→ ENML → 纯文本排版（ab_glyph 按字宽贪心换行，复用标注画布中文字体栅格化）→ A4@150dpi 白底位图分页（标题/正文两级字号、1.5 倍行距、自动分页）→ JPEG（image crate，质量 85）→ 手写最小 image-only PDF 容器（DCTDecode XObject + content stream + xref/trailer）；产物写入系统临时目录并在状态栏给出完整路径与页数；文件名按标题清洗非法字符。结构单测：多页分页、页对象计数、startxref 指向 xref 表、对象偏移逐条命中；headless 集成：文件菜单可达 + 同一导出方法产物字节合法（%PDF 头/页对象/%%EOF）
- **共享状态标识**（对位原客户端共享笔记标识，headless egui 集成核验）：笔记卡（卡片视图「🌐 共享」徽标 / 紧凑视图 🌐）与编辑器元信息行（「🌐 已共享」+ 共享/取消共享切换按钮）双向展示；ℹ 笔记信息面板含共享行；切换走 store `set_note_shared`（journal 记 note update——本地标识，同步轮次随整笔记重推记账）
- **附件拖拽导入**（对位原客户端拖文件入笔记，headless egui 集成核验）：编辑器打开时拖入文件 → 复用 add_resource 链路落资源（blob 内容寻址 + journal 记账）+ `<en-media hash type>` 占位插入正文末尾（置脏走自动保存/Ctrl+S）；按扩展名推断 MIME（png/jpg/gif/webp/bmp/svg/pdf/txt/md/mp3/mp4/zip，缺省 octet-stream）；同字节附件幂等去重（拖放事件可跨帧重复上报）；未开编辑器时状态栏指引
- **编辑器查找替换**（对位原客户端 Ctrl+F，headless egui 集成核验）：编辑器打开时 Ctrl+F 唤起查找栏（预览态自动切源码保证光标定位；无编辑器时仍聚焦列表搜索）；大小写不敏感命中计数 `n/m`，「下一个」循环推进并在源码中定位选中当前命中（Enter 连跳），「替换」替换当前命中、「全部替换」整篇替换（作用于 ENML 源，置脏走自动保存/Ctrl+S 落库），Esc 关闭；find_hits/replace_all 纯函数单测（大小写、空查询、相邻不重叠）
- **标签输入自动补全**（对位原客户端，headless egui 集成核验）：「🏷 添加标签…」菜单顶部搜索框，输入按前缀（忽略大小写）过滤既有标签候选，点击即补全挂载并清过滤；未命中的输入出现「＋ 新建「××」」一键创建并挂载（排除已挂与已存在同名）
- **清单多选与批量操作**（对位原客户端列表多选，headless egui 集成核验）：普通点击单选（清多选）；Ctrl+点击切换多选成员；Shift+点击锚点区间全选（顺序随列表序）；多选卡片高亮；多选态列表面板顶部浮现批量操作条——「已选 N 条」+「📦 移动到…」（弹层选各笔记本/未分类）+「🗑 移入回收站」（复用 move_note/trash_note 链路）；Delete 批量移入回收站、Esc 退出多选；切换视图/搜索自动清多选
- **列表头来源切换下拉**（对位原客户端，headless egui 集成核验）：点击「{来源} ▼」标题弹来源菜单——全部笔记 / 各笔记本（📓）/ 各标签（🏷），选中即切列表来源（同步清搜索、退出回收站视图），替代侧栏点选的唯一入口
- **全局快捷键**：Ctrl+N 新建、Ctrl+F 聚焦搜索（与 F6 等效）、Ctrl+S 保存、Delete 移入回收站（列表选中时；文本编辑聚焦中不抢占按键）
- **笔记信息面板**（ℹ，实机核验）：创建/更新时间、正文字数、笔记本、标签、来源 URL 两栏信息窗
- **深色模式**（对位原客户端深色配色，双主题）：`theme.rs` 重构为 `Palette` 双套（浅色逐值保持既有实测取色；深色 = 近黑底 `#1E1E20` + 深灰侧栏 `#232326` + 卡片 `#2A2A2E` + 浅色正文 + 亮一档品牌绿/链接蓝），`pal()` 进程级原子切换；查看菜单「🌙 深色模式」与菜单栏快捷 ☀️/🌙 钮同入口；选择持久化库 meta（key=`theme`），重启恢复。实机像素取证：预置 dark 的库启动即深色渲染（主底 73.5%/侧栏 12.3%/卡片 4.2% 命中调色板）；headless egui 测试核验 菜单切换→visuals/panel_fill/window_fill/meta 四路生效 + 重启恢复 + 切回持久化
- **快捷方式（侧栏置顶笔记）**（对位原客户端 Shortcuts）：笔记卡右键「📌 添加/取消快捷方式」；侧栏快捷方式分区（可折叠）——点击直达笔记（自动切回全部视图并打开），行右键取消置顶；置顶 uid 存库 meta（key=shortcuts），回收站/彻底删除的笔记自动从分区过滤（还原后恢复显示）
- **右键上下文菜单**（对位原客户端，headless egui 集成测试全链路核验）：侧栏笔记本行右键 → 重命名（对话框）/删除（确认）；侧栏标签行右键 → 重命名/删除（确认；解除全部笔记关联 + 删除账跨端对账）；笔记卡右键（卡片与紧凑两视图）→ 移入回收站 / 📌 快捷方式 / 移动到笔记本（平铺各笔记本 + 未分类，直列目标比悬停子菜单更可用）。配套 store 层 `rename_notebook`/`move_note`/`rename_tag`（journal 记 update，重命名与归属变更随同步轮次上云：updateNotebook/updateTag 新名 / updateNote 携带 notebookGuid），移动双向刷新两侧笔记本计数，标签重命名同步刷新关联笔记的 tags 派生文本
- **图注标注画布**（分析文档 P2，原 `markup.min.js` 的复刻）：图片附件一键打开标注画布（完整 Markup 命令通路：按钮 → `NoteService.dispatch` → 前端通道 → 画布）；箭头/矩形/文字三工具 + 五色 + 三档线宽 + 撤销/清空/旋转 90°；标注以图片像素坐标存储（窗口缩放无关），双渲染器共享模型——egui painter 实时叠加（编辑态）+ 软件栅格化（`ab_glyph` 中文文字光栅化）合成 PNG 经 `update_resource` 写回资源（原 `updateResource` 语义）。`RotateImage`（90° 倍数）/`ResizeImage`（Lanczos3）/`ClearImage` 命令同步真实现
- **编辑器**：标题编辑 + ENML 源码编辑 / 富文本预览双模式；2 秒无输入自动落库；Ctrl+S 手动保存；源码模式格式工具栏 B/I/U/S/**高亮**/H1/H2/代码/**表格**（`<table><tr><td>` 骨架）/**分割线**（`<hr/>`）/待办/链接（作用于光标/选区，包裹后恢复光标位置；无光标时插到 `</en-note>` 前——修复旧实现插到文档头破坏 XML 声明被 sanitize 清空的问题）；预览层裸行表格（tr 直挂 table）与 hr 渲染补齐（此前裸行被静默丢弃）
- **预览态行内块编辑**（所见即所得切片，格式保留）：预览中每个顶层块旁带「✎」按钮，点击以该块内 HTML 片段进入就地多行编辑（**带格式工具栏**：B/I/U/S/代码/链接 六键作用于编辑缓冲选区——所见文本直接施加标记，无需理解 HTML 源码），完成/取消就地生效；提交按 HTML 解析该片段（sanitize 后 splice 回 ENML 树）并走既有自动保存链路——块内粗斜体/链接等标记得以保留
- **渲染态直接选区操作**（所见即所得核心，2026-10 实机核验）：预览渲染文字可直接**拖拽框选**——行内富文本块（段落/标题/行内标签块）经自研可选 galley 组件渲染（`selectable_block`：`LayoutJob` 全 run 不裁剪构建保证 char 域与片段表逐字对齐，`Galley::cursor_from_pos` 光标命中、逐字 `pos_from_ccursor` 选区高亮铺底 + `painter.galley` 文字叠加、`Sense::click_and_drag` 拖拽状态机：按下置锚点→拖动动焦点→单击清除）；持有选区时浮现「选区格式：B/I/U/S/代码/链接」工具栏，点击即经**片段表映射**（`Piece { char_start/len, path }`——每个纯文本片段记录其在 ENML 树中的节点路径）把选区翻译为文本节点编辑列表（`selection_edits`），`apply_format_edits` 逆序拆分文本节点为 `[前文, <tag>选中</tag>, 后文]` 插入标记元素（跨兄弟/嵌套文本节点选区逐片段各生成区间，逆序应用规避下标漂移），sanitize 后写回 ENML 走自动保存。实机核验：预览态拖选第一段 → 工具栏出现 → B 键 → `<p>渲染态直接选<b>区操作验证文</b>本</p>` 落库；第二段拖选 → I 键 → `<i>` 同链路闭环
- **富文本预览**：`LayoutJob` 多样式段落——标题分级/粗斜下划线/删除线/行内代码/引用块/代码块/有序无序列表/`en-todo` 待办复选框/`en-media` 附件占位/`en-crypt` 锁定占位/表格扁平化
- **演示模式**：`Present` 命令驱动深色全屏放映（居中大标题 + 纯文本正文 + ←/→ 翻页 + Esc 退出），对位原 `present/` 子应用
- **标注进阶**：自由画笔（拖拽轨迹折线采样，双渲染器与箭头/矩形/文字一致）、选择/移动/多选/缩放/旋转（⬚ 选择工具：点击选中、Shift 加选、空白处框选、拖拽整体平移、🔍±等比缩放、删除所选；控制把手三级——角点黄=对角锚等比缩放、边中点青=单轴缩放、顶边外移紫点=旋转且枢轴可切换（公共包围盒中心/画布中心外部点，`start_rotation` 参数化入口）；`Annotation.angle` 双渲染器旋转——paint 侧旋转文字经离屏栅格化 + 旋转 Mesh 贴回（绕开 egui TextFormat 无字形旋转限制），rasterize 侧字形像素逐点旋转含 `px_bounds` 偏移修复）、PDF 附件一键进入按页标注（保存产物为新 PNG 资源挂同一条笔记，PDF 本体不改写——对位原客户端"标注层另存"）、JPEG 原格式写回（image/jpeg 资源标注后保持 JPEG 编码与文件名）
- **P3 GUI 入口**：「💬 聊天」窗口（会话选择/新建、消息列表按时间序、发送输入行——消费 `create_thread`/`post_message`/`list_messages`）；「📦 素材」面板（素材清单 + 一键登记当前笔记附件为素材——消费 `add_material`/`list_materials`）；「🏢 企业空间」面板（空间创建（名称/描述）、列表与成员计数展示、一键以当前用户身份加入——消费 `create_groupspace`/`join_groupspace`/`list_groupspaces`）
- **特征功能**：全文搜索（标题+正文子串）、回收站（还原/彻底删除+删除账）、笔记本管理（新建/删除级联回收站）、标签过滤、附件导出、账号显示（`user#host` 模型）、同步计数与全轮次同步
- **CJK 字体**：启动时按候选加载系统字体（msyh → simhei → simsun → Deng），egui 默认字体无汉字字形；msyh（微软雅黑，原客户端 UI 字体）优先且插队首，拉丁/数字同样走雅黑
- **视觉主题还原**（`theme.rs` 双主题 `Palette`，浅色对位原客户端实机截图的程序化取色）：浅色 UI——品牌绿 `#00B870`（新建/同步药丸、侧栏选中态文字、列表日期）、侧栏浅灰 `#ECECF0` + 选中项深灰底绿字、笔记列表白卡圆角发丝边（选中 = 亮绿描边 `#3EE87C` + 浅绿底 `#EEFEF5`，整卡可点）、编辑器纯白无边框大标题、链接经典蓝下划线；深色模式见上方专条；标注画布窗口局部深色化（`apply_dark`，对位原 markup 编辑器深底工作区，下帧自动恢复**当前**主题）；演示模式保持深底（原放映子应用同款）；缺字形图标（⬤/⬚/➚/▭/✎/⟳/🔍 在雅黑下为豆腐块）统一替换为安全字形（●/◇/↗/□/✏️/文字/＋－）
- **结构还原（第二轮）**：菜单栏行（文件/编辑/查看/笔记/格式/工具/帮助 七个真菜单——新建/同步/退出/清搜索/演示/删当前笔记/双模式切换/聊天/素材/企业空间/关于，菜单按钮扁平纯文字 hover 显底）；侧栏顶部「白药丸 + 绿圆＋」新建笔记组合控件 + 企业空间导航行；搜索框归位列首（`搜索笔记 (F6)`，F6 聚焦实测）；列表头 `全部笔记 ▼` + `N条笔记` 原版格式；编辑器元信息行（笔记本 chip 点击跳转侧栏 + 更新日期）+ 标签行（绿色「🏷 添加标签…」下拉真挂载 + 已挂标签 chip 带 ✕ 摘除，`biji-store::note_tags` 实时读关系表，CLI 侧核验计数/缓存三处同步）
- **精细打磨（第三轮）**：全局字号比例对位原客户端（正文 15 / 标题 24 / 按钮 14 / 小字 12，egui 默认 12.5/18/12.5/9）；笔记卡日期改原版短格式 `YYYY/M/D`（无补零，编辑器元信息仍为完整日期时间）；侧栏「笔记本/标签」分区可折叠（▼/▶ 手风琴，实测折叠隐藏/展开恢复）；滚动条细化 6px
- **对齐与主题修正（第四轮）**：向 OS 声明浅色主题偏好（`ViewportCommand::SetTheme(Light)`，Windows 标题栏不再跟随系统深色）；侧栏导航行/分区头全部改为**自绘左对齐控件**（egui Button 默认文字居中导致导航行与分区头错位）——图标恒定 x+10、文字恒定 x+30 两条对齐线，八行实测文字起点收敛 ±1px；面板分隔线取 `noninteractive.bg_stroke` 改发丝线色；定位并修掉一个隐性布局缺陷：`add_sized` 在 horizontal 内派生 top_down 子区会把面板右缘撑出贯穿全高的黑带（二分五轮锁定），「＋」改自绘＋interact 实现

GUI 实机验证记录（cua-driver 截图核验）：三栏布局 ✓、中文渲染 ✓、笔记点击→编辑器+预览 ✓、`▶ 演示` → 全屏放映 ✓（即原双 Thrift 通路的进程内闭环：按钮 → `NoteService.dispatch(Present)` → 前端通道 → GUI 消费）、预览态拖选→选区格式工具栏→B/I 键→ENML `<b>`/`<i>` 写回落库 ✓（渲染态直操全链）。
视觉主题实机对照 ✓：以原客户端实机截图（登录态 v7.4.30.9439）为基准程序化取色 → biji-gui 重启对照截图——绿药丸新建/同步、灰底侧栏选中态绿字、白卡列表选中亮绿边+浅绿底、绿日期、预览页签绿选中态、深色标注工作区（工具字形与色点全部正常渲染）逐项核对一致；标注交互（前台拖拽箭头、状态栏计数）在新主题下回归通过。
图注标注端到端实测 ✓：PNG 附件 → 标注按钮 → 画布打开（工具栏/颜色/线宽齐全）→ 前台拖拽绘制箭头（状态栏「标注 1 条」）→ 保存写回 → 资源 1871B→5536B → 独立解码 PNG 在拖拽对应图像坐标检出红色箭头像素。

PDF 按页标注（markupPdf）端到端实测 ✓（poppler 25.07.0，`pdftoppm` 入 PATH）：2 页手写 PDF 附件（1170B，`pdfinfo` 验证合法）→ 附件条「🏷 标注 test-doc.pdf」→ `MarkupPdf(page 1)` → `detect_renderer()` 命中 pdftoppm **按页渲染**（画布标题「test-doc.pdf · 第 1 页」，页面显示第 1 页文字实证按页取页）→ 前台拖拽绘制红色箭头（状态栏「标注 1 条」）→ 💾保存写回 → 新资源 `test-doc-p1-标注.png`（88217B，image/png）挂回同一条笔记 → `res extract` 独立解码核验：完整 150dpi 第 1 页渲染 + 箭头正落拖拽路径。验证数据与脚本在 `spike/demo-pdf/`（`make-test-pdf.py` 可复现 fixture）。

测试：`biji-gui` 含 6 个逻辑测试（日期格式化/前端通道/列表失效/ENML 行内样式切段）；eframe `App` 本体依赖显示器，不在 CI 断言范围。

## 分析文档 → 实现 追溯表

| 分析文档章节 | 原系统事实 | 本实现 | 位置 |
|---|---|---|---|
| §5 传输层 | Thrift over 命名管道（标注编辑器管道 / 壳↔编辑器 NW 管道） | 进程内调用：`NoteService` 方法 + `NodeWebkitCommand`→`MarkupFrontend::dispatch`（`biji markup` 可演示命令流） | `biji-service/src/lib.rs` |
| §5.1 结构体 | `NwNote`/`NwResource`/`NwResourceBase64`/`Display` | `NoteView`/`ResourceData`/`Base64Resource`/`DisplayInfo`（字段一一对应） | `biji-service` |
| §5.2 DesktopService 28 方法 | 编辑器→壳 | 28 个方法全部有签名级映射且**全部真实现**：笔记/资源/锁/账号/试用计数/事件回报、分段写回（`updateResource` offset 定位拼接/`createNoteFromResource` 分段装配幂等）、PDF 分页渲染（`renderPage`/`renderPages` 经渲染器接缝产出 PNG 文件，与 GUI 标注画布共享 `BIJI_PDF_RENDER_CMD`→pdftoppm 约定，CLI `res render` 可实测） | `biji-service::NoteService` |
| §5.3 NodeWebkitService 15 方法 | 壳→编辑器 | 15 个变体的枚举 + LoggingFrontend | `biji-service::NodeWebkitCommand` |
| §6.1 账号模型 | `user#service_host`；bootstrap 双 profile | `account set/show`；`AnalyticsInfo` 默认值 = 中国区 profile 实测开关 | `biji-service` |
| §6.2 云端 | 原厂托管服务端 | 协议对位但不连官方云：同步目标为自研后端 biji-server-go（`ServerRemote`，默认 `http://127.0.0.1:8332` 零配置直连） | `biji-sync` |
| §7.1 目录布局 | `<名>.exb` + `.exb.attachments` | `biji init` 产出 `biji.exb` + `biji.exb.attachments`；blob 两级哈希寻址、内容去重 | `biji-store::attachments_dir_for` |
| §7.2 格式结论 | `.exb`=SQLite；`NOCASEUTF8` 需应用注册 | rusqlite(bundled SQLite) + 打开时注册同名 collation（测试覆盖大小写折叠排序） | `biji-store::nocase_utf8` |
| §7.3 数据模型 | 核心四表（`note_attr`/`notebook_attr`/`tag_attr`/`resource_attr`，DDL 逐字段恢复）+ `expunged_items` | 四表 DDL **逐字段复刻**（含 `COLLATE NOCASEUTF8` 列）；`note_content`/`note_tag`/`sync_journal`/`meta` 为标注的扩展表（原存放点未恢复，分析文档 §12） | `biji-store::SCHEMA` |
| §7.3 标签三处维护 | `note_tag` 关系 + `tag_attr.note_count` + `note_attr.tags` 文本缓存 | `attach_tag`/`detach_tag` 三处同步维护（测试覆盖） | `biji-store` |
| §7.3 删除账 | `expunged_items`（同步必需） | 彻底删除即记账；`biji note rm --hard` + `sync status` 可对账 | `biji-store` + `biji-sync` |
| §9.1 正文格式 | ENML 2.0 公开 DTD | `biji-enml`：严格/宽松解析（真实笔记的 HTML 命名实体、碎片、DOCTYPE 全兼容）、白名单校验、script 剪除、纯文本提取 | `biji-enml` |
| §9.3 风险 2 | USN/CSN 计数模型 | 每次变更加 USN 日志；**完整同步轮次 `sync_round`：pull → apply → push → commit**；`PushPlan` 含变更快照与删除账 | `biji-sync` |

## 同步闭环（自有后端 biji-server-go + 本地 EDAM 风格演示）

`biji-sync` 的同步目标分两层：**产品层 = 自研 Go 后端**（`ServerRemote`，真实 HTTP 传输），**演示层 = 进程内 EDAM 风格服务器**（离线演示与测试闭环）：

- **`ServerRemote`（产品同步目标）**：EDAM Thrift over HTTP 客户端（`biji-sync::edam`），连接自研后端 `biji-server-go/`——默认 `http://127.0.0.1:8332` + `biji/biji` 零配置直连（`BIJI_SERVER_URL/USER/PASS` 覆盖；服务端 `BIJI_SERVER_USERS` 多用户时逐用户独立空间），UserStore.authenticate 换认证 token（服务端按用户持久化，重启免重登）；已实证附件字节保真、getResource 恰好一次、标签/删除账跨端对账、笔记本 stack（f7）往返、putResource 附件快路径（有资源 guid 身份时免整笔记重推）、maxEntries 分页游标续拉（261 条跨页 0.77s 实测）；
- **`MockEdamServer`**：`update_count` 单调递增、每对象独立 usn、expunged 删除账、guid 铸造——与 EDAM 增量拉取模型一致（`--demo` 离线演示）；
- **`SyncRemote` 双向协议**：`upload(plan, identities)`（guid ↔ 本地 uid 身份解析/铸造，服务器回执分配表）+ `pull(after_usn)`（增量笔记与删除账）+ `fetch_resource(guid)`（chunk 只带附件元数据时经 getResource 按需取 body，已落库附件按 guid 幂等跳过重复下载）；`LocalEdamRemote` 为 mock 侧实现；
- **`SyncEngine::sync_round`**：pull → apply（`apply_remote_note`/`apply_remote_expunge`，不写本地日志，天然无回声）→ push → commit（同步点只前进不回退）；
- **对账测试覆盖**：双客户端推拉、更新对账、彻底删除经删除账对账且新客户端绝不复活、双向变更、空轮次幂等、同步点不回退回归、**标签与笔记本/标签删除账跨端传播**、**推送游标与服务器游标分离（本地 journal usn 与服务器 usn 边界碰撞不漏推）**、**笔记本 stack wire 往返（f7）**、**分页 chunk 游标循环**、**putResource 快路径优先（有身份不整推）**。

```bash
biji sync ping            # 与自有后端免认证握手（验证 Thrift 传输）
biji sync round           # 与自有后端跑完整轮次（默认 127.0.0.1:8332，零环境变量）
biji sync round --demo    # 离线演示：与进程内 mock 服务器跑完整轮次
biji sync status          # 同步点与待推送清单
```

双客户端互见（真实后端）：启动 `biji-server-go`（默认地址/账密与客户端默认一致）→ A 端 `biji note add` → `biji sync round` → B 端 `biji sync round` 即拉到 A 的笔记；彻底删除经删除账对账后对端同步移除。

## 构建与验收

```bash
cargo build                      # 或 cargo test 全量测试（35 个）
cargo clippy --workspace --all-targets   # 零警告

# 端到端冒烟（与真实仓库操作完全一致）
biji init                                   # biji.exb + biji.exb.attachments
biji nb add 工作 --stack 公司
echo 正文 | biji note add --title 首页 --notebook 工作   # 输出 biji://note/1
biji tag add 重点 && biji tag attach 重点 1
biji note show 1                            # 纯文本渲染
biji search 正文
echo 数据 > f.txt && biji res add 1 f.txt && biji res extract 1
biji sync status && biji sync push --demo   # USN 闭环（真实 EDAM 见下）
biji markup 1                               # NodeWebkit 命令通路演示
biji enml validate note.enml                # DTD 白名单校验
```

## CI（GitHub Actions，`.github/workflows/desktop.yml`）

推送到 `main` / 提 PR 即触发，双平台构建桌面客户端并上传产物（Actions 页「Artifacts」下载）：

| 产物 | 运行器 | 目标 | 内容 |
|---|---|---|---|
| `biji-desktop-windows-x86_64` | windows-latest | x86_64-pc-windows-msvc（原生） | `biji.exe` + `biji-gui.exe` + README，zip |
| `biji-desktop-macos-aarch64` | macos-latest | aarch64-apple-darwin（原生） | `biji.app`（ad-hoc 签名）+ `biji` CLI + README，zip |
| `biji-desktop-macos-x86_64` | macos-latest | x86_64-apple-darwin（交叉编译） | 同上（Intel Mac） |

原生目标同时跑 `cargo test --workspace` 全量测试；`biji-server-go` 在 Linux 作业里构建 + 测试。
中文字体按平台加载（Windows：msyh 系；macOS：PingFang SC/冬青黑体/Arial Unicode；Linux：Noto CJK）。

**发布 Release**：推送 `v*` tag（如 `git tag v0.1.0 && git push origin v0.1.0`）即触发发布流——
全平台构建通过后自动创建 GitHub Release（自动生成变更说明）并挂载三平台 zip，
 Releases 页可直接下载，无需登录 Actions。

## 尚未复刻（与分析文档 §12 对应，持续收敛中）

1. **PDF 渲染质量上限**（已基本收敛）——按页渲染标注闭环已用 poppler 25.07.0 实测打通（见上方 GUI 验证记录）；残余仅为渲染保真度上限取决于外部渲染器二进制（pdftoppm 150dpi 光栅已满足标注用途，矢量/字体级高质量渲染可经 `BIJI_PDF_RENDER_CMD` 接入任意渲染命令）。
2. **旁路库格式**（.gdb/.tdb，原私有格式）——不做，复刻自有实现无此包袱。
3. **富文本编辑深化（已全部收敛）**——源码编辑模式的**格式工具栏**（B/I/U/S/H1/H2/代码/待办/链接 九键，`wrap_enml_around_selection` 6 组单测）、**预览态就地编辑格式工具栏**（B/I/U/S/代码/链接作用于片段缓冲选区，2026-10 实机核验：✎ → B 键 → 缓冲出现 `<b>` 包裹 → ✓ 完成 → ENML 落库自动保存全链闭环）与**渲染态直接选区操作**（预览渲染上直接拖选文字施加标记——自研 galley 光标命中 + 片段表标记树映射 + 文本节点拆分，2026-10 实机核验 B/I 两标记写回 ENML，45 项单测含嵌套节点/跨兄弟节点/CJK 安全/带属性链接）均已实现，无剩余项。
4. **标注旋转深化（已收敛）**——把手旋转（公共中心/外部点枢轴可切换）、paint 侧旋转文字（离屏栅格化 Mesh）、**绕外部点的标注轨道位移**（`rotate_around_canvas` 开启时移动拖拽变为绕画布中心的轨道变换：坐标经 rotate_point 刚体旋转、自身 angle 跟随，跨 ±180° 边界归一）均已实现；剩余仅 paint 侧文字字形直绘（egui TextFormat 限制，保存路径已支持）。

## 法律边界

本项目为独立实现的笔记应用，依据对协议行为与数据格式的静态分析结论重新实现，未复制、未反编译引用任何第三方二进制代码、资源文件或明文 JS 产物。ENML/DTD 为公开格式规范；所有第三方商标归其权利方所有。
