#!/bin/sh
# Minimal Lisp (SBCL/CLISP) project: run script.

. "$(dirname "$0")/../common.sh"

run_install sbcl clisp
check_cmd sbcl --version || check_cmd clisp --version || lang_skip "no sbcl/clisp for OS=$OS"

run_ebin_if sbcl --version
run_ebin_if clisp --version

if run sbcl --version; then
    run /bin/sh -c 'echo "(write-line \"ok\")" | sbcl --noinform --quit' | grep -q ok
else
    run /bin/sh -c 'echo "(write-line \"ok\")" | clisp -q' | grep -q ok
fi
lang_ok
