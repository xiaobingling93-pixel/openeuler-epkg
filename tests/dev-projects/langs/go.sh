#!/bin/sh
# Minimal Go project: build and run a tiny program.

. "$(dirname "$0")/../common.sh"

# Install one of go/golang/gcc-go (Alpine has conflict between go and gcc-go if both requested)
run_install go ca-certificates
check_cmd go version || { run_install golang ca-certificates; check_cmd go version || { run_install gcc-go ca-certificates; check_cmd go version || lang_skip "no go package for OS=$OS"; }; }

run_ebin_if go version

# Use GOCACHE inside env so go build/run can write (avoid permission denied on host .cache)
# Create test file - use go for conda/Windows (no /bin/sh)
if [ "$OS" = "conda" ]; then
    run go run -e -exec "" /dev/stdin <<'EOF'
package main
import (
    "fmt"
    "os"
)
func main() {
    os.MkdirAll("/tmp/goproj", 0755)
    f, _ := os.Create("/tmp/goproj/main.go")
    f.WriteString("package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"ok\") }\n")
    f.Close()
    fmt.Println("created")
}
EOF
    run go build -o /tmp/goproj/hello /tmp/goproj/main.go
    run /tmp/goproj/hello | grep -q ok
    lang_ok
    exit 0
fi

run /bin/sh -c 'mkdir -p /tmp/goproj && cd /tmp/goproj && printf "%s\n" "package main" "import \"fmt\"" "func main() { fmt.Println(\"ok\") }" > main.go'
run /bin/sh -c 'export GOCACHE=/tmp/go-build && cd /tmp/goproj && go build -o hello main.go && ./hello'
run /bin/sh -c 'cd /tmp && rm -rf gogetproj && mkdir -p gogetproj && cd gogetproj && go mod init test && export GOCACHE=/tmp/go-build && go get rsc.io/quote'
run /bin/sh -c 'export GOCACHE=/tmp/go-build && cd /tmp/gogetproj && printf "%s\n" "package main" "import (" "\"fmt\"" "\"rsc.io/quote\"" ")" "func main() { fmt.Println(quote.Hello()) }" > main.go && go run main.go'
lang_ok
