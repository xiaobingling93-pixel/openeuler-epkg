#!/bin/sh
# Minimal Rust project: cargo build, run (if rust/cargo available).

. "$(dirname "$0")/../common.sh"

# brew: need bash for shell commands (vdso_time SIGSEGV issue resolved by absolute path fix)
# coreutils for ls/mkdir/etc (host paths filtered from PATH in brew namespace)
# gcc for linker (cargo needs cc linker)
# Install all packages in one command to avoid symlink conflicts (re-install fails if symlinks exist)
if [ "$OS" = "brew" ]; then
    run_install rust cargo rustc gcc bash coreutils
else
    run_install rust cargo rustc
fi

check_cmd cargo --version || lang_skip "no rust package for OS=$OS"

run_ebin cargo --version

# msys2/conda on Windows have bash but no /bin/sh
# brew: host /bin/sh fails in namespace (vdso_time SIGSEGV), use brew's bash
if [ "$OS" = "msys2" ] || [ "$OS" = "conda" ]; then
    SHELL_CMD="bash -c"
elif [ "$OS" = "brew" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/rustproj/src && cd /tmp/rustproj && printf "%s\n" "[package]" "name=\"rustproj\"" "version=\"0.1.0\"" "[profile.release]" "opt-level=0" > Cargo.toml && echo "fn main() { println!(\"ok\"); }" > src/main.rs'
run $SHELL_CMD 'cd /tmp/rustproj && cargo build && cargo run' | grep -q ok
run $SHELL_CMD 'cd /tmp/rustproj && cargo add rand && echo "fn main() { println!(\"{}\", rand::random::<u32>()); }" > src/main.rs && cargo run' | grep -q .
# Exercise ebin for cargo (build in rustproj)
if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/ebin/cargo" ]; then
    run $SHELL_CMD 'cd /tmp/rustproj && '"$ENV_ROOT"'/ebin/cargo build'
fi
lang_ok
