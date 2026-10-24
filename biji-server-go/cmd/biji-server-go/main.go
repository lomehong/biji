// biji-server-go：自研笔记服务端（EDAM Thrift over HTTP 的同步子集）。
// 与 Rust 复刻客户端 biji-cli/biji-gui 的 biji-sync::edam 直接对接。
//
// 用法：biji-server-go [-addr 127.0.0.1:8332] [-data server-state.json]
//
//	认证用户名/密码经 BIJI_SERVER_USER / BIJI_SERVER_PASS 注入（默认 biji/biji）。
package main

import (
	"flag"
	"log"
	"net/http"
	"os"

	"biji-server-go/internal/edam"
	"biji-server-go/internal/store"
)

func main() {
	addr := flag.String("addr", "127.0.0.1:8332", "HTTP 监听地址")
	dataPath := flag.String("data", "server-state.json", "状态落盘文件")
	// 凭证只从环境变量读（默认仅供本机联调），永不写入代码或日志
	user := flag.String("user", envOr("BIJI_SERVER_USER", "biji"), "认证用户名（BIJI_SERVER_USER）")
	pass := flag.String("pass", envOr("BIJI_SERVER_PASS", "biji"), "认证密码（BIJI_SERVER_PASS）")
	flag.Parse()

	st, err := store.Load(*dataPath)
	if err != nil {
		log.Fatalf("加载状态失败: %v", err)
	}
	srv := edam.New(st, *user, *pass, "/edam/note", *dataPath)
	http.HandleFunc("/edam/user", srv.ServeUserStore)
	http.HandleFunc("/edam/note", srv.ServeNoteStore)

	log.Printf("biji-server-go 监听 %s（用户 %s，状态 %s）", *addr, *user, *dataPath)
	log.Fatal(http.ListenAndServe(*addr, nil))
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
