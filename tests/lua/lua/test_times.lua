-- Test posix.times() function
-- Tests process time information

-- Test 1: Get all times values (no selector)
local times = posix.times()
assert(times ~= nil, "times() should return table")
assert(type(times) == "table", "times() should return table type")
assert(times.utime ~= nil, "utime should exist")
assert(times.stime ~= nil, "stime should exist")
assert(times.cutime ~= nil, "cutime should exist")
assert(times.cstime ~= nil, "cstime should exist")
assert(times.elapsed ~= nil, "elapsed should exist")

-- All times should be numbers (floats)
assert(type(times.utime) == "number", "utime should be number")
assert(type(times.stime) == "number", "stime should be number")
assert(type(times.cutime) == "number", "cutime should be number")
assert(type(times.cstime) == "number", "cstime should be number")
assert(type(times.elapsed) == "number", "elapsed should be number")

-- Times should be non-negative
assert(times.utime >= 0, "utime should be non-negative")
assert(times.stime >= 0, "stime should be non-negative")
assert(times.cutime >= 0, "cutime should be non-negative")
assert(times.cstime >= 0, "cstime should be non-negative")
assert(times.elapsed >= 0, "elapsed should be non-negative")

-- Test 2: Get specific times value - utime
local utime = posix.times("utime")
assert(type(utime) == "number", "utime selector should return number")
assert(utime == times.utime, "utime selector should match table value")

-- Test 3: Get specific times value - elapsed
local elapsed = posix.times("elapsed")
assert(type(elapsed) == "number", "elapsed selector should return number")
assert(elapsed == times.elapsed, "elapsed selector should match table value")

-- Test 4: Execute Python command and compare elapsed times
local current_times = posix.times()

if type(io.popen) == "function" then
  -- Execute Python command to get os.times().elapsed
  local success, python_handle_or_err = pcall(io.popen, "python3 -c 'import os; print(os.times().elapsed / 100 / os.sysconf(\"SC_CLK_TCK\"))'")
  if success then
    local python_handle = python_handle_or_err
    local python_output = python_handle:read("*a")
    python_handle:close()
    local python_elapsed = tonumber(python_output:match("([%d%.]+)"))

    if python_elapsed then
      -- Assert elapsed times match within 1 second tolerance
      local elapsed_diff = math.abs(current_times.elapsed - python_elapsed)
      assert(elapsed_diff <= 1.0, string.format("Elapsed times differ by %.2f seconds", elapsed_diff))
    else
      -- Failed to parse Python elapsed time, skip this check
    end
  else
    -- io.popen failed (likely not supported), skip Python elapsed time comparison
  end
else
  -- io.popen not supported, skip Python elapsed time comparison
end

-- Compare posix.times() CPU time with os.clock()
-- os.clock() measures Lua CPU time, posix.times() measures process CPU time
local cpu_total = current_times.utime + current_times.stime
-- Allow os.clock() to be >= posix CPU time (Lua CPU time includes process CPU time)
local os_clock_now = os.clock()
assert(os_clock_now >= cpu_total, string.format("os.clock() (%.3f) should be >= posix.times CPU total (%.3f)",
        os_clock_now, cpu_total))
