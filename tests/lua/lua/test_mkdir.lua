-- Test posix.mkdir() function
-- Tests directory creation

local test_dir = "/tmp/epkg_posix_test"

-- Test 1: Create new directory
local new_dir = test_dir .. "/mkdir_test"
-- Remove directory if it exists from previous test
if posix.stat(new_dir) ~= nil then
    posix.rmdir(new_dir)
end
local result = posix.mkdir(new_dir)
assert(result == 0, "mkdir should succeed for new directory")

-- Verify directory was created
local stat = posix.stat(new_dir)
assert(stat ~= nil, "created directory should exist")
assert(stat.type == "directory", "created path should be a directory")

-- Test 2: Create nested directories (should fail if parent doesn't exist)
local nested_dir = test_dir .. "/nested/deep/path"
-- Clean up if it exists from previous test
if posix.stat(test_dir .. "/nested") ~= nil then
    posix.rmdir(test_dir .. "/nested/deep/path")
    posix.rmdir(test_dir .. "/nested/deep")
    posix.rmdir(test_dir .. "/nested")
end
local result = posix.mkdir(nested_dir)
-- mkdir doesn't create parent directories, so this should fail
assert(result == nil, "mkdir on nested path without parent should return nil")

-- Test 3: Try to create existing directory (should fail)
local result = posix.mkdir(new_dir)
assert(result == nil, "mkdir on existing directory should return nil")

-- Cleanup: Remove test directories using posix functions
posix.rmdir(new_dir)
