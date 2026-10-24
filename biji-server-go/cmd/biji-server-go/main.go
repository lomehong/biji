// biji-server-go：自研笔记服务端（EDAM 风格 Thrift over HTTP）。
// 与 Rust 复刻客户端 biji-cli/biji-gui 的 biji-sync::edam 直接对接。
//
// 用法：biji-server-go [-addr 127.0.0.1:8332] [-data server-state.json]
//
//	[-cert cert.pem -key key.pem]
//
// 认证（两种模式二选一）：
//   - 单用户（默认）：BIJI_SERVER_USER / BIJI_SERVER_PASS（默认 biji/biji），
//     状态落单文件；
//   - 多用户：BIJI_SERVER_USERS="alice:pw1,bob:pw2"——每用户独立状态文件
//     （server-state.json → server-state.<user>.json）与独立 USN 记账。
// 可选 -cert/-key 启用 TLS（自签或正规证书；公网部署也可用反代终结 TLS）。
package main

import (
	"flag"
	"log"
	"net/http"
	"os"
	"strings"

	"biji-server-go/internal/edam"
	"biji-server-go/internal/store"
)

func main() {
	addr := flag.String("addr", "127.0.0.1:8332", "HTTP 监听地址")
	dataPath := flag.String("data", "server-state.json", "状态落盘文件")
	certFile := flag.String("cert", "", "TLS 证书 PEM（启用 HTTPS）")
	keyFile := flag.String("key", "", "TLS 私钥 PEM（启用 HTTPS）")
	// 凭证只从环境变量读（默认仅供本机联调），永不写入代码或日志
	user := flag.String("user", envOr("BIJI_SERVER_USER", "biji"), "认证用户名（BIJI_SERVER_USER；多用户模式忽略）")
	pass := flag.String("pass", envOr("BIJI_SERVER_PASS", "biji"), "认证密码（BIJI_SERVER_PASS；多用户模式忽略）")
	flag.Parse()

	var srv *edam.Server
	if usersSpec := os.Getenv("BIJI_SERVER_USERS"); usersSpec != "" {
		users := map[string]string{}
		for _, pair := range strings.Split(usersSpec, ",") {
			parts := strings.SplitN(strings.TrimSpace(pair), ":", 2)
			if len(parts) == 2 && parts[0] != "" && parts[1] != "" {
				users[parts[0]] = parts[1]
			}
		}
		if len(users) == 0 {
			log.Fatal("BIJI_SERVER_USERS 格式错误，应为 user1:pass1,user2:pass2")
		}
		srv = edam.NewMulti(users, "/edam/note", *dataPath)
		log.Printf("biji-server-go 监听 %s（多用户 %d 个，状态模板 %s）",
			*addr, len(users), *dataPath)
	} else {
		st, err := store.Load(*dataPath)
		if err != nil {
			log.Fatalf("加载状态失败: %v", err)
		}
		srv = edam.New(st, *user, *pass, "/edam/note", *dataPath)
		log.Printf("biji-server-go 监听 %s（用户 %s，状态 %s）", *addr, *user, *dataPath)
	}
	http.HandleFunc("/edam/user", srv.ServeUserStore)
	http.HandleFunc("/edam/note", srv.ServeNoteStore)

	if *certFile != "" && *keyFile != "" {
		log.Fatal(http.ListenAndServeTLS(*addr, *certFile, *keyFile, nil))
	}
	log.Fatal(http.ListenAndServe(*addr, nil))
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
