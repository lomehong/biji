// Package thrift 实现 EDAM 所需的 Thrift 二进制协议编解码
// （严格版本消息头 + 字段递归），与 Rust 客户端 biji-sync::edam 镜像。
package thrift

import (
	"encoding/binary"
	"errors"
	"fmt"
)

// Thrift 消息类型。
const (
	Call      = 1
	Reply     = 2
	Exception = 3
)

// Thrift 值类型。
const (
	TypeStop   = 0
	TypeBool   = 2
	TypeI16    = 6
	TypeI32    = 8
	TypeI64    = 10
	TypeString = 11
	TypeStruct = 12
	TypeList   = 15
)

const VersionOne = int32(-2147418112) // 0x80010000

// Field 是 struct 内的一个字段。
type Field struct {
	ID  int16
	Val TVal
}

// TVal 是 Thrift 值的通用表示。
type TVal struct {
	Bool   bool
	I32    int32
	I64    int64
	Str    string
	List   []TVal
	Struct []Field
}

// AsI64 取整数值（I32/I64 皆可）。
func (v TVal) AsI64() int64 {
	if v.I64 != 0 {
		return v.I64
	}
	return int64(v.I32)
}

// AsStr 取字符串。
func (v TVal) AsStr() string { return v.Str }

// AsBool 取布尔。
func (v TVal) AsBool() bool { return v.Bool }

// AsList 取列表。
func (v TVal) AsList() []TVal { return v.List }

// AsStruct 取嵌套字段。
func (v TVal) AsStruct() []Field { return v.Struct }

// Writer 累积响应消息字节。
type Writer struct{ Buf []byte }

func (w *Writer) U8(v byte)       { w.Buf = append(w.Buf, v) }
func (w *Writer) I16(v int16)     { w.Buf = binary.BigEndian.AppendUint16(w.Buf, uint16(v)) }
func (w *Writer) I32(v int32)     { w.Buf = binary.BigEndian.AppendUint32(w.Buf, uint32(v)) }
func (w *Writer) I64(v int64)     { w.Buf = binary.BigEndian.AppendUint64(w.Buf, uint64(v)) }
func (w *Writer) Bytes(b []byte)  { w.Buf = append(w.Buf, b...) }
func (w *Writer) String(s string) { w.I32(int32(len(s))); w.Buf = append(w.Buf, s...) }

func (w *Writer) FieldBegin(id int16, t byte) {
	w.U8(t)
	var b2 [2]byte
	binary.BigEndian.PutUint16(b2[:], uint16(id))
	w.Buf = append(w.Buf, b2[:]...)
}

func (w *Writer) FieldStop() { w.U8(TypeStop) }

func (w *Writer) FieldString(id int16, s string) {
	w.FieldBegin(id, TypeString)
	w.String(s)
}

func (w *Writer) FieldI64(id int16, v int64) {
	w.FieldBegin(id, TypeI64)
	w.I64(v)
}

func (w *Writer) FieldI32(id int16, v int32) {
	w.FieldBegin(id, TypeI32)
	w.I32(v)
}

func (w *Writer) FieldBool(id int16, v bool) {
	w.FieldBegin(id, TypeBool)
	if v {
		w.U8(1)
	} else {
		w.U8(0)
	}
}

func (w *Writer) FieldStructBegin(id int16) { w.FieldBegin(id, TypeStruct) }
func (w *Writer) FieldListBegin(id int16, elem byte, n int) {
	w.FieldBegin(id, TypeList)
	w.U8(elem)
	w.I32(int32(n))
}

// MessageBegin 写严格版本头 + 名 + 序号。
func (w *Writer) MessageBegin(name string, msgType byte, seqID int32) {
	w.I32(VersionOne | int32(msgType))
	w.String(name)
	w.I32(seqID)
}

// Reader 顺序读取请求字节。
type Reader struct {
	B []byte
	P int
}

var ErrTruncated = errors.New("thrift: truncated message")

func (r *Reader) U8() (byte, error) {
	if r.P >= len(r.B) {
		return 0, ErrTruncated
	}
	v := r.B[r.P]
	r.P++
	return v, nil
}

func (r *Reader) Fixed(n int) ([]byte, error) {
	if r.P+n > len(r.B) {
		return nil, fmt.Errorf("%w: need %d bytes at %d", ErrTruncated, n, r.P)
	}
	v := r.B[r.P : r.P+n]
	r.P += n
	return v, nil
}

func (r *Reader) I32() (int32, error) {
	b, err := r.Fixed(4)
	if err != nil {
		return 0, err
	}
	return int32(binary.BigEndian.Uint32(b)), nil
}

func (r *Reader) I64() (int64, error) {
	b, err := r.Fixed(8)
	if err != nil {
		return 0, err
	}
	return int64(binary.BigEndian.Uint64(b)), nil
}

func (r *Reader) Str() (string, error) {
	n, err := r.I32()
	if err != nil {
		return "", err
	}
	if n < 0 {
		return "", fmt.Errorf("%w: negative string length %d", ErrTruncated, n)
	}
	b, err := r.Fixed(int(n))
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// Message 解析请求消息头，返回方法名、类型、序号。
func (r *Reader) Message() (name string, msgType byte, seq int32, err error) {
	ver, err := r.I32()
	if err != nil {
		return
	}
	if ver & ^0x00ff != VersionOne { // 高 16 位须为 0x8001
		err = fmt.Errorf("bad thrift version: %08x", ver)
		return
	}
	msgType = byte(ver & 0xff)
	name, err = r.Str()
	if err != nil {
		return
	}
	seq, err = r.I32()
	return
}

// Field 解析一个字段头，返回 id 与类型。
func (r *Reader) Field() (id int16, t byte, err error) {
	t, err = r.U8()
	if err != nil || t == TypeStop {
		return
	}
	b2, err := r.Fixed(2)
	if err != nil {
		return
	}
	id = int16(binary.BigEndian.Uint16(b2))
	return
}

// I16 读取 2 字节大端整数。
func (r *Reader) I16() (int16, error) {
	b, err := r.Fixed(2)
	if err != nil {
		return 0, err
	}
	return int16(binary.BigEndian.Uint16(b)), nil
}

// Fields 解析消息级/嵌套字段序列（直到 STOP）。
func (r *Reader) Fields() ([]Field, error) {
	var out []Field
	for {
		t, err := r.U8()
		if err != nil {
			return nil, err
		}
		if t == TypeStop {
			return out, nil
		}
		id, err := r.I16()
		if err != nil {
			return nil, err
		}
		val, err := r.Value(t)
		if err != nil {
			return nil, err
		}
		out = append(out, Field{ID: id, Val: val})
	}
}

// Value 按类型解析一个值。
func (r *Reader) Value(t byte) (TVal, error) {
	switch t {
	case TypeBool:
		b, err := r.U8()
		return TVal{Bool: b == 1}, err
	case TypeI16:
		b, err := r.Fixed(2)
		if err != nil {
			return TVal{}, err
		}
		return TVal{I32: int32(int16(binary.BigEndian.Uint16(b)))}, nil
	case TypeI32:
		v, err := r.I32()
		return TVal{I32: v}, err
	case TypeI64:
		v, err := r.I64()
		return TVal{I64: v}, err
	case TypeString:
		s, err := r.Str()
		return TVal{Str: s}, err
	case TypeStruct:
		fs, err := r.Fields()
		return TVal{Struct: fs}, err
	case TypeList:
		et, err := r.U8()
		if err != nil {
			return TVal{}, err
		}
		n, err := r.I32()
		if err != nil {
			return TVal{}, err
		}
		var items []TVal
		for i := 0; i < int(n); i++ {
			v, err := r.Value(et)
			if err != nil {
				return TVal{}, err
			}
			items = append(items, v)
		}
		return TVal{List: items}, nil
	}
	return TVal{}, fmt.Errorf("thrift: unsupported value type %d", t)
}

// Skip 跳过一个指定类型的值（未知字段旁路）。
func (r *Reader) Skip(t byte) error {
	switch t {
	case TypeBool:
		_, err := r.U8()
		return err
	case TypeI32:
		_, err := r.I32()
		return err
	case TypeI64:
		_, err := r.I64()
		return err
	case TypeString:
		_, err := r.Str()
		return err
	case TypeStruct:
		return r.skipStruct()
	case TypeList:
		et, err := r.U8()
		if err != nil {
			return err
		}
		n, err := r.I32()
		if err != nil {
			return err
		}
		for i := 0; i < int(n); i++ {
			if err := r.Skip(et); err != nil {
				return err
			}
		}
		return nil
	}
	return fmt.Errorf("thrift: cannot skip type %d", t)
}

func (r *Reader) skipStruct() error {
	for {
		t, err := r.U8()
		if err != nil {
			return err
		}
		if t == TypeStop {
			return nil
		}
		if _, err := r.Fixed(2); err != nil {
			return err
		}
		if err := r.Skip(t); err != nil {
			return err
		}
	}
}
