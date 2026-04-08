#!/bin/sh
# Minimal Scala project: run script (needs Java).

. "$(dirname "$0")/../common.sh"

# openEuler has xorg-x11-fonts packaging conflict, need --ignore-file-conflicts
if [ "$OS" = "openeuler" ]; then
    $EPKG_BIN -e "$ENV_NAME" --assume-yes install --ignore-missing --ignore-file-conflicts scala || true
fi
run_install scala openjdk-17-jdk default-jdk java-openjdk

# openEuler's scala 2.10.6 (2013) doesn't support -e flag, use pipe instead
if [ "$OS" = "openeuler" ]; then
    # Test with pipe for old scala versions
    check_cmd sh -c 'echo "println(1)" | timeout 5 scala' || lang_skip "no scala for OS=$OS"
else
    check_cmd scala -e 'println(1)' || lang_skip "no scala for OS=$OS"
fi

if [ "$OS" = "openeuler" ]; then
    run /bin/sh -c 'echo "println(1)" | timeout 5 scala'
    run /bin/sh -c 'echo "println(\"ok\")" | timeout 5 scala' | grep -q ok
else
    run_ebin scala -e 'println(1)'
    run scala -e "println(\"ok\")" | grep -q ok
fi
lang_ok
