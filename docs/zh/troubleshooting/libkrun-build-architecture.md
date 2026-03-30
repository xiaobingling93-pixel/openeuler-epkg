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
