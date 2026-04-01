#!/bin/bash
# epkg benchmark script
# Usage: ./scripts/benchmark.sh [command]

BIN=target/x86_64-pc-windows-gnu/release/epkg.exe
ENV=alpine
CMD=${1:-"ls /proc"}
RUNS=${2:-5}

echo "=== epkg Performance Benchmark ==="
echo "日期: $(date)"
echo "Binary: $BIN"
echo "命令: epkg run -e $ENV $CMD"
echo "Runs: $RUNS"
echo ""

echo "| Run | Time (s) |"
echo "|-----|----------|"

total=0
for i in $(seq 1 $RUNS); do
    start=$(date +%s.%N)
    $BIN run -e $ENV $CMD > /dev/null 2>&1
    end=$(date +%s.%N)
    t=$(echo "$end - $start" | bc)
    printf "| %d | %.2f |\n" $i $t
    total=$(echo "$total + $t" | bc)
done

avg=$(echo "scale=2; $total / $RUNS" | bc)
echo ""
echo "**Average: ${avg}s**"