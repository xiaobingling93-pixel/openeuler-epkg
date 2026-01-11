-- Test posix.pathconf() function
-- Tests path configuration values

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Get all pathconf values (no selector)
local pathconf = posix.pathconf(test_dir)
assert(pathconf ~= nil, "pathconf() should return table")
assert(type(pathconf) == "table", "pathconf() should return table type")
assert(pathconf.link_max ~= nil, "link_max should exist")
assert(pathconf.name_max ~= nil, "name_max should exist")
assert(pathconf.path_max ~= nil, "path_max should exist")
assert(pathconf.pipe_buf ~= nil, "pipe_buf should exist")

-- Test 2: Get specific pathconf value - name_max
local name_max = posix.pathconf(test_dir, "name_max")
assert(type(name_max) == "number", "name_max selector should return number")
-- Note: pathconf values can be -1 if not supported, or positive values
assert(name_max == pathconf.name_max, "name_max selector should match table value")

-- Test 3: Get specific pathconf value - path_max
local path_max = posix.pathconf(test_dir, "path_max")
assert(type(path_max) == "number", "path_max selector should return number")
assert(path_max == pathconf.path_max, "path_max selector should match table value")

-- Test 4: Get specific pathconf value - link_max
local link_max = posix.pathconf(test_dir, "link_max")
assert(type(link_max) == "number", "link_max selector should return number")
assert(link_max == pathconf.link_max, "link_max selector should match table value")

-- Cleanup: test_dir is shared, no cleanup needed
