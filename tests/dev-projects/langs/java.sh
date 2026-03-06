#!/bin/sh
# Minimal Java project: compile and run.

. "$(dirname "$0")/../common.sh"

run_install openjdk-17-jdk default-jdk java-openjdk openjdk17-jre openjdk17 openjdk-17 openjdk
check_cmd javac -version || lang_skip "no java for OS=$OS"

run_ebin javac -version
run_ebin_if java -version

run /bin/sh -c 'mkdir -p /tmp/javaproj && cd /tmp/javaproj && printf "%s\n" "public class Main { public static void main(String[] args) { System.out.println(\"ok\"); } }" > Main.java'
run /bin/sh -c 'cd /tmp/javaproj && javac Main.java && java Main' | grep -q ok
lang_ok
