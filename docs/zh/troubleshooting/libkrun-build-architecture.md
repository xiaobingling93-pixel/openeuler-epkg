# libkrun Build Architecture

## IMPORTANT: Do NOT compile libkrun separately!

**libkrun is NOT meant to be compiled standalone!** It is compiled through epkg's build system and statically linked.

### Correct Build Process

```bash
# From epkg root directory - builds libkrun as dependency
make
# or
cargo build --target x86_64-pc-windows-gnu --features libkrun
```

### Common Mistake (AVOID THIS)

```bash
# WRONG: Do NOT cd into git/libkrun and run cargo build
cd git/libkrun/src/vmm && cargo build  # <-- NEVER DO THIS
```

### Why?

1. **Static linking**: libkrun is built as a static library and linked into epkg
2. **Feature flags**: epkg enables specific features (e.g., `libkrun`) that configure libkrun
3. **Cross-compilation**: epkg manages the cross-compilation target (x86_64-pc-windows-gnu)
4. **Dependency resolution**: epkg's Cargo.toml controls all dependency versions

### When modifying libkrun code:

1. Edit files in `git/libkrun/`
2. Build from epkg root: `make` or `cargo build --target x86_64-pc-windows-gnu --features libkrun`
3. The changes will be picked up and compiled into the static library

### Related Rules

- See `.cursor/rules/no-build-release.mdc`: Use `make` not `cargo build --release`

## CRITICAL: Init Path is /usr/bin/init (CARVED IN STONE)

**NEVER change the init path from `/usr/bin/init` to `/bin/init` or any other path.**

The alpine environment has the epkg guest init binary at:
```
/mnt/c/Users/aa/.epkg/envs/alpine/usr/bin/init  (190MB)
```

This path is **CARVED IN STONE** and must remain `/usr/bin/init` forever. The init binary is the epkg guest that handles vsock communication with the host.

### Where this is set:

1. **Kernel cmdline**: `init=/usr/bin/init` in `src/libkrun.rs` (base_cmdline format string)
2. **krun_set_exec**: `/usr/bin/init` passed to libkrun in `src/libkrun.rs`

### Verification:
```bash
# Verify init exists at the correct path
ll /mnt/c/Users/aa/.epkg/envs/alpine/usr/bin/init
# Should show: -rwxrwxrwx 4 root root 190M ... /usr/bin/init
```

**DO NOT CHANGE THIS PATH. EVER.**
