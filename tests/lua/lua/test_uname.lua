-- Test posix.uname() function
-- Tests system information retrieval

-- Test 1: Get all uname fields (no format)
local uname = posix.uname()
assert(uname ~= nil, "uname() should return value")
assert(type(uname) == "string", "uname() should return string type")
assert(#uname > 0, "uname() should return non-empty string")

-- Parse the uname string (format: sysname nodename release version machine)
-- Note: version may contain spaces, so we parse from the beginning
local parts = {}
for word in uname:gmatch("%S+") do
    table.insert(parts, word)
end
assert(#parts >= 5, "uname string should contain at least 5 space-separated parts")

local sysname = parts[1]
local nodename = parts[2]
local release = parts[3]
-- Version and machine are the last two parts (version may contain spaces)
local machine = parts[#parts]

assert(sysname ~= nil and #sysname > 0, "sysname should exist and be non-empty")
assert(nodename ~= nil and #nodename > 0, "nodename should exist and be non-empty")
assert(release ~= nil and #release > 0, "release should exist and be non-empty")
assert(machine ~= nil and #machine > 0, "machine should exist and be non-empty")

-- Test 2: Get specific uname fields using format string
local sysname_sel = posix.uname("%s")
assert(type(sysname_sel) == "string", "uname with %s format should return string")
assert(sysname_sel == sysname, "uname %s should match parsed sysname")

local nodename_sel = posix.uname("%n")
assert(type(nodename_sel) == "string", "uname with %n format should return string")
assert(nodename_sel == nodename, "uname %n should match parsed nodename")

local release_sel = posix.uname("%r")
assert(type(release_sel) == "string", "uname with %r format should return string")
assert(release_sel == release, "uname %r should match parsed release")

local machine_sel = posix.uname("%m")
assert(type(machine_sel) == "string", "uname with %m format should return string")
assert(machine_sel == machine, "uname %m should match parsed machine")

-- Test 3: Test custom format string
local custom_format = posix.uname("%s-%r")
assert(type(custom_format) == "string", "uname with custom format should return string")
assert(custom_format == sysname .. "-" .. release, "custom format should match")

-- Test 4: Test default format (should match default output)
local default_format = posix.uname()
assert(type(default_format) == "string", "uname() should return string")
assert(#default_format > 0, "uname() should return non-empty string")
