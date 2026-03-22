#!/bin/sh
# Minimal C++ project: g++ build and run.

. "$(dirname "$0")/../common.sh"

# Note: on Arch Linux, g++ is part of gcc package, not a separate package
run_install build-base g++ gcc-c++ build-essential gcc
check_cmd g++ --version || lang_skip "no g++ for OS=$OS"

run_ebin g++ --version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/cppproj && cd /tmp/cppproj && printf "%s\n" "#include <iostream>" "int main() { std::cout << \"ok\" << std::endl; return 0; }" > main.cc'
run $SHELL_CMD 'cd /tmp/cppproj && g++ -o hello main.cc && ./hello' | grep -q ok
lang_ok
