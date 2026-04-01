#!/bin/bash
# epkg performance analysis script
# Analyzes timing breakdown from EPKG_DEBUG_LIBKRUN=1 output

BIN="/mnt/c/Users/aa/.epkg/envs/self/usr/bin/epkg.exe"
ENV="alpine"
CMD=${1:-"ls /proc"}
RUNS=${2:-5}

echo "=== epkg Performance Analysis ==="
echo "日期: $(date)"
echo "命令: epkg run -e $ENV $CMD"
echo "Runs: $RUNS"
echo ""

# Results array
declare -a vm_config_times
declare -a guest_connected_times
declare -a cmd_exec_times
declare -a total_times
declare -a wall_times

for i in $(seq 1 $RUNS); do
    echo "--- Run $i ---"

    start=$(date +%s.%N)
    output=$(EPKG_DEBUG_LIBKRUN=1 $BIN run -e $ENV $CMD 2>&1)
    end=$(date +%s.%N)
    wall=$(echo "$end - $start" | bc)

    # Parse timing from output
    vm_config=$(echo "$output" | grep "VM config took" | sed 's/.*took \([0-9.]*\)ms.*/\1/')
    guest=$(echo "$output" | grep "Guest connected after" | sed 's/.*after \([0-9.]*\)ms.*/\1/')
    cmd_exec=$(echo "$output" | grep "command execution took" | sed 's/.*took \([0-9.]*\)ms.*/\1/')
    total=$(echo "$output" | grep "TOTAL time" | sed 's/.*time \([0-9.]*\)ms.*/\1/')

    echo "  VM config:      ${vm_config}ms"
    echo "  Guest connect:  ${guest}ms"
    echo "  Command exec:   ${cmd_exec}ms"
    echo "  TOTAL (measured): ${total}ms"
    echo "  Wall time:      ${wall}s"

    vm_config_times+=($vm_config)
    guest_connected_times+=($guest)
    cmd_exec_times+=($cmd_exec)
    total_times+=($total)
    wall_times+=($wall)
    echo ""
done

# Calculate averages (skip first run for warm-up)
echo "=== Summary (avg of runs 2-$RUNS) ==="
avg_guest=$(echo "${guest_connected_times[@]:1}" | tr ' ' '\n' | awk '{sum+=$1} END {printf "%.0f", sum/NR}')
avg_cmd=$(echo "${cmd_exec_times[@]:1}" | tr ' ' '\n' | awk '{sum+=$1} END {printf "%.0f", sum/NR}')
avg_total=$(echo "${total_times[@]:1}" | tr ' ' '\n' | awk '{sum+=$1} END {printf "%.0f", sum/NR}')
avg_wall=$(echo "${wall_times[@]:1}" | tr ' ' '\n' | awk '{sum+=$1} END {printf "%.2f", sum/NR}')

echo "| Phase | Time (ms) | % of Total |"
echo "|-------|-----------|------------|"
echo "| VM config | ${avg_guest}ms | $(( avg_guest * 100 / avg_total ))% |"
echo "| Guest connect | ${avg_guest}ms | $(( avg_guest * 100 / avg_total ))% |"
echo "| Command exec | ${avg_cmd}ms | $(( avg_cmd * 100 / avg_total ))% |"
echo "| **Total** | **${avg_total}ms** | 100% |"
echo ""
echo "Wall time average: ${avg_wall}s"