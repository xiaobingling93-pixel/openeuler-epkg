#!/bin/bash
# Compare epkg rpmlua with system rpm rpmlua for basic posix functions
# This script tests compatibility between implementations

set -e

# Source common variables and setup
source "$(dirname "$0")/common.sh"

RPM_RPMLUA="${RPM_RPMLUA:-rpmlua}"

# Check if system rpm rpmlua is available
if ! command -v rpmlua &> /dev/null; then
    echo "Warning: system rpmlua not found, skipping comparison tests"
    exit 0
fi

TEST_DIR="/tmp/epkg_posix_compare_test"
mkdir -p "$TEST_DIR"


# Helper function to compare epkg and rpm rpmlua results
# Usage: compare_test "test_name" "lua_code" ["truncate_len"]
# If truncate_len is set, truncate output display to that length
compare_test() {
    local test_name="$1"
    local lua_code="$2"
    local truncate_len="${3:-}"

    echo "=== Testing $test_name ==="
    $EPKG_RPMLUA -e "$lua_code" > /tmp/epkg_out.txt 2>&1
    $RPM_RPMLUA -e "$lua_code" > /tmp/rpm_out.txt 2>&1
    EPKG_RESULT=$(cat /tmp/epkg_out.txt | grep -v "^$" | tail -1)
    RPM_RESULT=$(cat /tmp/rpm_out.txt | grep -v "^$" | tail -1)
    if [ -n "$truncate_len" ]; then
        echo "  epkg: ${EPKG_RESULT:0:$truncate_len}..."
        echo "  rpm:  ${RPM_RESULT:0:$truncate_len}..."
    else
        echo "  epkg: $EPKG_RESULT"
        echo "  rpm:  $RPM_RESULT"
    fi

    # Normalize RPM results for comparison (convert X.0 to X)
    RPM_NORMALIZED="${RPM_RESULT%.0}"

    if [ "$EPKG_RESULT" != "$RPM_NORMALIZED" ]; then
        echo "  WARNING: Results differ"
    else
        echo "  OK: Results match"
    fi
    echo ""
}

compare_test "posix.access()" "local f = '/etc/passwd'; print(posix.access(f))"

compare_test "posix.access() - read mode" "local f = '/etc/passwd'; print(posix.access(f, 'r'))"

compare_test "posix.access() - non-existent file" "print(posix.access('/nonexistent/file/path'))"

compare_test "posix.getprocessid() - uid" "print(posix.getprocessid('uid'))"

compare_test "posix.getprocessid() - gid" "print(posix.getprocessid('gid'))"

compare_test "posix.getprocessid() - euid" "print(posix.getprocessid('euid'))"

compare_test "posix.getprocessid() - egid" "print(posix.getprocessid('egid'))"

compare_test "posix.getprocessid() - ppid" "print(posix.getprocessid('ppid'))"

compare_test "posix.uname() - sysname" "print(posix.uname('sysname'))"

compare_test "posix.uname() - nodename" "print(posix.uname('nodename'))"

compare_test "posix.uname() - release" "print(posix.uname('release'))"

compare_test "posix.uname() - machine" "print(posix.uname('machine'))"

compare_test "posix.getenv() - PATH" "print(posix.getenv('PATH') or 'nil')" "50"

compare_test "posix.getenv() - HOME" "print(posix.getenv('HOME') or 'nil')"

compare_test "posix.getenv() - non-existent" "print(posix.getenv('NONEXISTENT_VAR') or 'nil')"

compare_test "posix.stat() - mode" "print(posix.stat('/etc/passwd', 'mode'))"

compare_test "posix.stat() - type" "print(posix.stat('/etc/passwd', 'type'))"

compare_test "posix.stat() - type (directory)" "print(posix.stat('/etc', 'type'))"

compare_test "posix.sysconf() - arg_max" "print(posix.sysconf('arg_max'))"

compare_test "posix.sysconf() - open_max" "print(posix.sysconf('open_max'))"

compare_test "posix.sysconf() - clk_tck" "print(posix.sysconf('clk_tck'))"

compare_test "posix.sysconf() - ngroups_max" "print(posix.sysconf('ngroups_max'))"

compare_test "posix.sysconf() - child_max" "print(posix.sysconf('child_max'))"

compare_test "posix.stat() - size" "print(posix.stat('/etc/passwd', 'size'))"

compare_test "posix.stat() - uid" "print(posix.stat('/etc/passwd', 'uid'))"

compare_test "posix.stat() - gid" "print(posix.stat('/etc/passwd', 'gid'))"

compare_test "posix.ttyname() - stdin" "print(posix.ttyname(0) or 'nil')"

compare_test "posix.ttyname() - stdout" "print(posix.ttyname(1) or 'nil')"

compare_test "posix.ttyname() - stderr" "print(posix.ttyname(2) or 'nil')"

compare_test "posix.ttyname() - invalid fd" "print(posix.ttyname(999) or 'nil')"

compare_test "posix.getlogin()" "print(posix.getlogin() or 'nil')"

compare_test "posix.getpasswd() - name" "local p = posix.getpasswd(); print(p and p.name or 'nil')"

compare_test "posix.getpasswd() - uid" "local p = posix.getpasswd(); print(p and p.uid or 'nil')"

compare_test "posix.getpasswd() - gid" "local p = posix.getpasswd(); print(p and p.gid or 'nil')"

compare_test "posix.getpasswd() - dir" "local p = posix.getpasswd(); print(p and p.dir or 'nil')"

compare_test "posix.getpasswd() - shell" "local p = posix.getpasswd(); print(p and p.shell or 'nil')"

compare_test "posix.getpasswd() - by name" "local p = posix.getpasswd(); local name = p and p.name or 'root'; print(posix.getpasswd(name) and posix.getpasswd(name).name or 'nil')"

compare_test "posix.getgroup() - by gid" "local p = posix.getpasswd(); local gid = p and p.gid or 0; print(posix.getgroup(gid) and posix.getgroup(gid).gid or 'nil')"

compare_test "posix.getgroup() - by name" "local p = posix.getpasswd(); local g = p and posix.getgroup(p.gid) or nil; local name = g and g.name or 'root'; print(posix.getgroup(name) and posix.getgroup(name).name or 'nil')"

compare_test "posix.umask()" "print(posix.umask())"

compare_test "posix.pathconf() - name_max" "print(posix.pathconf('/tmp', 'name_max'))"

compare_test "posix.pathconf() - path_max" "print(posix.pathconf('/tmp', 'path_max'))"

compare_test "posix.pathconf() - link_max" "print(posix.pathconf('/tmp', 'link_max'))"

compare_test "rpm.vercmp() - equal versions" "print(rpm.vercmp('1.0-1', '1.0-1'))"

compare_test "rpm.vercmp() - basic less than" "print(rpm.vercmp('1.0-1', '1.0-2'))"

compare_test "rpm.vercmp() - basic greater than" "print(rpm.vercmp('1.0-2', '1.0-1'))"

compare_test "rpm.vercmp() - epoch comparison" "print(rpm.vercmp('2:1.0-1', '1:2.0-1'))"

compare_test "rpm.vercmp() - numeric vs alphabetic (RPM: numbers > letters)" "print(rpm.vercmp('2.1.76', '2.1.fb69'))"

compare_test "rpm.vercmp() - reverse alphabetic vs numeric" "print(rpm.vercmp('2.1.fb69', '2.1.76'))"

compare_test "rpm.vercmp() - version with release > version without release" "print(rpm.vercmp('12.0.0-bp160.1.2', '12.0.0'))"

compare_test "rpm.vercmp() - version without release < version with release" "print(rpm.vercmp('12.0.0', '12.0.0-bp160.1.2'))"

compare_test "rpm.vercmp() - semantic versioning example" "print(rpm.vercmp('1.0.9', '0.18.0'))"

compare_test "rpm.vercmp() - complex version comparison" "print(rpm.vercmp('7.3.0', '7.0.99.1'))"

compare_test "rpm.vercmp() - pre-release markers (tilde lowest precedence)" "print(rpm.vercmp('1.0~beta', '1.0'))"

compare_test "rpm.vercmp() - pre-release vs final" "print(rpm.vercmp('1.0', '1.0~beta'))"

# Test rpm.glob() function
TEST_DIR_GLOB="/tmp/epkg_glob_compare_test"
mkdir -p "$TEST_DIR_GLOB"
echo "test1" > "$TEST_DIR_GLOB/file1.txt"
echo "test2" > "$TEST_DIR_GLOB/file2.txt"
echo "test3" > "$TEST_DIR_GLOB/file3.log"

# Note: We use double quotes inside the lua string to allow bash variable expansion
compare_test "rpm.glob() - basic *.txt" "local t = rpm.glob(\"$TEST_DIR_GLOB/*.txt\"); print(type(t) == 'table' and #t or 'nil')"

compare_test "rpm.glob() - count *.txt files" "local t = rpm.glob(\"$TEST_DIR_GLOB/*.txt\"); print(t and #t or 'nil')"

compare_test "rpm.glob() - no matches (should return nil)" "print(rpm.glob(\"$TEST_DIR_GLOB/*.nonexistent\") == nil and 'nil' or 'not nil')"

compare_test "rpm.glob() - NOCHECK with no matches" "local t = rpm.glob(\"$TEST_DIR_GLOB/*.nope\", 'c'); print(type(t) == 'table' and t[1] or 'nil')" 100

compare_test "rpm.glob() - NOCHECK returns pattern" "local t = rpm.glob(\"$TEST_DIR_GLOB/*.xyz\", 'c'); print(t and t[1] or 'nil')" 100

compare_test "rpm.glob() - *.log files" "local t = rpm.glob(\"$TEST_DIR_GLOB/*.log\"); print(type(t) == 'table' and #t or 'nil')"

compare_test "rpm.glob() - all files" "local t = rpm.glob(\"$TEST_DIR_GLOB/*\"); print(type(t) == 'table' and #t or 'nil')"

# Cleanup glob test files
rm -rf "$TEST_DIR_GLOB"

echo ""
echo "=== Comparison tests completed ==="

