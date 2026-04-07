#!/bin/sh
# Minimal Scala project: run script (needs Java).

. "$(dirname "$0")/../common.sh"

run_install scala openjdk-17-jdk default-jdk java-openjdk
check_cmd scala -e 'println(1)' || lang_skip "no scala for OS=$OS"

run_ebin scala -e 'println(1)'

run scala -e "println(\"ok\")" | grep -q ok
lang_ok
