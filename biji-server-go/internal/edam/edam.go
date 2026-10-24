// Package edam 实现 EDAM 风格 Thrift over HTTP 的服务端路由：
// 解析客户端严格 Thrift 二进制请求，分发到 store，编码 Thrift 响应。
//
// 多用户：BIJI_SERVER_USERS="u1:p1,u2:p2" 启用多账号——每用户独立 Store
// （独立 updateCount/数据文件/会话 token）；未设置时保持单用户行为
// （BIJI_SERVER_USER/PASS，默认 biji/biji）。token 随各用户状态落盘，重启不失效。
package edam

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"log"
	"net/http"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"biji-server-go/internal/store"
	"biji-server-go/internal/thrift"
)

func timeNow() time.Time { return time.Now() }

func newToken() string {
	b := make([]byte, 16)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

// Server EDAM 服务端（可承载多用户）。
type Server struct {
	Store     *store.Store // 单用户模式的store（多用户模式下为 nil）
	User      string       // 单用户模式的认证用户名
	Pass      string       // 单用户模式的认证密码
	mu        sync.Mutex
	users     map[string]string      // 用户 → 密码（多用户模式）
	stores    map[string]*store.Store // 用户 → Store（懒加载）
	userFor   map[string]string       // token → 用户（内存索引，落盘源在各 Store.Tokens）
	dataPath  string                  // 单用户状态文件 / 多用户状态文件模板
	noteStore string                  // getNoteStoreUrl 返回值路径前缀
}

// New 创建单用户服务端（向后兼容）；noteStoreURL 为 NoteStore 路径前缀
// （按请求 Host 拼绝对地址），dataPath 为写操作后的状态落盘文件。
func New(st *store.Store, user, pass, noteStoreURL, dataPath string) *Server {
	return &Server{
		Store:     st,
		User:      user,
		Pass:      pass,
		users:     map[string]string{user: pass},
		stores:    map[string]*store.Store{user: st},
		userFor:   map[string]string{},
		dataPath:  dataPath,
		noteStore: noteStoreURL,
	}
}

// NewMulti 创建多用户服务端：每用户独立状态文件（由 dataPath 模板派生：
// state.json → state.<user>.json），首次访问懒加载。
func NewMulti(users map[string]string, noteStoreURL, dataPath string) *Server {
	return &Server{
		users:     users,
		stores:    map[string]*store.Store{},
		userFor:   map[string]string{},
		dataPath:  dataPath,
		noteStore: noteStoreURL,
	}
}

// statePathFor 用户的状态文件：单用户为 dataPath 原样；多用户为
// <dir>/<base>.<user>.<ext>（state.json → state.alice.json）。
func (s *Server) statePathFor(user string) string {
	if s.Store != nil { // 单用户模式
		return s.dataPath
	}
	ext := filepath.Ext(s.dataPath)
	base := strings.TrimSuffix(s.dataPath, ext)
	return fmt.Sprintf("%s.%s%s", base, user, ext)
}

// storeFor 取（必要时懒加载）用户的 Store。
func (s *Server) storeFor(user string) (*store.Store, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.Store != nil {
		return s.Store, nil
	}
	if st, ok := s.stores[user]; ok {
		return st, nil
	}
	st, err := store.Load(s.statePathFor(user))
	if err != nil {
		return nil, err
	}
	s.stores[user] = st
	// 恢复该用户已落盘的会话 token 索引
	for tok := range st.Tokens {
		s.userFor[tok] = user
	}
	return st, nil
}

// reply 编码 Thrift REPLY：结果字段 0 = struct。
func reply(method string, seq int32, fields func(w *thrift.Writer)) []byte {
	var w thrift.Writer
	w.MessageBegin(method, thrift.Reply, seq)
	w.FieldBegin(0, thrift.TypeStruct)
	fields(&w)
	w.FieldStop()
	w.U8(0)
	return w.Buf
}

// replyRaw 编码顶层字段，不包 result struct（EDAM 的 i32/i64 直接返回值）。
func replyRaw(method string, seq int32, fields func(w *thrift.Writer)) []byte {
	var w thrift.Writer
	w.MessageBegin(method, thrift.Reply, seq)
	fields(&w)
	w.FieldStop()
	w.U8(0)
	return w.Buf
}

// exception 编码 TApplicationException（字段 1 = 文本）。
func exception(msg string) []byte {
	var w thrift.Writer
	w.MessageBegin("TApplicationException", thrift.Exception, 0)
	w.FieldBegin(1, thrift.TypeString)
	w.String(msg)
	w.FieldStop()
	w.U8(0)
	return w.Buf
}

// serveUserStore 处理 /edam/user。
func (s *Server) ServeUserStore(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "read body", http.StatusBadRequest)
		return
	}
	rd := &thrift.Reader{B: body}
	name, _, seq, err := rd.Message()
	if err != nil {
		w.Write(exception(err.Error()))
		return
	}
	raw, err := rd.Fields()
	if err != nil {
		w.Write(exception(err.Error()))
		return
	}
	// Rust rpc 把参数包在 message f1 的 args 容器 STRUCT 内，先下钻
	args := raw
	for _, f := range raw {
		if f.ID == 1 {
			args = f.Val.Struct
			break
		}
	}
	log.Printf("[edam] %s args=%+v", name, args)
	str := func(id int16) string {
		for _, f := range args {
			if f.ID == id {
				if f.Val.Str != "" {
					return f.Val.Str
				}
			}
		}
		return ""
	}
	var out []byte
	switch name {
	case "checkVersion":
		out = reply(name, seq, func(w *thrift.Writer) {
			w.FieldBegin(0, thrift.TypeBool)
			w.U8(1) // 兼容
			w.FieldStop()
		})
	case "authenticate":
		user, pass := str(1), str(2)
		want, known := s.users[user]
		if !known || pass == "" || pass != want {
			out = exception("EDAMUserException: 认证失败")
			break
		}
		st, err := s.storeFor(user)
		if err != nil {
			out = exception("加载用户状态失败: " + err.Error())
			break
		}
		tok := newToken()
		st.TokenPut(tok)
		s.mu.Lock()
		s.userFor[tok] = user
		s.mu.Unlock()
		if err := st.Save(s.statePathFor(user)); err != nil {
			log.Printf("[edam] 会话落盘失败: %v", err)
		}
		// AuthenticationResult: f2 = authenticationToken
		out = reply(name, seq, func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeString)
			w.String(tok)
			w.FieldStop()
		})
	case "getNoteStoreUrl":
		// 参数容器 f1 = authenticationToken（与 authenticate 的 user=f1 同层）
		tok := str(1)
		if _, ok := s.userForToken(tok); !ok {
			w.Write(exception("EDAMUserException: 无效 token"))
			return
		}
		out = reply(name, seq, func(w *thrift.Writer) {
			scheme := "http"
			if r.TLS != nil {
				scheme = "https"
			}
			w.FieldBegin(0, thrift.TypeString)
			w.String(scheme + "://" + r.Host + s.noteStore)
			w.FieldStop()
		})
	default:
		out = exception("未知 UserStore 方法: " + name)
	}
	w.Header().Set("Content-Type", "application/x-thrift")
	w.Write(out)
}

// userForToken token → (用户, Store)；无效返回 nil。
func (s *Server) userForToken(tok string) (*store.Store, bool) {
	if tok == "" {
		return nil, false
	}
	s.mu.Lock()
	user, ok := s.userFor[tok]
	s.mu.Unlock()
	if ok {
		st, _ := s.storeFor(user)
		return st, true
	}
	// 内存索引未命中：可能重启后索引缺失，扫各用户已加载/落盘的 token
	for user := range s.users {
		st, err := s.storeFor(user)
		if err != nil {
			continue
		}
		if st.TokenValid(tok) {
			s.mu.Lock()
			s.userFor[tok] = user
			s.mu.Unlock()
			return st, true
		}
	}
	return nil, false
}

// serveNoteStore 处理 /edam/note。
func (s *Server) ServeNoteStore(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "read body", http.StatusBadRequest)
		return
	}
	rd := &thrift.Reader{B: body}
	name, _, seq, err := rd.Message()
	if err != nil {
		w.Write(exception(err.Error()))
		return
	}
	raw, err := rd.Fields()
	if err != nil {
		w.Write(exception(err.Error()))
		return
	}
	// Rust rpc 把参数包在 message f1 的 args 容器 STRUCT 内，先下钻
	args := raw
	for _, f := range raw {
		if f.ID == 1 {
			args = f.Val.Struct
			break
		}
	}
	log.Printf("[edam] %s args=%+v", name, args)
	// 容器 f1 = authenticationToken
	tok := ""
	for _, f := range args {
		if f.ID == 1 && f.Val.Str != "" {
			tok = f.Val.Str
			break
		}
	}
	st, ok := s.userForToken(tok)
	if !ok {
		w.Write(exception("EDAMUserException: 无效 token"))
		return
	}
	str := func(id int16) string {
		for _, f := range args {
			if f.ID == id {
				return f.Val.Str
			}
		}
		return ""
	}
	structArg := func(id int16) []thrift.Field {
		for _, f := range args {
			if f.ID == id {
				return f.Val.Struct
			}
		}
		return nil
	}
	// afterUSN 兼容 i32（EDAM 定义）与 i64
	i32i64 := func(id int16) int64 {
		for _, f := range args {
			if f.ID == id {
				if f.Val.I64 != 0 {
					return f.Val.I64
				}
				return int64(f.Val.I32)
			}
		}
		return 0
	}
	i32arg := func(id int16) int32 {
		for _, f := range args {
			if f.ID == id {
				return f.Val.I32
			}
		}
		return 0
	}
	out := func() []byte {
		switch name {
		case "getSyncState":
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldI64(3, st.UpdateCount)
			})
		case "getFilteredSyncChunk":
			after := i32i64(2)
			maxEntries := int(i32arg(3)) // 0 = 不限（旧客户端兼容）
			return reply(name, seq, func(w *thrift.Writer) {
				notes := st.NotesSince(after)
				high := st.UpdateCount
				truncated := maxEntries > 0 && len(notes) > maxEntries
				if truncated {
					notes = notes[:maxEntries] // USN 升序截断
					high = notes[len(notes)-1].USN
				}
				// 截断时其余集合只带 (after, high] 区间，游标续拉语义闭合
				inRange := func(usn int64) bool { return usn > after && usn <= high }
				var nbs []*store.Notebook
				for _, nb := range st.NotebooksSince(after) {
					if inRange(nb.USN) {
						nbs = append(nbs, nb)
					}
				}
				var tgs []*store.Tag
				for _, tg := range st.TagsSince(after) {
					if inRange(tg.USN) {
						tgs = append(tgs, tg)
					}
				}
				var expNoteGuids, expNbGuids, expTagGuids []string
				for _, e := range st.ExpungedSince(after) {
					if e.USN > high {
						continue
					}
					switch e.Kind {
					case "note":
						expNoteGuids = append(expNoteGuids, e.GUID)
					case "notebook":
						expNbGuids = append(expNbGuids, e.GUID)
					case "tag":
						expTagGuids = append(expTagGuids, e.GUID)
					}
				}
				w.FieldI32(2, int32(high))
				w.FieldI64(3, st.UpdateCount)
				// list 元素不带字段头：每个 STRUCT 元素直接写字段序列 + STOP
				w.FieldListBegin(4, thrift.TypeStruct, len(notes))
				for _, n := range notes {
					writeNoteFields(w, st, n, false)
				}
				// f5 notebooks：让新端先建笔记本再对齐笔记归属（f7 stack 分组）
				w.FieldListBegin(5, thrift.TypeStruct, len(nbs))
				for _, nb := range nbs {
					w.FieldString(1, nb.GUID)
					w.FieldString(2, nb.Name)
					if nb.Stack != "" {
						w.FieldString(7, nb.Stack)
					}
					w.FieldI32(5, int32(nb.USN))
					w.FieldStop()
				}
				// f6 tags：标签跨端传播（Tag: f1 guid, f2 name）
				w.FieldListBegin(6, thrift.TypeStruct, len(tgs))
				for _, tg := range tgs {
					w.FieldString(1, tg.GUID)
					w.FieldString(2, tg.Name)
					w.FieldStop()
				}
				// f11/f12/f13 删除账（笔记/笔记本/标签）
				w.FieldListBegin(11, thrift.TypeString, len(expNoteGuids))
				for _, g := range expNoteGuids {
					w.String(g)
				}
				w.FieldListBegin(12, thrift.TypeString, len(expNbGuids))
				for _, g := range expNbGuids {
					w.String(g)
				}
				w.FieldListBegin(13, thrift.TypeString, len(expTagGuids))
				for _, g := range expTagGuids {
					w.String(g)
				}
				w.FieldStop()
			})
		case "getResource":
			// 参数：f2 guid, f3 withBinary, f4 withData（EDAM IDL 序；
			// body 在 withBinary 或 withData 任一为真时携带）
			guid := str(2)
			res := st.ResourceForGuid(guid)
			if res == nil {
				return exception("EDAMNotFoundException: 附件不存在 " + guid)
			}
			withBody := noteBool(args, 3) || noteBool(args, 4)
			return reply(name, seq, func(w *thrift.Writer) {
				writeResourceFields(w, res, withBody)
				w.FieldStop()
			})
		case "putResource":
			// 扩展方法：单附件上传，不动笔记其余附件。
			// 参数：f2 guid, f3 noteGuid, f4 Data{f1 body, f3 size},
			// f5 mime, f6 fileName → 裸 f0 i64 usn。
			guid, noteGUID := str(2), str(3)
			data := parseDataBytes(structArg(4))
			usn := st.UpsertResource(noteGUID, guid, str(5), str(6), data)
			if usn == 0 {
				return exception("EDAMNotFoundException: guid 属于其他笔记 " + guid)
			}
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		case "getNote":
			// 参数：f2 guid → 完整 Note（含 f13 附件元数据）。
			guid := str(2)
			n := st.NoteForGuid(guid)
			if n == nil {
				return exception("EDAMNotFoundException: 笔记不存在 " + guid)
			}
			return reply(name, seq, func(w *thrift.Writer) {
				writeNoteFields(w, st, n, false)
			})
		case "findNotes":
			// 参数：f2 filter{f3 words 空格分隔}, f3 offset, f4 maxNotes
			// → f0{f1 totalNotes, f2 list<NoteMetadata{f1 guid,f2 title,f3 usn}>}
			filter := structArg(2)
			words := strings.Fields(noteStr(filter, 3))
			offset, maxN := int(i32arg(3)), int(i32arg(4))
			if maxN <= 0 {
				maxN = 50
			}
			total, hits := st.SearchNotes(words, offset, maxN)
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldI32(1, int32(total))
				w.FieldListBegin(2, thrift.TypeStruct, len(hits))
				for _, n := range hits {
					w.FieldString(1, n.GUID)
					w.FieldString(2, n.Title)
					w.FieldI64(3, n.USN)
					w.FieldStop()
				}
				w.FieldStop()
			})
		case "listNotebooks":
			nbs := st.ListNotebooks()
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldListBegin(0, thrift.TypeStruct, len(nbs))
				for _, nb := range nbs {
					w.FieldString(1, nb.GUID)
					w.FieldString(2, nb.Name)
					if nb.Stack != "" {
						w.FieldString(7, nb.Stack)
					}
					w.FieldI32(5, int32(nb.USN))
					w.FieldStop()
				}
				w.FieldStop()
			})
		case "listTags":
			tgs := st.ListTags()
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldListBegin(0, thrift.TypeStruct, len(tgs))
				for _, tg := range tgs {
					w.FieldString(1, tg.GUID)
					w.FieldString(2, tg.Name)
					w.FieldStop()
				}
				w.FieldStop()
			})
		case "getNotebook":
			nb := st.NotebookForGuid(str(2))
			if nb == nil {
				return exception("EDAMNotFoundException: 笔记本不存在 " + str(2))
			}
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, nb.GUID)
				w.FieldString(2, nb.Name)
				if nb.Stack != "" {
					w.FieldString(7, nb.Stack)
				}
				w.FieldI32(5, int32(nb.USN))
				w.FieldStop()
			})
		case "getTag":
			tg := st.TagForGuid(str(2))
			if tg == nil {
				return exception("EDAMNotFoundException: 标签不存在 " + str(2))
			}
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, tg.GUID)
				w.FieldString(2, tg.Name)
				w.FieldStop()
			})
		case "listNoteVersions":
			// 参数：f2 guid → f0 list<VersionRef{f1 usn, f2 updated, f3 title}>
			vs := st.VersionsOf(str(2))
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldListBegin(0, thrift.TypeStruct, len(vs))
				for _, v := range vs {
					w.FieldI64(1, v.USN)
					w.FieldI64(2, v.Updated)
					w.FieldString(3, v.Title)
					w.FieldStop()
				}
				w.FieldStop()
			})
		case "getNoteVersion":
			// 参数：f2 guid, f3 usn → Note 形状的历史快照。
			v := st.VersionByUSN(str(2), i32i64(3))
			if v == nil {
				return exception("EDAMNotFoundException: 版本不存在")
			}
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, str(2))
				w.FieldString(2, v.Title)
				w.FieldString(3, v.Content)
				w.FieldI64(7, v.Updated)
				w.FieldStop()
			})
		case "createNote":
			note := strArgStruct(args, 2)
			guid := newGUID()
			// f9 active 缺省为 true（创建即激活）
			active := true
			if noteHasField(note, 9) {
				active = noteBool(note, 9)
			}
			usn := st.UpsertNote(guid, noteStr(note, 2), noteStr(note, 3), noteStr(note, 11), active)
			if noteHasField(note, 13) {
				st.ReplaceResources(guid, parseResourceInputs(noteList(note, 13)))
			}
			// Note: guid=1, updateSequenceNum=10（reply 外层已包 f0 STRUCT）
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
				w.FieldI32(10, int32(usn))
			})
		case "updateNote":
			note := strArgStruct(args, 2)
			guid := noteStr(note, 1)
			// 补丁语义：未出现的字段保持原值（active/标题/正文/归属）
			title, content, notebook, active := noteStr(note, 2), noteStr(note, 3), noteStr(note, 11), noteBool(note, 9)
			if old := st.NoteForGuid(guid); old != nil {
				if !noteHasField(note, 2) {
					title = old.Title
				}
				if !noteHasField(note, 3) {
					content = old.Content
				}
				if !noteHasField(note, 11) {
					notebook = old.NotebookGUID
				}
				if !noteHasField(note, 9) {
					active = old.Active
				}
			}
			usn := st.UpsertNote(guid, title, content, notebook, active)
			if noteHasField(note, 13) {
				st.ReplaceResources(guid, parseResourceInputs(noteList(note, 13)))
			}
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeNote":
			usn := st.ExpungeNote(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		case "createNotebook":
			nb := structArg(2)
			guid := newGUID()
			usn := st.UpsertNotebook(guid, noteStr(nb, 2), noteStr(nb, 7))
			// Notebook: f0 struct{ guid=1, updateSequenceNum=5 }
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
				w.FieldI32(5, int32(usn))
			})
		case "updateNotebook":
			guid := str(2)
			nb := structArg(3)
			// f7 stack 出现即更新（空串 = 清除分组）；未出现保持原值
			stack := ""
			hasStack := false
			for _, f := range nb {
				if f.ID == 7 {
					stack, hasStack = f.Val.Str, true
				}
			}
			if !hasStack {
				if old := st.NotebookForGuid(guid); old != nil {
					stack = old.Stack
				}
			}
			usn := st.UpsertNotebook(guid, noteStr(nb, 2), stack)
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeNotebook":
			usn := st.ExpungeNotebook(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		case "createTag":
			tg := structArg(2)
			guid := newGUID()
			_ = st.UpsertTag(guid, noteStr(tg, 2))
			// Tag: f0 struct{ guid=1 }
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
			})
		case "updateTag":
			guid := str(2)
			tg := structArg(3)
			usn := st.UpsertTag(guid, noteStr(tg, 2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeTag":
			usn := st.ExpungeTag(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		default:
			return exception("未知 NoteStore 方法: " + name)
		}
	}()
	// 只读方法不落盘
	switch name {
	case "getSyncState", "getFilteredSyncChunk", "getResource",
		"getNote", "findNotes", "listNotebooks", "listTags",
		"getNotebook", "getTag", "listNoteVersions", "getNoteVersion":
	default:
		path := s.dataPath
		if s.Store == nil {
			s.mu.Lock()
			user := s.userFor[tok]
			s.mu.Unlock()
			path = s.statePathFor(user)
		}
		if err := st.Save(path); err != nil {
			log.Printf("[edam] 落盘失败: %v", err)
		}
	}
	w.Header().Set("Content-Type", "application/x-thrift")
	w.Write(out)
}

// writeNoteFields 写 Note STRUCT 字段序列（以 STOP 收尾，可作 list 元素
// 或独立 struct）：正文/归属/active/时间戳 + f13 附件（metadataOnly：
// guid/mime/fileName + Data.size，body 经 getResource 按需拉取）。
func writeNoteFields(w *thrift.Writer, st *store.Store, n *store.Note, withBody bool) {
	w.FieldString(1, n.GUID)
	w.FieldString(2, n.Title)
	w.FieldString(3, n.Content)
	w.FieldBool(9, n.Active)
	w.FieldI64(7, n.Updated)
	if n.NotebookGUID != "" {
		w.FieldString(11, n.NotebookGUID)
	}
	if rs := st.ResourcesForNote(n.GUID); len(rs) > 0 {
		w.FieldListBegin(13, thrift.TypeStruct, len(rs))
		for _, r := range rs {
			writeResourceFields(w, r, withBody)
			w.FieldStop()
		}
	}
	w.FieldStop()
}

// parseDataBytes 取 Data STRUCT 的 f1 body 字节。
func parseDataBytes(dataFields []thrift.Field) []byte {
	if dataFields == nil {
		return nil
	}
	for _, f := range dataFields {
		if f.ID == 1 {
			return []byte(f.Val.Str)
		}
	}
	return nil
}

// strArgStruct 取 args 中 id 对应的嵌套 struct 字段值。
func strArgStruct(args []thrift.Field, id int16) []thrift.Field {
	for _, f := range args {
		if f.ID == id {
			return f.Val.Struct
		}
	}
	return nil
}

// noteStr 取 note 字段列表中 id 对应的字符串。
func noteStr(fields []thrift.Field, id int16) string {
	for _, f := range fields {
		if f.ID == id {
			return f.Val.Str
		}
	}
	return ""
}

// noteList 取 note 字段列表中 id 对应的 list 元素结构体列表。
func noteList(fields []thrift.Field, id int16) [][]thrift.Field {
	for _, f := range fields {
		if f.ID == id {
			out := make([][]thrift.Field, 0, len(f.Val.List))
			for _, it := range f.Val.List {
				out = append(out, it.Struct)
			}
			return out
		}
	}
	return nil
}

// noteHasField 判断 note 是否携带某字段（区别于空值）。
func noteHasField(fields []thrift.Field, id int16) bool {
	for _, f := range fields {
		if f.ID == id {
			return true
		}
	}
	return false
}

// parseResourceInputs 解析 note f13 resources list（Data{f1 body,f4 mime} + f4 mime + f14 attributes{f1 fileName}）。
func parseResourceInputs(items [][]thrift.Field) []store.ResourceInput {
	var out []store.ResourceInput
	for _, st := range items {
		mime := ""
		fileName := ""
		var data []byte
		for _, f := range st {
			switch f.ID {
			case 4:
				mime = f.Val.Str
			case 3:
				for _, df := range f.Val.Struct {
					if df.ID == 1 {
						data = []byte(df.Val.Str)
					}
				}
			case 14:
				for _, af := range f.Val.Struct {
					if af.ID == 1 {
						fileName = af.Val.Str
					}
				}
			}
		}
		out = append(out, store.ResourceInput{Mime: mime, FileName: fileName, Data: data})
	}
	return out
}

// noteBool 取 note 字段列表中 id 对应的布尔。
func noteBool(fields []thrift.Field, id int16) bool {
	for _, f := range fields {
		if f.ID == id {
			return f.Val.Bool
		}
	}
	return false
}

// writeResourceFields 写 Resource STRUCT 的字段序列（不含首尾 STOP）：
// 元数据 guid/mime/fileName + Data.size；withBody 时 Data 额外携带 f1 body。
func writeResourceFields(w *thrift.Writer, r *store.Resource, withBody bool) {
	w.FieldString(1, r.GUID)
	w.FieldBegin(3, thrift.TypeStruct) // Data{f1 body, f3 size}
	if withBody {
		w.FieldString(1, string(r.Data))
	}
	w.FieldI32(3, int32(len(r.Data)))
	w.FieldStop()
	w.FieldString(4, r.Mime)
	w.FieldBegin(14, thrift.TypeStruct) // attributes{f1 fileName}
	w.FieldString(1, r.FileName)
	w.FieldStop()
}

func newGUID() string {
	b := make([]byte, 8)
	binary.BigEndian.PutUint64(b, uint64(timeNowNano()))
	return hex.EncodeToString(b)
}

func timeNowNano() int64 { return timeNow().UnixNano() }
