#!/bin/bash
# epkg benchmark script with timing breakdown
# Usage: ./scripts/benchmark_perf.sh [command]

BIN=target/x86_64-pc-windows-gnu/release/epkg.exe
ENV=alpine
CMD=${1:-"ls /proc"}
LOG_FILE=${2:-"/tmp/epkg_perf.log"}

echo "=== epkg Performance Benchmark (with timing breakdown) ==="
echo "日期: $(date)"
echo "Binary: $BIN"
echo "命令: epkg run -e $ENV $CMD"
echo "日志文件: $LOG_FILE"
echo ""

# Run with EPKG_DEBUG_LIBKRUN=1 to get timing breakdown
export EPKG_DEBUG_LIBKRUN=1

echo "--- Single run with timing breakdown ---"
$BIN run -e $ENV $CMD 2>&1 | tee $LOG_FILE | grep -E "\[PERF\]|\[epkg" | head -30

echo ""
echo "--- Summary from log ---"
grep -E "VM config|Guest connected|command execution|TOTAL" $LOG_FILE 2>/dev/null