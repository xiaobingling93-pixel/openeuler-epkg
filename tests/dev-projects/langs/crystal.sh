#!/bin/sh
# Minimal Crystal project: build and run.

. "$(dirname "$0")/../common.sh"

run_install crystal gcc
check_cmd crystal --version || lang_skip "no crystal for OS=$OS"

run_ebin crystal --version

# msys2/conda on Windows have bash but no /bin/sh
# brew: use brew's bash to ensure consistent glibc environment
if [ "$OS" = "msys2" ] || [ "$OS" = "conda" ]; then
    SHELL_CMD="bash -c"
elif [ "$OS" = "brew" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/crystalproj && cd /tmp/crystalproj && echo "puts \"ok\"" > main.cr'
# Use --static for fully static binary that doesn't need dynamic linker.
# Crystal-compiled binaries use interpreter /lib64/ld-linux-x86-64.so.2.
# In brew namespace, only HOMEBREW_PREFIX is mounted; /lib64/ resolves to
# HOST's ld.so, but the binary links against BREW's libc. Mixing HOST ld.so
# with BREW libc causes SIGSEGV. Static linking avoids this mismatch entirely.
run $SHELL_CMD 'cd /tmp/crystalproj && crystal build main.cr --static && ./main'
lang_ok
