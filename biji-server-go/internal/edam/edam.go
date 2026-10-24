// Package edam 实现 EDAM Thrift over HTTP 的服务端路由：
// 解析客户端严格 Thrift 二进制请求，分发到 store，编码 Thrift 响应。
package edam

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"io"
	"log"
	"net/http"
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

// Server EDAM 服务端。
type Server struct {
	Store     *store.Store
	User      string // 认证用户名
	Pass      string // 认证密码
	mu        sync.Mutex
	tokens    map[string]bool // 已认证会话 token
	noteStore string          // getNoteStoreUrl 返回值
	dataPath  string          // 写操作后的状态落盘路径
}

// New 创建服务端；noteStoreURL 为 NoteStore 路径前缀（按请求 Host 拼绝对地址），
// dataPath 为写操作后的状态落盘文件。
func New(st *store.Store, user, pass, noteStoreURL, dataPath string) *Server {
	return &Server{
		Store:     st,
		User:      user,
		Pass:      pass,
		tokens:    map[string]bool{},
		noteStore: noteStoreURL,
		dataPath:  dataPath,
	}
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
		if user == s.User && pass == s.Pass {
			tok := newToken()
			s.mu.Lock()
			s.tokens[tok] = true
			s.mu.Unlock()
			// AuthenticationResult: f2 = authenticationToken
			out = reply(name, seq, func(w *thrift.Writer) {
				w.FieldBegin(2, thrift.TypeString)
				w.String(tok)
				w.FieldStop()
			})
		} else {
			out = exception("EDAMUserException: 认证失败")
		}
	case "getNoteStoreUrl":
		// 参数容器 f1 = authenticationToken（与 authenticate 的 user=f1 同层）
		tok := str(1)
		if !s.validToken(tok) {
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

func (s *Server) validToken(tok string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.tokens[tok]
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
	if !s.validToken(tok) {
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
	out := func() []byte {
		defer func(b []byte) {
			_ = b
		}(nil)
		switch name {
		case "getSyncState":
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldI64(3, s.Store.UpdateCount)
			})
		case "getFilteredSyncChunk":
			after := i32i64(2)
			return reply(name, seq, func(w *thrift.Writer) {
				notes := s.Store.NotesSince(after)
				high := s.Store.UpdateCount
				nbs := s.Store.NotebooksSince(after)
				tgs := s.Store.TagsSince(after)
				var expNoteGuids, expNbGuids, expTagGuids []string
				for _, e := range s.Store.ExpungedSince(after) {
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
				w.FieldI64(3, high)
				// list 元素不带字段头：每个 STRUCT 元素直接写字段序列 + STOP
				w.FieldListBegin(4, thrift.TypeStruct, len(notes))
				for _, n := range notes {
					w.FieldString(1, n.GUID)
					w.FieldString(2, n.Title)
					w.FieldString(3, n.Content)
					w.FieldBool(9, n.Active)
					w.FieldI64(7, n.Updated)
					if n.NotebookGUID != "" {
						w.FieldString(11, n.NotebookGUID)
					}
					// f13 resources：chunk 只带元数据（guid/mime/fileName + Data.size），
					// 大附件 body 经 getResource 按需拉取（EDAM 分页语义）
					if rs := s.Store.ResourcesForNote(n.GUID); len(rs) > 0 {
						w.FieldListBegin(13, thrift.TypeStruct, len(rs))
						for _, r := range rs {
							writeResourceFields(w, r, false)
							w.FieldStop()
						}
					}
					w.FieldStop()
				}
				// f5 notebooks：让新端先建笔记本再对齐笔记归属
				w.FieldListBegin(5, thrift.TypeStruct, len(nbs))
				for _, nb := range nbs {
					w.FieldString(1, nb.GUID)
					w.FieldString(2, nb.Name)
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
			r := s.Store.ResourceForGuid(guid)
			if r == nil {
				return exception("EDAMNotFoundException: 附件不存在 " + guid)
			}
			withBody := noteBool(args, 3) || noteBool(args, 4)
			return reply(name, seq, func(w *thrift.Writer) {
				writeResourceFields(w, r, withBody)
				w.FieldStop()
			})
		case "createNote":
			note := strArgStruct(args, 2)
			guid := newGUID()
			usn := s.Store.UpsertNote(guid, noteStr(note, 2), noteStr(note, 3), noteStr(note, 11), noteBool(note, 9))
			if noteHasField(note, 13) {
				s.Store.ReplaceResources(guid, parseResourceInputs(noteList(note, 13)))
			}
			// Note: guid=1, updateSequenceNum=10（reply 外层已包 f0 STRUCT）
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
				w.FieldI32(10, int32(usn))
			})
		case "updateNote":
			note := strArgStruct(args, 2)
			guid := noteStr(note, 1)
			usn := s.Store.UpsertNote(guid, noteStr(note, 2), noteStr(note, 3), noteStr(note, 11), noteBool(note, 9))
			if noteHasField(note, 13) {
				s.Store.ReplaceResources(guid, parseResourceInputs(noteList(note, 13)))
			}
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeNote":
			usn := s.Store.ExpungeNote(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		case "createNotebook":
			nb := structArg(2)
			guid := newGUID()
			usn := s.Store.UpsertNotebook(guid, noteStr(nb, 2))
			// Notebook: f0 struct{ guid=1, updateSequenceNum=5 }
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
				w.FieldI32(5, int32(usn))
			})
		case "updateNotebook":
			guid := str(2)
			nb := structArg(3)
			usn := s.Store.UpsertNotebook(guid, noteStr(nb, 2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeNotebook":
			usn := s.Store.ExpungeNotebook(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		case "createTag":
			tg := structArg(2)
			guid := newGUID()
			_ = s.Store.UpsertTag(guid, noteStr(tg, 2))
			// Tag: f0 struct{ guid=1 }
			return reply(name, seq, func(w *thrift.Writer) {
				w.FieldString(1, guid)
			})
		case "updateTag":
			guid := str(2)
			tg := structArg(3)
			usn := s.Store.UpsertTag(guid, noteStr(tg, 2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI32(0, int32(usn))
			})
		case "expungeTag":
			usn := s.Store.ExpungeTag(str(2))
			return replyRaw(name, seq, func(w *thrift.Writer) {
				w.FieldI64(0, usn)
			})
		default:
			return exception("未知 NoteStore 方法: " + name)
		}
	}()
	// 只读方法不落盘（getSyncState/getFilteredSyncChunk/getResource）
	switch name {
	case "getSyncState", "getFilteredSyncChunk", "getResource":
	default:
		if err := s.Store.Save(s.dataPath); err != nil {
			log.Printf("[edam] 落盘失败: %v", err)
		}
	}
	w.Header().Set("Content-Type", "application/x-thrift")
	w.Write(out)
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
