#!/bin/bash
# virtiofs performance analysis script
# Analyzes timing breakdown and virtiofs call statistics

BIN="/mnt/c/Users/aa/.epkg/envs/self/usr/bin/epkg.exe"
ENV="alpine"

echo "=== virtiofs Performance Analysis ==="
echo "日期: $(date)"
echo ""

# Test different workloads
echo "=== Test 1: Simple command (echo hello) ==="
powershell.exe -Command "
\$env:EPKG_DEBUG_LIBKRUN = '1'
\$output = & 'C:\Users\aa\.epkg\envs\self\usr\bin\epkg.exe' run -e alpine echo hello 2>&1
\$output | Select-String -Pattern '\[PERF\]'
"

echo ""
echo "=== Test 2: List small directory (ls /) ==="
powershell.exe -Command "
\$env:EPKG_DEBUG_LIBKRUN = '1'
\$output = & 'C:\Users\aa\.epkg\envs\self\usr\bin\epkg.exe' run -e alpine ls / 2>&1
\$output | Select-String -Pattern '\[PERF\]'
"

echo ""
echo "=== Test 3: List large directory (ls /usr/bin) ==="
powershell.exe -Command "
\$env:EPKG_DEBUG_LIBKRUN = '1'
\$output = & 'C:\Users\aa\.epkg\envs\self\usr\bin\epkg.exe' run -e alpine ls /usr/bin 2>&1
\$output | Select-String -Pattern '\[PERF\]'
"

echo ""
echo "=== Test 4: List with attributes (ls -l /usr/bin) ==="
powershell.exe -Command "
\$env:EPKG_DEBUG_LIBKRUN = '1'
\$output = & 'C:\Users\aa\.epkg\envs\self\usr\bin\epkg.exe' run -e alpine ls -l /usr/bin 2>&1
\$output | Select-String -Pattern '\[PERF\]'
"

echo ""
echo "=== Summary ==="
echo "Phase breakdown (approximate):"
echo "- Guest connect: VM boot + kernel init + vsock connect"
echo "- Command exec: send command + execute + return result"
echo ""
echo "Virtiofs overhead:"
echo "- ls / vs echo: small directory traversal"
echo "- ls /usr/bin vs ls /: large directory traversal"
echo "- ls -l vs ls: getattr (lstat) overhead"