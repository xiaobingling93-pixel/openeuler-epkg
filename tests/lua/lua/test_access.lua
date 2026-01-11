-- Test posix.access() function
-- Tests file access permission checking

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Create test files with different permissions
local test_file = test_dir .. "/test_file"
local test_dir_path = test_dir .. "/test_dir"
os.execute("touch " .. test_file)
os.execute("chmod 644 " .. test_file)
os.execute("mkdir -p " .. test_dir_path)
os.execute("chmod 755 " .. test_dir_path)

-- Test 1: Check file existence (default mode)
local result = posix.access(test_file)
assert(result == 0, "test_file should exist")

-- Test 2: Check non-existent file
local result = posix.access("/nonexistent/file/path")
assert(result == nil, "non-existent file should return nil")

-- Test 3: Check read permission
local result = posix.access(test_file, "r")
assert(result == 0, "test_file should be readable")

-- Test 4: Check write permission
local result = posix.access(test_file, "w")
assert(result == 0, "test_file should be writable")

-- Test 5: Check execute permission (should fail for regular file)
local result = posix.access(test_file, "x")
assert(result == nil, "regular file should not be executable")

-- Test 6: Check directory execute permission
local result = posix.access(test_dir_path, "x")
assert(result == 0, "directory should be executable (traversable)")

-- Test 7: Check multiple permissions
local result = posix.access(test_file, "rw")
assert(result == 0, "test_file should be readable and writable")

-- Test 8: Check file existence explicitly
local result = posix.access(test_file, "f")
assert(result == 0, "test_file should exist with 'f' mode")

-- Cleanup: Remove test files and directories using posix functions
posix.unlink(test_file)
posix.rmdir(test_dir_path)
