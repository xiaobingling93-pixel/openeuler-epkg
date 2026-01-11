-- Test posix.stat() function
-- Tests file status information retrieval

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/stat_test"
local test_dir_path = test_dir .. "/stat_dir"
os.execute("echo 'test content' > " .. test_file)
os.execute("mkdir -p " .. test_dir_path)

-- Test 1: Get full stat table
local stat = posix.stat(test_file)
assert(stat ~= nil, "stat should return table")
assert(type(stat.mode) == "string", "mode should be string")
assert(type(stat.ino) == "number", "ino should be number")
assert(type(stat.dev) == "number", "dev should be number")
assert(type(stat.nlink) == "number", "nlink should be number")
assert(type(stat.uid) == "number", "uid should be number")
assert(type(stat.gid) == "number", "gid should be number")
assert(type(stat.size) == "number", "size should be number")
assert(type(stat.atime) == "number", "atime should be number")
assert(type(stat.mtime) == "number", "mtime should be number")
assert(type(stat.ctime) == "number", "ctime should be number")
assert(type(stat.type) == "string", "type should be string")
assert(stat.type == "regular", "test_file should be regular file")
assert(stat.size > 0, "test_file should have content")

-- Test 2: Get specific stat field - mode
local mode = posix.stat(test_file, "mode")
assert(type(mode) == "string", "mode selector should return string")
assert(#mode == 9, "mode string should be 9 characters")

-- Test 3: Get specific stat field - size
local size = posix.stat(test_file, "size")
assert(type(size) == "number", "size selector should return number")
assert(size > 0, "size should be positive")

-- Test 4: Get specific stat field - type
local file_type = posix.stat(test_file, "type")
assert(file_type == "regular", "file type should be 'regular'")

local dir_type = posix.stat(test_dir_path, "type")
assert(dir_type == "directory", "directory type should be 'directory'")

-- Test 5: Get specific stat field - ino
local ino = posix.stat(test_file, "ino")
assert(type(ino) == "number", "ino selector should return number")
assert(ino > 0, "ino should be positive")

-- Test 6: Get specific stat field - uid
local uid = posix.stat(test_file, "uid")
assert(type(uid) == "number", "uid selector should return number")

-- Test 7: Get specific stat field - gid
local gid = posix.stat(test_file, "gid")
assert(type(gid) == "number", "gid selector should return number")

-- Test 8: Get specific stat field - mtime
local mtime = posix.stat(test_file, "mtime")
assert(type(mtime) == "number", "mtime selector should return number")
assert(mtime > 0, "mtime should be positive")

-- Test 9: Get specific stat field - _mode (numeric mode)
local _mode = posix.stat(test_file, "_mode")
assert(type(_mode) == "number", "_mode selector should return number")

-- Test 10: Test with non-existent file (should return nil)
local stat = posix.stat("/nonexistent/file")
assert(stat == nil, "stat on non-existent file should return nil")

-- Test 11: Test with symlink (should follow symlink)
local symlink_path = test_dir .. "/symlink_test"
os.execute("ln -s " .. test_file .. " " .. symlink_path)
local stat = posix.stat(symlink_path)
assert(stat ~= nil, "stat on symlink should work")
assert(stat.type == "link", "symlink type should be 'link'")

-- Cleanup: Remove test files, symlink, and directories using posix functions
posix.unlink(symlink_path)
posix.unlink(test_file)
posix.rmdir(test_dir_path)
