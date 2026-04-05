#!/bin/bash
# Test script for interactive bash in VM mode

EPKG="epkg -e alpine"

echo "=== Test 1: Non-interactive command (should work) ==="
$EPKG run -- bash -c "echo HELLO"
echo ""

echo "=== Test 2: Multiple commands via -c (should work) ==="
$EPKG run -- bash -c "id; whoami; pwd"
echo ""

echo "=== Test 3: Check PTY devices in VM ==="
$EPKG run -- stat /dev/ptmx
echo ""

echo "=== Test 4: Check /dev/pts/ ==="
$EPKG run -- ls -la /dev/pts/
echo ""

echo "=== Test 5: Check stdin in VM (piped) ==="
echo "test" | $EPKG run -- bash -c "cat"
echo ""

echo "=== Test 6: Interactive stdin test (problematic) ==="
echo "id" | $EPKG run bash
echo "Exit code: $?"
echo ""

echo "=== Test 7: Check if bash is available ==="
$EPKG run -- which bash
$EPKG run -- bash --version | head -1
echo ""

echo "All tests completed!"
