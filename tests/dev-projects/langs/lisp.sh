#!/bin/sh
# Minimal Lisp (SBCL/CLISP) project: run script.

. "$(dirname "$0")/../common.sh"

run_install sbcl clisp
check_cmd sbcl --version || check_cmd clisp --version || lang_skip "no sbcl/clisp for OS=$OS"

run_ebin_if sbcl --version
run_ebin_if clisp --version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

if run sbcl --version; then
    run $SHELL_CMD 'echo "(write-line \"ok\")" | sbcl --noinform --quit' | grep -q ok
else
    run $SHELL_CMD 'echo "(write-line \"ok\")" | clisp -q' | grep -q ok
fi
lang_ok
