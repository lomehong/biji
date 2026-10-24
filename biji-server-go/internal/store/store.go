// Package store 维护服务端内存笔记模型与 JSON 落盘持久化。
// 同步语义与 EDAM 一致：updateCount 单调递增、每对象独立 USN、删除账。
// 落盘为原子写（temp + rename），会话 token 随状态持久（重启不失效）。
package store

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"os"
	"sort"
	"strings"
	"sync"
	"time"
)

// NoteVersion 笔记历史版本快照（updateNote 时保留前值，上限 maxVersions）。
type NoteVersion struct {
	USN     int64  `json:"usn"`
	Updated int64  `json:"updated"`
	Title   string `json:"title"`
	Content string `json:"content"`
}

const maxVersions = 10

// Note 服务端笔记。
type Note struct {
	GUID         string        `json:"guid"`
	Title        string        `json:"title"`
	Content      string        `json:"content"`
	NotebookGUID string        `json:"notebook_guid"`
	Active       bool          `json:"active"`
	Updated      int64         `json:"updated"`
	USN          int64         `json:"usn"`
	Versions     []NoteVersion `json:"versions,omitempty"`
}

// ResourceInput 上行附件（无 guid，新建由服务端分配）。
type ResourceInput struct {
	Mime     string
	FileName string
	Data     []byte
}

// Resource 服务端附件资源。
type Resource struct {
	GUID     string `json:"guid"`
	NoteGUID string `json:"note_guid"`
	Mime     string `json:"mime"`
	FileName string `json:"file_name"`
	Data     []byte `json:"data"`
	USN      int64  `json:"usn"`
}

// Notebook 服务端笔记本（stack 为分组，对位 EDAM Notebook.stack）。
type Notebook struct {
	GUID  string `json:"guid"`
	Name  string `json:"name"`
	Stack string `json:"stack,omitempty"`
	USN   int64  `json:"usn"`
}

// Tag 服务端标签。
type Tag struct {
	GUID string `json:"guid"`
	Name string `json:"name"`
	USN  int64  `json:"usn"`
}

// Expunged 删除账条目。
type Expunged struct {
	GUID string `json:"guid"`
	Kind string `json:"kind"` // note | notebook | tag
	USN  int64  `json:"usn"`
}

// Store 线程安全的内存笔记服务端状态（单用户一份）。
type Store struct {
	mu          sync.Mutex
	UpdateCount int64            `json:"update_count"`
	Notes       map[string]*Note `json:"notes"`
	Notebooks   map[string]*Notebook
	Tags        map[string]*Tag
	Resources   map[string]*Resource
	Expunged    []Expunged      `json:"expunged"`
	Tokens      map[string]bool `json:"tokens"` // 持久会话（重启恢复）
}

func NewGUID() string {
	b := make([]byte, 16)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

// New 空服务端状态。
func New() *Store {
	return &Store{
		Notes:     map[string]*Note{},
		Notebooks: map[string]*Notebook{},
		Tags:      map[string]*Tag{},
		Resources: map[string]*Resource{},
		Tokens:    map[string]bool{},
	}
}

// Load 从 JSON 文件恢复；文件不存在时返回新状态。
func Load(path string) (*Store, error) {
	s := New()
	b, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return s, nil
		}
		return nil, err
	}
	if err := json.Unmarshal(b, s); err != nil {
		return nil, err
	}
	if s.Notes == nil {
		s.Notes = map[string]*Note{}
	}
	if s.Notebooks == nil {
		s.Notebooks = map[string]*Notebook{}
	}
	if s.Tags == nil {
		s.Tags = map[string]*Tag{}
	}
	if s.Resources == nil {
		s.Resources = map[string]*Resource{}
	}
	if s.Tokens == nil {
		s.Tokens = map[string]bool{}
	}
	return s, nil
}

// Save 原子落盘：先写临时文件再 rename（Windows 上 rename 不覆盖已存在
// 目标，先删除旧文件；掉电至多丢一次更新，不会截断既有状态）。
func (s *Store) Save(path string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		return err
	}
	return os.Rename(tmp, path)
}

// NextUSN 递增并返回全局更新计数。
func (s *Store) NextUSN() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	return s.UpdateCount
}

// SnapshotUSN 当前更新计数（只读快照，供测试）。
func (s *Store) SnapshotUSN() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.UpdateCount
}

// UpsertNote 创建或更新笔记（usn 单调；更新时保留前值快照进版本历史），
// 返回对象 usn。
func (s *Store) UpsertNote(guid, title, content, notebookGUID string, active bool) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	n, ok := s.Notes[guid]
	if !ok {
		n = &Note{GUID: guid}
		s.Notes[guid] = n
	} else if n.Title != title || n.Content != content {
		// 版本历史：保留被覆盖的前值（内容未变不产生新版本）
		snapshot := NoteVersion{USN: n.USN, Updated: n.Updated, Title: n.Title, Content: n.Content}
		n.Versions = append(n.Versions, snapshot)
		if len(n.Versions) > maxVersions {
			n.Versions = n.Versions[len(n.Versions)-maxVersions:]
		}
	}
	n.Title, n.Content, n.NotebookGUID, n.Active, n.USN = title, content, notebookGUID, active, usn
	n.Updated = time.Now().UnixMilli()
	return usn
}

// ExpungeNote 删除笔记并记删除账；返回对象 usn（不存在返回记账 usn）。
func (s *Store) ExpungeNote(guid string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	if n, ok := s.Notes[guid]; ok {
		n.Active = false
		n.USN = usn
	}
	s.Expunged = append(s.Expunged, Expunged{GUID: guid, Kind: "note", USN: usn})
	return usn
}

// UpsertNotebook 创建或更新笔记本（stack 为分组，可空）。
func (s *Store) UpsertNotebook(guid, name, stack string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	nb := s.Notebooks[guid]
	if nb == nil {
		nb = &Notebook{GUID: guid}
		s.Notebooks[guid] = nb
	}
	nb.Name, nb.Stack, nb.USN = name, stack, usn
	return usn
}

// ExpungeNotebook 删除笔记本。
func (s *Store) ExpungeNotebook(guid string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	delete(s.Notebooks, guid)
	s.Expunged = append(s.Expunged, Expunged{GUID: guid, Kind: "notebook", USN: usn})
	return usn
}

// UpsertTag 创建或更新标签。
func (s *Store) UpsertTag(guid, name string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	s.Tags[guid] = &Tag{GUID: guid, Name: name, USN: usn}
	return usn
}

// ExpungeTag 删除标签。
func (s *Store) ExpungeTag(guid string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	delete(s.Tags, guid)
	s.Expunged = append(s.Expunged, Expunged{GUID: guid, Kind: "tag", USN: usn})
	return usn
}

// NotesSince 返回 usn 大于 after 的笔记快照（按 USN 升序——分页确定性）。
func (s *Store) NotesSince(after int64) []*Note {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Note
	for _, n := range s.Notes {
		if n.USN > after {
			out = append(out, snapshotNote(n))
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].USN < out[j].USN })
	return out
}

func snapshotNote(n *Note) *Note {
	cp := *n
	return &cp
}

// ReplaceResources 替换笔记的附件资源集（f13 存在时整体替换）。
func (s *Store) ReplaceResources(noteGUID string, inputs []ResourceInput) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	for g, r := range s.Resources {
		if r.NoteGUID == noteGUID {
			delete(s.Resources, g)
		}
	}
	var usn int64
	for _, in := range inputs {
		s.UpdateCount++
		usn = s.UpdateCount
		guid := NewGUID()
		s.Resources[guid] = &Resource{
			GUID:     guid,
			NoteGUID: noteGUID,
			Mime:     in.Mime,
			FileName: in.FileName,
			Data:     in.Data,
			USN:      usn,
		}
	}
	return usn
}

// UpsertResource 单附件上传（putResource 扩展）：按指定 guid 新增或替换
// 该笔记的一个附件（不动其他附件），并推进所属笔记 USN（chunk 重新下发
// 元数据）。返回对象 usn。
func (s *Store) UpsertResource(noteGUID, guid, mime, fileName string, data []byte) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	if old, ok := s.Resources[guid]; ok && old.NoteGUID != noteGUID {
		return 0 // guid 属于其他笔记：拒绝（不静默搬家）
	}
	s.UpdateCount++
	usn := s.UpdateCount
	s.Resources[guid] = &Resource{
		GUID:     guid,
		NoteGUID: noteGUID,
		Mime:     mime,
		FileName: fileName,
		Data:     data,
		USN:      usn,
	}
	if n, ok := s.Notes[noteGUID]; ok {
		n.USN = usn // 笔记元数据需重新同步
		n.Updated = time.Now().UnixMilli()
	}
	return usn
}

// ResourcesForNote 返回笔记的附件快照。
func (s *Store) ResourcesForNote(noteGUID string) []*Resource {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Resource
	for _, r := range s.Resources {
		if r.NoteGUID == noteGUID {
			cp := *r
			out = append(out, &cp)
		}
	}
	return out
}

// ResourceForGuid 按 guid 取单个附件（getResource 按需拉取用）；不存在返回 nil。
func (s *Store) ResourceForGuid(guid string) *Resource {
	s.mu.Lock()
	defer s.mu.Unlock()
	if r, ok := s.Resources[guid]; ok {
		cp := *r
		return &cp
	}
	return nil
}

// NoteForGuid 按 guid 取笔记快照（getNote 用）；不存在返回 nil。
func (s *Store) NoteForGuid(guid string) *Note {
	s.mu.Lock()
	defer s.mu.Unlock()
	if n, ok := s.Notes[guid]; ok {
		return snapshotNote(n)
	}
	return nil
}

// NotebookForGuid / TagForGuid 单查（getNotebook/getTag 用）。
func (s *Store) NotebookForGuid(guid string) *Notebook {
	s.mu.Lock()
	defer s.mu.Unlock()
	if nb, ok := s.Notebooks[guid]; ok {
		cp := *nb
		return &cp
	}
	return nil
}

func (s *Store) TagForGuid(guid string) *Tag {
	s.mu.Lock()
	defer s.mu.Unlock()
	if tg, ok := s.Tags[guid]; ok {
		cp := *tg
		return &cp
	}
	return nil
}

// ListNotebooks / ListTags 全量快照（listNotebooks/listTags 用，USN 升序）。
func (s *Store) ListNotebooks() []*Notebook {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Notebook
	for _, nb := range s.Notebooks {
		cp := *nb
		out = append(out, &cp)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].USN < out[j].USN })
	return out
}

func (s *Store) ListTags() []*Tag {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Tag
	for _, tg := range s.Tags {
		cp := *tg
		out = append(out, &cp)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].USN < out[j].USN })
	return out
}

// SearchNotes 服务端搜索（findNotes）：词间 AND、命中标题或正文（大小写
// 不敏感）、仅 active 笔记、USN 降序（新变更在前）。返回命中总数与分页。
func (s *Store) SearchNotes(words []string, offset, max int) (int, []*Note) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var hits []*Note
	for _, n := range s.Notes {
		if !n.Active {
			continue
		}
		hay := strings.ToLower(n.Title + "\n" + n.Content)
		ok := true
		for _, w := range words {
			if w != "" && !strings.Contains(hay, strings.ToLower(w)) {
				ok = false
				break
			}
		}
		if ok {
			hits = append(hits, snapshotNote(n))
		}
	}
	sort.Slice(hits, func(i, j int) bool { return hits[i].USN > hits[j].USN })
	total := len(hits)
	if offset > 0 {
		if offset >= total {
			return total, nil
		}
		hits = hits[offset:]
	}
	if max > 0 && len(hits) > max {
		hits = hits[:max]
	}
	return total, hits
}

// VersionsOf / VersionByUSN 笔记版本历史（listNoteVersions/getNoteVersion 用）。
func (s *Store) VersionsOf(guid string) []NoteVersion {
	s.mu.Lock()
	defer s.mu.Unlock()
	if n, ok := s.Notes[guid]; ok {
		out := make([]NoteVersion, len(n.Versions))
		copy(out, n.Versions)
		return out
	}
	return nil
}

func (s *Store) VersionByUSN(guid string, usn int64) *NoteVersion {
	s.mu.Lock()
	defer s.mu.Unlock()
	n, ok := s.Notes[guid]
	if !ok {
		return nil
	}
	for i := range n.Versions {
		if n.Versions[i].USN == usn {
			cp := n.Versions[i]
			return &cp
		}
	}
	return nil
}

// NotebooksSince 返回 usn 大于 after 的笔记本快照（USN 升序）。
func (s *Store) NotebooksSince(after int64) []*Notebook {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Notebook
	for _, nb := range s.Notebooks {
		if nb.USN > after {
			cp := *nb
			out = append(out, &cp)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].USN < out[j].USN })
	return out
}

// TagsSince 返回 usn 大于 after 的标签快照（USN 升序）。
func (s *Store) TagsSince(after int64) []*Tag {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Tag
	for _, tg := range s.Tags {
		if tg.USN > after {
			cp := *tg
			out = append(out, &cp)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].USN < out[j].USN })
	return out
}

// ExpungedSince 返回 usn 大于 after 的删除账条目。
func (s *Store) ExpungedSince(after int64) []Expunged {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []Expunged
	for _, e := range s.Expunged {
		if e.USN > after {
			out = append(out, e)
		}
	}
	return out
}

// TokenPut / TokenValid 持久会话（authenticate 铸造、NoteStore 校验）。
func (s *Store) TokenPut(tok string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Tokens[tok] = true
}

func (s *Store) TokenValid(tok string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.Tokens[tok]
}
