#!/bin/sh
# Minimal Rust project: cargo build, run (if rust/cargo available).

. "$(dirname "$0")/../common.sh"

run_install rust cargo rustc
check_cmd cargo --version || lang_skip "no rust package for OS=$OS"

run_ebin cargo --version

run /bin/sh -c 'mkdir -p /tmp/rustproj/src && cd /tmp/rustproj && echo "[package]\nname=\"rustproj\"\nversion=\"0.1.0\"\n[profile.release]\nopt-level=0" > Cargo.toml && echo "fn main() { println!(\"ok\"); }" > src/main.rs'
run /bin/sh -c 'cd /tmp/rustproj && cargo build && cargo run' | grep -q ok
run /bin/sh -c 'cd /tmp/rustproj && cargo add rand && echo "fn main() { println!(\"{}\", rand::random::<u32>()); }" > src/main.rs && cargo run' | grep -q .
# Exercise ebin for cargo (build in rustproj)
if [ -n "${ENV_ROOT:-}" ] && [ -x "$ENV_ROOT/usr/ebin/cargo" ]; then
    run /bin/sh -c 'cd /tmp/rustproj && '"$ENV_ROOT"'/usr/ebin/cargo build'
fi
lang_ok
