// Package store 维护服务端内存笔记模型与 JSON 落盘持久化。
// 同步语义与 EDAM 一致：updateCount 单调递增、每对象独立 USN、删除账。
package store

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"os"
	"sync"
	"time"
)

// Note 服务端笔记。
type Note struct {
	GUID         string `json:"guid"`
	Title        string `json:"title"`
	Content      string `json:"content"`
	NotebookGUID string `json:"notebook_guid"`
	Active       bool   `json:"active"`
	Updated      int64  `json:"updated"`
	USN          int64  `json:"usn"`
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

// Notebook 服务端笔记本。
type Notebook struct {
	GUID string `json:"guid"`
	Name string `json:"name"`
	USN  int64  `json:"usn"`
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

// Store 线程安全的内存笔记服务端状态。
type Store struct {
	mu          sync.Mutex
	UpdateCount int64            `json:"update_count"`
	Notes       map[string]*Note `json:"notes"`
	Notebooks   map[string]*Notebook
	Tags        map[string]*Tag
	Resources   map[string]*Resource
	Expunged    []Expunged `json:"expunged"`
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
	return s, nil
}

// Save 落盘为 JSON。
func (s *Store) Save(path string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, b, 0o644)
}

// NextUSN 递增并返回全局更新计数。
func (s *Store) NextUSN() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	return s.UpdateCount
}

// SnapshotUSN 当前更新计数（不加锁只读快照，供测试）。
func (s *Store) SnapshotUSN() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.UpdateCount
}

// UpsertNote 创建或更新笔记（usn 单调），返回对象 usn。
func (s *Store) UpsertNote(guid, title, content, notebookGUID string, active bool) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	n, ok := s.Notes[guid]
	if !ok {
		n = &Note{GUID: guid}
		s.Notes[guid] = n
	}
	n.Title, n.Content, n.NotebookGUID, n.Active, n.USN = title, content, notebookGUID, active, usn
	n.Updated = time.Now().UnixMilli()
	return usn
}

// ExpungeNote 删除笔记并记删除账；返回对象 usn（不存在返回 0）。
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

// UpsertNotebook 创建或更新笔记本。
func (s *Store) UpsertNotebook(guid, name string) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.UpdateCount++
	usn := s.UpdateCount
	s.Notebooks[guid] = &Notebook{GUID: guid, Name: name, USN: usn}
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

// NotesSince 返回 usn 大于 after 的笔记快照。
func (s *Store) NotesSince(after int64) []*Note {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Note
	for _, n := range s.Notes {
		if n.USN > after {
			out = append(out, n)
		}
	}
	return out
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

// ResourcesForNote 返回笔记的附件快照。
func (s *Store) ResourcesForNote(noteGUID string) []*Resource {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Resource
	for _, r := range s.Resources {
		if r.NoteGUID == noteGUID {
			out = append(out, r)
		}
	}
	return out
}

// NotebooksSince 返回 usn 大于 after 的笔记本快照。
func (s *Store) NotebooksSince(after int64) []*Notebook {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []*Notebook
	for _, nb := range s.Notebooks {
		if nb.USN > after {
			out = append(out, nb)
		}
	}
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
