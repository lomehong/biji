package thrift

import (
	"bytes"
	"encoding/binary"
	"testing"
)

// 镜像 Rust 客户端 biji-sync::edam::TWriter 的编码断言。
func TestMessageHeaderMirror(t *testing.T) {
	var w Writer
	w.MessageBegin("getSyncState", Call, 1)
	// 版本字 80 01 00 01 + 名长 12 + getSyncState + seq 1
	want := append([]byte{0x80, 0x01, 0x00, 0x01, 0, 0, 0, 12}, []byte("getSyncState")...)
	want = append(want, 0, 0, 0, 1)
	if !bytes.Equal(w.Buf, want) {
		t.Fatalf("header mismatch:\n got %x\nwant %x", w.Buf, want)
	}
}

func TestFieldRoundtripMirror(t *testing.T) {
	// 写：f1 STR token / f2 I64 4096
	var w Writer
	w.FieldString(1, "token-abc")
	w.FieldI64(3, 4096)
	w.FieldStop()

	// 独立读取（不复用写端内部状态）
	r := &Reader{B: w.Buf}
	id1, t1, err := r.Field()
	if err != nil || id1 != 1 || t1 != TypeString {
		t.Fatalf("field1: id=%d t=%d err=%v", id1, t1, err)
	}
	s, err := r.Str()
	if err != nil || s != "token-abc" {
		t.Fatalf("str: %q %v", s, err)
	}
	id2, t2, err := r.Field()
	if err != nil || id2 != 3 || t2 != TypeI64 {
		t.Fatalf("field2: id=%d t=%d err=%v", id2, t2, err)
	}
	v, err := r.I64()
	if err != nil || v != 4096 {
		t.Fatalf("i64: %d %v", v, err)
	}
	stop, err := r.U8()
	if err != nil || stop != TypeStop {
		t.Fatalf("stop: %d %v", stop, err)
	}
}

func TestFramedRoundtripMirror(t *testing.T) {
	// 帧格式：4B 大端长度 + 消息体（EDAM servlet 实际使用）
	msg := []byte{0x80, 0x01, 0x00, 0x02, 0, 0, 0, 1}
	var framed []byte
	framed = binaryBigEndianAppendUint32(framed, uint32(len(msg)))
	framed = append(framed, msg...)

	if got := int32(binary.BigEndian.Uint32(framed[:4])); int(got) != len(msg) {
		t.Fatalf("frame len %d", got)
	}
	if !bytes.Equal(framed[4:], msg) {
		t.Fatal("body mismatch")
	}
}

func binaryBigEndianAppendUint32(b []byte, v uint32) []byte {
	return append(b, byte(v>>24), byte(v>>16), byte(v>>8), byte(v))
}
