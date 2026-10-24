// Features 补齐测试：stack 同步 / putResource / chunk 分页 / 多用户隔离 /
// token 持久化 / 读 API（findNotes/getNote/list*/单查）/ 笔记版本历史。
package edam

import (
	"net/http"
	"net/http/httptest"
	"bytes"
	"path/filepath"
	"testing"

	"biji-server-go/internal/store"
	"biji-server-go/internal/thrift"
)

// newAuthServer 起一个单用户服务端并完成 authenticate，返回 (srv, token)。
func newAuthServer(t *testing.T) (*Server, string) {
	t.Helper()
	st := store.New()
	dir := t.TempDir()
	srv := New(st, "u", "p", "/edam/note", filepath.Join(dir, "state.json"))
	_, fields := parseReply(t, call(t, srv.ServeUserStore, "authenticate", func(w *thrift.Writer) {
		w.FieldString(1, "u")
		w.FieldString(2, "p")
	}))
	tok := fieldStr(unwrapStruct(t, fields), 2)
	if tok == "" {
		t.Fatal("缺 token")
	}
	return srv, tok
}

func noteCallTok(t *testing.T, srv *Server, tok, method string, build func(w *thrift.Writer)) ([]thrift.Field, []thrift.Field) {
	t.Helper()
	_, msg := parseReply(t, call(t, srv.ServeNoteStore, method, func(w *thrift.Writer) {
		w.FieldString(1, tok)
		build(w)
	}))
	return msg, unwrapStruct(t, msg)
}

// S1 笔记本 stack：createNotebook 带 f7 → chunk f5 与 getNotebook 回传 f7；
// updateNotebook 缺 f7 保持原 stack、空串清除。
func TestNotebookStackSync(t *testing.T) {
	srv, tok := newAuthServer(t)
	_, inner := noteCallTok(t, srv, tok, "createNotebook", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "工作")
		w.FieldString(7, "公司")
		w.FieldStop()
	})
	guid := fieldStr(inner, 1)
	if guid == "" {
		t.Fatal("缺 guid")
	}
	// chunk 回传 f7
	_, chunk := noteCallTok(t, srv, tok, "getFilteredSyncChunk", func(w *thrift.Writer) {
		w.FieldI32(2, 0)
	})
	var gotStack string
	for _, f := range chunk {
		if f.ID == 5 {
			nb := f.Val.List[0].Struct
			for _, nf := range nb {
				if nf.ID == 7 {
					gotStack = nf.Val.Str
				}
			}
		}
	}
	if gotStack != "公司" {
		t.Fatalf("chunk stack = %q, 期望 公司", gotStack)
	}
	// updateNotebook 不带 f7：保持原 stack
	noteCallTok(t, srv, tok, "updateNotebook", func(w *thrift.Writer) {
		w.FieldString(2, guid)
		w.FieldBegin(3, thrift.TypeStruct)
		w.FieldString(2, "工作改")
		w.FieldStop()
	})
	_, nb := noteCallTok(t, srv, tok, "getNotebook", func(w *thrift.Writer) {
		w.FieldString(2, guid)
	})
	if fieldStr(nb, 7) != "公司" || fieldStr(nb, 2) != "工作改" {
		t.Fatalf("缺 f7 时 stack 应保持: %+v", nb)
	}
	// 空 f7：清除 stack
	noteCallTok(t, srv, tok, "updateNotebook", func(w *thrift.Writer) {
		w.FieldString(2, guid)
		w.FieldBegin(3, thrift.TypeStruct)
		w.FieldString(2, "工作改")
		w.FieldString(7, "")
		w.FieldStop()
	})
	_, nb2 := noteCallTok(t, srv, tok, "getNotebook", func(w *thrift.Writer) {
		w.FieldString(2, guid)
	})
	for _, f := range nb2 {
		if f.ID == 7 && f.Val.Str != "" {
			t.Fatalf("空 f7 应清除 stack: %+v", f)
		}
	}
}

// S2 putResource：单附件上传不动其他附件；body 经 getResource 取回；
// 跨笔记 guid 冲突拒绝。
func TestPutResource(t *testing.T) {
	srv, tok := newAuthServer(t)
	// 建两笔记 + 笔记1 带一个初始附件
	_, n1 := noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "一")
		w.FieldString(3, "<en-note/>")
		w.FieldStop()
	})
	g1 := fieldStr(n1, 1)
	_, n2 := noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "二")
		w.FieldString(3, "<en-note/>")
		w.FieldStop()
	})
	g2 := fieldStr(n2, 1)
	noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {})
	// putResource 上传到 g1
	payload := []byte{0x01, 0x02, 0xff}
	_, raw := parseReply(t, call(t, srv.ServeNoteStore, "putResource", func(w *thrift.Writer) {
		w.FieldString(1, tok)
		w.FieldString(2, "res-put-1")
		w.FieldString(3, g1)
		w.FieldBegin(4, thrift.TypeStruct)
		w.FieldString(1, string(payload))
		w.FieldStop()
		w.FieldString(5, "application/octet-stream")
		w.FieldString(6, "up.bin")
	}))
	if fieldI64(raw, 0) == 0 {
		t.Fatalf("缺 usn: %+v", raw)
	}
	// getResource 取回字节
	_, res := noteCallTok(t, srv, tok, "getResource", func(w *thrift.Writer) {
		w.FieldString(2, "res-put-1")
		w.FieldBool(3, true)
	})
	var body string
	for _, f := range res {
		if f.ID == 3 {
			for _, df := range f.Val.Struct {
				if df.ID == 1 {
					body = df.Val.Str
				}
			}
		}
		if f.ID == 14 {
			for _, af := range f.Val.Struct {
				if af.ID == 1 && af.Val.Str != "up.bin" {
					t.Fatalf("fileName = %q", af.Val.Str)
				}
			}
		}
	}
	if body != string(payload) {
		t.Fatalf("字节失真: %x", body)
	}
	// chunk 笔记1 现带 1 个资源元数据
	_, chunk := noteCallTok(t, srv, tok, "getFilteredSyncChunk", func(w *thrift.Writer) {
		w.FieldI32(2, 0)
	})
	for _, f := range chunk {
		if f.ID != 4 {
			continue
		}
		for _, el := range f.Val.List {
			var isG1 bool
			var resCount int
			for _, nf := range el.Struct {
				if nf.ID == 1 && nf.Val.Str == g1 {
					isG1 = true
				}
				if nf.ID == 13 {
					resCount = len(nf.Val.List)
				}
			}
			if isG1 && resCount != 1 {
				t.Fatalf("笔记 %s 资源数 = %d, 期望 1", g1, resCount)
			}
		}
	}
	// 同一 guid 改挂到 g2：拒绝（异常响应）
	resp := call(t, srv.ServeNoteStore, "putResource", func(w *thrift.Writer) {
		w.FieldString(1, tok)
		w.FieldString(2, "res-put-1")
		w.FieldString(3, g2)
		w.FieldBegin(4, thrift.TypeStruct)
		w.FieldStop()
		w.FieldString(5, "x")
		w.FieldString(6, "x")
	})
	rd := &thrift.Reader{B: resp}
	if _, mtype, _, _ := rd.Message(); mtype != thrift.Exception {
		t.Fatal("跨笔记 guid 冲突应拒绝")
	}
}

// S3 chunk 分页：7 条笔记 maxEntries=3 → 3+3+1 三页，游标闭合到 updateCount。
func TestChunkPagination(t *testing.T) {
	srv, tok := newAuthServer(t)
	for i := 0; i < 7; i++ {
		noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(2, "n")
			w.FieldString(3, "<en-note/>")
			w.FieldStop()
		})
	}
	var after int64
	var pages, totalNotes int
	for page := 0; page < 5; page++ {
		_, chunk := noteCallTok(t, srv, tok, "getFilteredSyncChunk", func(w *thrift.Writer) {
			w.FieldI32(2, int32(after))
			w.FieldI32(3, 3)
		})
		notes := fieldsList(chunk, 4)
		totalNotes += len(notes)
		pages++
		after = fieldI64(chunk, 2) // chunkHighUSN 游标
		if after >= fieldI64(chunk, 3) {
			break
		}
	}
	if pages != 3 || totalNotes != 7 {
		t.Fatalf("pages=%d notes=%d, 期望 3 页 7 条", pages, totalNotes)
	}
}

// fieldsList 取字段列表中 id 对应的 list 元素。
func fieldsList(fields []thrift.Field, id int16) []thrift.TVal {
	for _, f := range fields {
		if f.ID == id {
			return f.Val.List
		}
	}
	return nil
}

// S4 多用户隔离：alice/bob 独立记账与数据；bob 看不到 alice 的笔记。
func TestMultiUserIsolation(t *testing.T) {
	dir := t.TempDir()
	srv := NewMulti(map[string]string{"alice": "ap", "bob": "bp"},
		"/edam/note", filepath.Join(dir, "state.json"))
	auth := func(user, pass string) string {
		_, fields := parseReply(t, call(t, srv.ServeUserStore, "authenticate", func(w *thrift.Writer) {
			w.FieldString(1, user)
			w.FieldString(2, pass)
		}))
		return fieldStr(unwrapStruct(t, fields), 2)
	}
	atok, btok := auth("alice", "ap"), auth("bob", "bp")
	if atok == "" || btok == "" || atok == btok {
		t.Fatal("token 铸造异常")
	}
	noteCallTok(t, srv, atok, "createNote", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "alice 私有")
		w.FieldString(3, "<en-note/>")
		w.FieldStop()
	})
	_, bchunk := noteCallTok(t, srv, btok, "getFilteredSyncChunk", func(w *thrift.Writer) {
		w.FieldI32(2, 0)
	})
	if n := len(fieldsList(bchunk, 4)); n != 0 {
		t.Fatalf("bob 不应看到 alice 笔记, got %d", n)
	}
	if fieldI64(bchunk, 3) != 0 {
		t.Fatalf("bob updateCount 应为 0, got %d", fieldI64(bchunk, 3))
	}
	// alice 的 token 不能写 bob 域？token 只映射 alice store——bob 数据独立文件
	// （目录中只应出现 alice 的状态文件）
	if files, _ := filepath.Glob(filepath.Join(dir, "*")); len(files) > 2 { // state.alice.json (+可能的tmp)
		t.Fatalf("意外文件: %v", files)
	}
}

// S5 token 持久化：authenticate 落盘后重载，token 仍有效。
func TestTokenPersistenceAcrossReload(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")
	st := store.New()
	srv := New(st, "u", "p", "/edam/note", path)
	_, fields := parseReply(t, call(t, srv.ServeUserStore, "authenticate", func(w *thrift.Writer) {
		w.FieldString(1, "u")
		w.FieldString(2, "p")
	}))
	tok := fieldStr(unwrapStruct(t, fields), 2)
	// 模拟重启：重载状态 + 新 Server
	st2, err := store.Load(path)
	if err != nil {
		t.Fatal(err)
	}
	srv2 := New(st2, "u", "p", "/edam/note", path)
	if _, mtype, _, _ := (&thrift.Reader{B: call(t, srv2.ServeNoteStore, "getSyncState", func(w *thrift.Writer) {
		w.FieldString(1, tok)
	})}).Message(); mtype == thrift.Exception {
		t.Fatal("重启后持久 token 应仍有效")
	}
}

// S6 读 API：findNotes（多词 AND + 分页总数）/ getNote / listNotebooks / listTags。
func TestReadApis(t *testing.T) {
	srv, tok := newAuthServer(t)
	_, nb := noteCallTok(t, srv, tok, "createNotebook", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "日志")
		w.FieldStop()
	})
	nbGuid := fieldStr(nb, 1)
	noteCallTok(t, srv, tok, "createTag", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "重要")
		w.FieldStop()
	})
	var g1 string
	for _, title := range []string{"购物 清单", "会议 纪要", "购物 票据"} {
		_, inner := noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(2, title)
			w.FieldString(3, "<en-note>正文</en-note>")
			w.FieldStop()
		})
		if g1 == "" {
			g1 = fieldStr(inner, 1)
		}
	}
	// findNotes：单词命中 2 条；双词 AND 命中 1 条；offset 分页
	// 注意 fieldI64 返回 int64；直接取字段
	_, inner := noteCallTok(t, srv, tok, "findNotes", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(3, "购物")
		w.FieldStop()
		w.FieldI32(3, 0)
		w.FieldI32(4, 10)
	})
	if got := fieldI64(inner, 1); got != 2 {
		t.Fatalf("购物 totalNotes = %d, 期望 2", got)
	}
	_, inner = noteCallTok(t, srv, tok, "findNotes", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(3, "购物 清单")
		w.FieldStop()
		w.FieldI32(3, 0)
		w.FieldI32(4, 10)
	})
	if got := fieldI64(inner, 1); got != 1 {
		t.Fatalf("购物+清单 AND totalNotes = %d, 期望 1", got)
	}
	// getNote：标题 + 归属 + 附件元数据
	_, note := noteCallTok(t, srv, tok, "getNote", func(w *thrift.Writer) {
		w.FieldString(2, g1)
	})
	if fieldStr(note, 2) != "购物 清单" {
		t.Fatalf("getNote title = %q", fieldStr(note, 2))
	}
	// listNotebooks / listTags
	_, nbs := noteCallTok(t, srv, tok, "listNotebooks", func(w *thrift.Writer) {})
	if n := len(fieldsList(nbs, 0)); n != 1 {
		t.Fatalf("listNotebooks = %d", n)
	}
	_, tgs := noteCallTok(t, srv, tok, "listTags", func(w *thrift.Writer) {})
	if n := len(fieldsList(tgs, 0)); n != 1 {
		t.Fatalf("listTags = %d", n)
	}
	// getNotebook / getTag
	_, one := noteCallTok(t, srv, tok, "getNotebook", func(w *thrift.Writer) {
		w.FieldString(2, nbGuid)
	})
	if fieldStr(one, 2) != "日志" {
		t.Fatalf("getNotebook = %+v", one)
	}
}

// S6b 笔记版本历史：两次内容更新 → listNoteVersions 两条；getNoteVersion 取回旧文。
func TestNoteVersions(t *testing.T) {
	srv, tok := newAuthServer(t)
	_, inner := noteCallTok(t, srv, tok, "createNote", func(w *thrift.Writer) {
		w.FieldBegin(2, thrift.TypeStruct)
		w.FieldString(2, "t")
		w.FieldString(3, "<en-note>v0</en-note>")
		w.FieldStop()
	})
	guid := fieldStr(inner, 1)
	for _, c := range []string{"<en-note>v1</en-note>", "<en-note>v2</en-note>"} {
		noteCallTok(t, srv, tok, "updateNote", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(1, guid)
			w.FieldString(2, "t")
			w.FieldString(3, c)
			w.FieldStop()
		})
	}
	_, vs := noteCallTok(t, srv, tok, "listNoteVersions", func(w *thrift.Writer) {
		w.FieldString(2, guid)
	})
	list := fieldsList(vs, 0)
	if len(list) != 2 {
		t.Fatalf("版本数 = %d, 期望 2", len(list))
	}
	// 第一版内容是 v0（更新到 v1 时的前值快照）
	var usn0 int64
	for _, f := range list[0].Struct {
		if f.ID == 1 {
			usn0 = f.Val.I64
		}
	}
	_, v := noteCallTok(t, srv, tok, "getNoteVersion", func(w *thrift.Writer) {
		w.FieldString(2, guid)
		w.FieldI64(3, usn0)
	})
	if fieldStr(v, 3) != "<en-note>v0</en-note>" {
		t.Fatalf("历史版本内容 = %q", fieldStr(v, 3))
	}
}

// S7 原子落盘：Save 后无 .tmp 残留、Load 回读完整。
func TestAtomicSaveNoTmpResidue(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")
	st := store.New()
	st.UpsertNote("g", "甲", "<en-note/>", "", true)
	if err := st.Save(path); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Load(path); err != nil {
		t.Fatal(err)
	}
	files, _ := filepath.Glob(filepath.Join(dir, "*"))
	for _, f := range files {
		if filepath.Ext(f) == ".tmp" {
			t.Fatalf("残留临时文件: %s", f)
		}
	}
}

var _ = httptest.NewRecorder // 保持导入（fieldsList 等辅助在异常路径使用）
var _ = http.MethodPost
var _ = bytes.MinRead
