-- Test posix.sysconf() function
-- Tests system configuration values

-- Test 1: Get all sysconf values (no selector)
local sysconf = posix.sysconf()
assert(sysconf ~= nil, "sysconf() should return table")
assert(type(sysconf) == "table", "sysconf() should return table type")
assert(sysconf.arg_max ~= nil, "arg_max should exist")
assert(sysconf.child_max ~= nil, "child_max should exist")
assert(sysconf.clk_tck ~= nil, "clk_tck should exist")
assert(sysconf.ngroups_max ~= nil, "ngroups_max should exist")
assert(sysconf.stream_max ~= nil, "stream_max should exist")
assert(sysconf.tzname_max ~= nil, "tzname_max should exist")
assert(sysconf.open_max ~= nil, "open_max should exist")

-- Test 2: Get specific sysconf value - arg_max
local arg_max = posix.sysconf("arg_max")
assert(type(arg_max) == "number", "arg_max selector should return number")
assert(arg_max > 0, "arg_max should be positive")
assert(arg_max == sysconf.arg_max, "arg_max selector should match table value")

-- Test 3: Get specific sysconf value - open_max
local open_max = posix.sysconf("open_max")
assert(type(open_max) == "number", "open_max selector should return number")
assert(open_max > 0, "open_max should be positive")
assert(open_max == sysconf.open_max, "open_max selector should match table value")

-- Test 4: Get specific sysconf value - clk_tck
local clk_tck = posix.sysconf("clk_tck")
assert(type(clk_tck) == "number", "clk_tck selector should return number")
assert(clk_tck > 0, "clk_tck should be positive")
assert(clk_tck == sysconf.clk_tck, "clk_tck selector should match table value")
