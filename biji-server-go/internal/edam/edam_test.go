package edam

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"biji-server-go/internal/store"
	"biji-server-go/internal/thrift"
)

// 镜像客户端断言：checkVersion 免认证可达、authenticate 账密换 token、
// getSyncState 的 updateCount 编码在字段 3。
func TestCheckVersionReplyMirror(t *testing.T) {
	out := reply("evernote_checkVersion", 1, func(w *thrift.Writer) {
		w.FieldBegin(0, thrift.TypeBool)
		w.U8(1) // 兼容
		w.FieldStop()
	})
	r := &thrift.Reader{B: out}
	name, mtype, _, err := r.Message()
	if err != nil {
		t.Fatal(err)
	}
	if name != "evernote_checkVersion" {
		t.Fatalf("name %q", name)
	}
	if mtype != thrift.Reply {
		t.Fatalf("mtype %d", mtype)
	}
	fs, err := r.Fields()
	if err != nil {
		t.Fatal(err)
	}
	inner := unwrapResult(t, fs)
	found := false
	for i := range inner {
		if inner[i].ID == 0 && inner[i].Val.Bool {
			found = true
		}
	}
	if !found {
		t.Fatal("checkVersion 应返回 true")
	}
}

func TestSyncStateUpdateCountField3(t *testing.T) {
	var w thrift.Writer
	w.MessageBegin("evernote_getSyncState", thrift.Reply, 1)
	w.FieldBegin(0, thrift.TypeStruct)
	w.FieldI64(3, 6)
	w.FieldStop()
	w.U8(0)
	out := w.Buf
	r := &thrift.Reader{B: out}
	if _, mtype, _, err := r.Message(); err != nil {
		t.Fatal(err)
	} else if mtype != thrift.Reply {
		t.Fatalf("mtype %d", mtype)
	}
	fs, ferr := r.Fields()
	if ferr != nil {
		t.Fatal(ferr)
	}
	inner := unwrapResult(t, fs)
	var found *thrift.Field
	for i := range inner {
		if inner[i].ID == 3 {
			found = &inner[i]
		}
	}
	if found == nil {
		t.Fatal("缺字段 3")
	}
	if v := found.Val.I64; v != 6 {
		t.Fatalf("i64: %d", v)
	}
}

// unwrapResult 剥掉 reply() 的 STRUCT 外壳，返回内层字段。
func unwrapResult(t *testing.T, fields []thrift.Field) []thrift.Field {
	t.Helper()
	for i := range fields {
		if fields[i].ID == 0 {
			return fields[i].Val.Struct
		}
	}
	return nil
}

func TestExceptionLayout(t *testing.T) {
	out := exception("EDAM processing error")
	if !bytes.Contains(out, []byte("EDAM processing error")) {
		t.Fatal("缺异常文本")
	}
	if out[0] != 0x80 {
		t.Fatal("异常响应须为裸 Thrift 消息（镜像真实服务端实测）")
	}
}

// call 经 HTTP handler 发一次 EDAM 调用并返回响应体（参数走 f1 args 容器）。
func call(t *testing.T, h func(w http.ResponseWriter, r *http.Request), method string, build func(w *thrift.Writer)) []byte {
	t.Helper()
	var w thrift.Writer
	w.MessageBegin(method, thrift.Call, 1)
	w.FieldBegin(1, thrift.TypeStruct)
	build(&w)
	w.FieldStop()
	w.U8(0)
	req := httptest.NewRequest("POST", "http://x/edam", bytes.NewReader(w.Buf))
	rec := httptest.NewRecorder()
	h(rec, req)
	if rec.Code != 200 {
		t.Fatalf("%s HTTP %d: %s", method, rec.Code, rec.Body.String())
	}
	return rec.Body.Bytes()
}

// parseReply 解析裸消息 REPLY，返回 message 层字段；EXCEPTION 直接判失败。
func parseReply(t *testing.T, body []byte) (string, []thrift.Field) {
	t.Helper()
	rd := &thrift.Reader{B: body}
	name, mtype, _, err := rd.Message()
	if err != nil {
		t.Fatal(err)
	}
	if mtype == thrift.Exception {
		t.Fatalf("%s 返回异常: %x", name, body)
	}
	fields, err := rd.Fields()
	if err != nil {
		t.Fatal(err)
	}
	return name, fields
}

// unwrapStruct 取 message f0 的 STRUCT 内容（剥壳）；f0 非结构体时判失败。
func unwrapStruct(t *testing.T, fields []thrift.Field) []thrift.Field {
	t.Helper()
	for i := range fields {
		if fields[i].ID == 0 {
			return fields[i].Val.Struct
		}
	}
	t.Fatal("缺 result 字段 0")
	return nil
}

func fieldStr(fields []thrift.Field, id int16) string {
	for i := range fields {
		if fields[i].ID == id {
			return fields[i].Val.Str
		}
	}
	return ""
}

// fieldI64 兼容 i32/i64 读取。
func fieldI64(fields []thrift.Field, id int16) int64 {
	for i := range fields {
		if fields[i].ID == id {
			if fields[i].Val.I64 != 0 {
				return fields[i].Val.I64
			}
			return int64(fields[i].Val.I32)
		}
	}
	return 0
}

// TestFullEdamRoundTrip 走完 authenticate → getNoteStoreUrl → 笔记本/笔记全链路，
// 断言响应布局与客户端解析约定一致（单层 result struct / 裸标量）。
func TestFullEdamRoundTrip(t *testing.T) {
	st := store.New()
	srv := New(st, "u", "p", "/edam/note", filepath.Join(t.TempDir(), "state.json"))

	var token string
	t.Run("authenticate", func(t *testing.T) {
		_, fields := parseReply(t, call(t, srv.ServeUserStore, "evernote_authenticate", func(w *thrift.Writer) {
			w.FieldString(1, "u")
			w.FieldString(2, "p")
		}))
		token = fieldStr(unwrapStruct(t, fields), 2)
		if token == "" {
			t.Fatal("缺 token")
		}
	})
	t.Run("authenticate 拒绝错密码", func(t *testing.T) {
		body := call(t, srv.ServeUserStore, "evernote_authenticate", func(w *thrift.Writer) {
			w.FieldString(1, "u")
			w.FieldString(2, "wrong")
		})
		rd := &thrift.Reader{B: body}
		if _, mtype, _, _ := rd.Message(); mtype != thrift.Exception {
			t.Fatal("错密码应返回异常")
		}
	})

	t.Run("getNoteStoreUrl", func(t *testing.T) {
		_, fields := parseReply(t, call(t, srv.ServeUserStore, "evernote_getNoteStoreUrl", func(w *thrift.Writer) {
			w.FieldString(1, token)
		}))
		if got := fieldStr(unwrapStruct(t, fields), 0); got != "http://x/edam/note" {
			t.Fatalf("noteStore = %q", got)
		}
	})
	t.Run("getNoteStoreUrl 拒绝无效 token", func(t *testing.T) {
		body := call(t, srv.ServeUserStore, "evernote_getNoteStoreUrl", func(w *thrift.Writer) {
			w.FieldString(1, "bogus")
		})
		rd := &thrift.Reader{B: body}
		if _, mtype, _, _ := rd.Message(); mtype != thrift.Exception {
			t.Fatal("无效 token 应返回异常")
		}
	})

	// NoteStore 调用统一在参数容器 f1 放 token，返回 (message 层字段, 剥壳后字段)
	noteCall := func(method string, build func(w *thrift.Writer)) ([]thrift.Field, []thrift.Field) {
		_, msgFields := parseReply(t, call(t, srv.ServeNoteStore, method, func(w *thrift.Writer) {
			w.FieldString(1, token)
			build(w)
		}))
		return msgFields, unwrapStruct(t, msgFields)
	}

	var nbGUID, noteGUID string
	t.Run("createNotebook", func(t *testing.T) {
		_, inner := noteCall("evernote_createNotebook", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(2, "项目")
			w.FieldStop()
		})
		nbGUID = fieldStr(inner, 1)
		if nbGUID == "" {
			t.Fatalf("缺 notebook guid: %+v", inner)
		}
		if fieldI64(inner, 5) == 0 {
			t.Fatal("缺 updateSequenceNum=5")
		}
	})
	t.Run("updateNotebook 裸 i32", func(t *testing.T) {
		msgFields, _ := noteCall("evernote_updateNotebook", func(w *thrift.Writer) {
			w.FieldString(2, nbGUID)
			w.FieldBegin(3, thrift.TypeStruct)
			w.FieldString(2, "项目改")
			w.FieldStop()
		})
		if got := fieldI64(msgFields, 0); got == 0 {
			t.Fatalf("updateNotebook 应返回裸 f0 usn: %+v", msgFields)
		}
		if st.Notebooks[nbGUID].Name != "项目改" {
			t.Fatal("名称未更新")
		}
	})
	t.Run("createNote", func(t *testing.T) {
		_, inner := noteCall("evernote_createNote", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(2, "标题")
			w.FieldString(3, "<en-note>正文</en-note>")
			w.FieldString(11, nbGUID) // Note.notebookGuid
			// f13 resources：二进制 body 保真
			w.FieldListBegin(13, thrift.TypeStruct, 1)
			w.FieldBegin(3, thrift.TypeStruct) // Data{body,size,mime}
			w.FieldString(1, string([]byte{0x00, 0xff, 0xfe, 0x7f}))
			w.FieldString(4, "application/octet-stream")
			w.FieldStop()
			w.FieldString(4, "application/octet-stream")
			w.FieldBegin(14, thrift.TypeStruct) // attributes{fileName}
			w.FieldString(1, "blob.bin")
			w.FieldStop()
			w.FieldStop()
			w.FieldBool(9, true)
			w.FieldStop()
		})
		noteGUID = fieldStr(inner, 1)
		if noteGUID == "" || fieldI64(inner, 10) == 0 {
			t.Fatalf("缺 guid/usn: %+v", inner)
		}
		if rs := st.ResourcesForNote(noteGUID); len(rs) != 1 || rs[0].FileName != "blob.bin" {
			t.Fatalf("资源未入库: %+v", rs)
		} else if got := rs[0].Data; len(got) != 4 || got[1] != 0xff || got[2] != 0xfe {
			t.Fatalf("资源字节失真: %x", got)
		}
	})
	t.Run("getSyncState 与块拉取", func(t *testing.T) {
		_, inner := noteCall("evernote_getSyncState", func(w *thrift.Writer) {})
		if fieldI64(inner, 3) == 0 {
			t.Fatalf("缺 updateCount f3: %+v", inner)
		}
		_, chunk := noteCall("evernote_getFilteredSyncChunk", func(w *thrift.Writer) {
			w.FieldI32(2, 0)
		})
		if fieldI64(chunk, 3) == 0 {
			t.Fatal("缺 chunk updateCount")
		}
		// f4 notes 首元素应回传笔记本归属；f5 notebooks 应回传笔记本本体
		var sawNote, sawNotebook bool
		for i := range chunk {
			switch chunk[i].ID {
			case 4:
				notes := chunk[i].Val.List
				if len(notes) == 0 {
					t.Fatal("chunk 缺笔记")
				}
				if got := fieldStr(notes[0].Struct, 11); got != nbGUID {
					t.Fatalf("note notebookGuid = %q, 期望 %q", got, nbGUID)
				}
				// f13 资源回传：guid 非空 + 字节保真
				var sawRes bool
				for _, el := range notes[0].Struct {
					if el.ID == 13 {
						if len(el.Val.List) != 1 {
							t.Fatal("chunk 缺资源")
						}
						res := el.Val.List[0].Struct
						var dataID string
						for _, rf := range res {
							if rf.ID == 1 && rf.Val.Str == "" {
								t.Fatal("资源 guid 为空")
							}
							if rf.ID == 3 {
								for _, df := range rf.Val.Struct {
									if df.ID == 1 {
										dataID = df.Val.Str
									}
								}
							}
						}
						if len(dataID) != 4 || dataID[0] != 0x00 || dataID[1] != 0xff || dataID[2] != 0xfe || dataID[3] != 0x7f {
							t.Fatalf("资源字节失真: %q", dataID)
						}
						sawRes = true
					}
				}
				if !sawRes {
					t.Fatal("note 未回传 f13 资源")
				}
				sawNote = true
			case 5:
				nbs := chunk[i].Val.List
				if len(nbs) == 0 || fieldStr(nbs[0].Struct, 1) != nbGUID || fieldStr(nbs[0].Struct, 2) != "项目改" {
					t.Fatalf("chunk notebooks 失真: %+v", nbs)
				}
				sawNotebook = true
			}
		}
		if !sawNote || !sawNotebook {
			t.Fatalf("chunk 缺字段（note=%v notebook=%v）", sawNote, sawNotebook)
		}
	})
	t.Run("expungeNote 裸 i64 + 删除账", func(t *testing.T) {
		msgFields, _ := noteCall("evernote_expungeNote", func(w *thrift.Writer) {
			w.FieldString(2, noteGUID)
		})
		if got := fieldI64(msgFields, 0); got == 0 {
			t.Fatalf("expungeNote 应返回裸 f0 usn: %+v", msgFields)
		}
		if len(st.Expunged) == 0 {
			t.Fatal("删除账为空")
		}
	})
	t.Run("createTag", func(t *testing.T) {
		_, inner := noteCall("evernote_createTag", func(w *thrift.Writer) {
			w.FieldBegin(2, thrift.TypeStruct)
			w.FieldString(2, "重要")
			w.FieldStop()
		})
		if fieldStr(inner, 1) == "" {
			t.Fatalf("缺 tag guid: %+v", inner)
		}
	})
}
