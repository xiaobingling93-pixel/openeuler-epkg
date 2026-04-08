#!/bin/sh
# Minimal Java project: compile and run.

. "$(dirname "$0")/../common.sh"

# Note: Arch Linux uses jdk-openjdk, Debian/Ubuntu uses openjdk-17-jdk, Alpine uses openjdk17, openEuler uses java-11-openjdk-devel
# openEuler has xorg-x11-fonts packaging conflict, need --ignore-file-conflicts
if [ "$OS" = "openeuler" ]; then
    $EPKG_BIN -e "$ENV_NAME" --assume-yes install --ignore-missing --ignore-file-conflicts java-1.8.0-openjdk-devel || true
fi
run_install java-1.8.0-openjdk-devel openjdk-17-jdk default-jdk java-openjdk openjdk17-jre openjdk17 openjdk-17 openjdk jdk-openjdk jdk17-openjdk java-11-openjdk-devel java-25-openjdk-devel
check_cmd javac -version || lang_skip "no java for OS=$OS"

run_ebin javac -version
run_ebin_if java -version

# Create test file - use java for conda/msys2 (no /bin/sh)
if [ "$OS" = "conda" ] || [ "$OS" = "msys2" ]; then
    run java -e '
        import java.io.*;
        public class init {
            public static void main(String[] args) throws Exception {
                new File("/tmp/javaproj").mkdirs();
                try (PrintWriter w = new PrintWriter("/tmp/javaproj/Main.java")) {
                    w.println("public class Main { public static void main(String[] args) { System.out.println(\"ok\"); } }");
                }
            }
        }
    ' 2>/dev/null || run java -e 'new java.io.File("/tmp/javaproj").mkdirs(); try (var w = new java.io.PrintWriter("/tmp/javaproj/Main.java")) { w.println("public class Main { public static void main(String[] args) { System.out.println(\"ok\"); } }"); }'
    run javac /tmp/javaproj/Main.java
    run java -cp /tmp/javaproj Main | grep -q ok
    lang_ok
    exit 0
fi

run /bin/sh -c 'mkdir -p /tmp/javaproj && cd /tmp/javaproj && printf "%s\n" "public class Main { public static void main(String[] args) { System.out.println(\"ok\"); } }" > Main.java'
run /bin/sh -c 'cd /tmp/javaproj && javac Main.java && java Main' | grep -q ok
lang_ok
