package store

import (
	"path/filepath"
	"testing"
)

func TestUSNMonotonicAndSince(t *testing.T) {
	s := New()
	if got := s.UpsertNote("g1", "甲", "<en-note/>", "nb1", true); got != 1 {
		t.Fatalf("首个 usn = %d", got)
	}
	if got := s.UpsertNote("g2", "乙", "<en-note/>", "", true); got != 2 {
		t.Fatalf("第二个 usn = %d", got)
	}
	if got := s.UpsertNote("g1", "甲改", "<en-note>改</en-note>", "nb2", true); got != 3 {
		t.Fatalf("更新 usn = %d", got)
	}
	notes := s.NotesSince(2)
	if len(notes) != 1 || notes[0].GUID != "g1" {
		t.Fatalf("NotesSince(2) = %+v", notes)
	}
	if notes[0].NotebookGUID != "nb2" {
		t.Fatalf("NotebookGUID = %q", notes[0].NotebookGUID)
	}
	if got := s.ExpungeNote("g2"); got != 4 {
		t.Fatalf("删除 usn = %d", got)
	}
	exp := s.ExpungedSince(0)
	if len(exp) != 1 || exp[0].GUID != "g2" || exp[0].Kind != "note" {
		t.Fatalf("ExpungedSince(0) = %+v", exp)
	}
	if s.UpdateCount != 4 {
		t.Fatalf("updateCount = %d", s.UpdateCount)
	}
}

func TestNotebookTagExpunge(t *testing.T) {
	s := New()
	g := s.UpsertNotebook("nb1", "项目")
	if g != 1 {
		t.Fatalf("notebook usn = %d", g)
	}
	s.UpsertTag("t1", "重要")
	if got := s.ExpungeNotebook("nb1"); got != 3 {
		t.Fatalf("expunge notebook usn = %d", got)
	}
	kinds := map[string]bool{}
	for _, e := range s.Expunged {
		kinds[e.Kind] = true
	}
	if !kinds["notebook"] {
		t.Fatalf("删除账缺 notebook: %+v", s.Expunged)
	}
}

func TestSaveLoadRoundTrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")
	s := New()
	s.UpsertNote("g1", "甲", "<en-note/>", "nb1", true)
	s.UpsertNotebook("nb1", "项目")
	if err := s.Save(path); err != nil {
		t.Fatal(err)
	}
	loaded, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if loaded.UpdateCount != 2 {
		t.Fatalf("updateCount = %d", loaded.UpdateCount)
	}
	if n := loaded.Notes["g1"]; n == nil || n.Title != "甲" || n.Updated == 0 {
		t.Fatalf("note 往返失真: %+v", n)
	}
	if nb := loaded.Notebooks["nb1"]; nb == nil || nb.Name != "项目" {
		t.Fatalf("notebook 往返失真: %+v", nb)
	}
}

func TestReplaceResourcesRoundTrip(t *testing.T) {
	s := New()
	s.UpsertNote("g1", "甲", "<en-note/>", "nb1", true)
	payload := []byte{0x00, 0xff, 0xfe, 0x01, 0x02}
	s.ReplaceResources("g1", []ResourceInput{{Mime: "application/octet-stream", FileName: "blob.bin", Data: payload}})
	rs := s.ResourcesForNote("g1")
	if len(rs) != 1 || rs[0].GUID == "" || string(rs[0].Data) != string(payload) {
		t.Fatalf("资源失真: %+v", rs)
	}
	// 整体替换：旧资源清空
	s.ReplaceResources("g1", []ResourceInput{{Mime: "text/plain", FileName: "a.txt", Data: []byte("hi")}})
	rs = s.ResourcesForNote("g1")
	if len(rs) != 1 || rs[0].FileName != "a.txt" {
		t.Fatalf("替换后失真: %+v", rs)
	}
	if s.SnapshotUSN() != 3 { // note + res1 + res2
		t.Fatalf("updateCount = %d", s.SnapshotUSN())
	}
}
