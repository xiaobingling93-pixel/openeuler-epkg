-- Test posix.getprocessid() function
-- Tests process ID retrieval

-- Test 1: Get all process IDs (no selector)
local ids = posix.getprocessid()
assert(ids ~= nil, "getprocessid() should return table")
assert(type(ids) == "table", "getprocessid() should return table type")
assert(ids.pid ~= nil, "pid should exist")
assert(ids.uid ~= nil, "uid should exist")
assert(ids.gid ~= nil, "gid should exist")
assert(ids.euid ~= nil, "euid should exist")
assert(ids.egid ~= nil, "egid should exist")
assert(ids.ppid ~= nil, "ppid should exist")
assert(ids.pgrp ~= nil, "pgrp should exist")

-- Test 2: Get specific process ID - pid
local pid = posix.getprocessid("pid")
assert(type(pid) == "number", "pid selector should return number")
assert(pid > 0, "pid should be positive")
assert(pid == ids.pid, "pid selector should match table value")

-- Test 3: Get specific process ID - uid
local uid = posix.getprocessid("uid")
assert(type(uid) == "number", "uid selector should return number")
assert(uid >= 0, "uid should be non-negative")
assert(uid == ids.uid, "uid selector should match table value")

-- Test 4: Get specific process ID - gid
local gid = posix.getprocessid("gid")
assert(type(gid) == "number", "gid selector should return number")
assert(gid >= 0, "gid should be non-negative")
assert(gid == ids.gid, "gid selector should match table value")

-- Test 5: Get specific process ID - euid
local euid = posix.getprocessid("euid")
assert(type(euid) == "number", "euid selector should return number")
assert(euid >= 0, "euid should be non-negative")
assert(euid == ids.euid, "euid selector should match table value")

-- Test 6: Get specific process ID - egid
local egid = posix.getprocessid("egid")
assert(type(egid) == "number", "egid selector should return number")
assert(egid >= 0, "egid should be non-negative")
assert(egid == ids.egid, "egid selector should match table value")

-- Test 7: Get specific process ID - ppid
local ppid = posix.getprocessid("ppid")
assert(type(ppid) == "number", "ppid selector should return number")
assert(ppid > 0, "ppid should be positive")
assert(ppid == ids.ppid, "ppid selector should match table value")

-- Test 8: Get specific process ID - pgrp
local pgrp = posix.getprocessid("pgrp")
assert(type(pgrp) == "number", "pgrp selector should return number")
assert(pgrp > 0, "pgrp should be positive")
assert(pgrp == ids.pgrp, "pgrp selector should match table value")

-- Test 9: Invalid selector (should error)
local success, err = pcall(function() posix.getprocessid("invalid") end)
assert(not success, "invalid selector should cause error")
