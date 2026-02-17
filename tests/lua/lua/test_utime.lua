-- Test posix.utime() function
-- Tests file time modification

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/utime_test"
os.execute("echo 'test content' > " .. test_file)

-- Test 1: Get original modification time
local original_stat = posix.stat(test_file)
assert(original_stat ~= nil, "stat should return table")
local original_mtime = original_stat.mtime

-- Test 2: Set new modification time
local new_mtime = original_mtime + 100
local result = posix.utime(test_file, new_mtime)
assert(result == 0, "utime should succeed")

-- Test 3: Verify modification time was changed
local new_stat = posix.stat(test_file)
assert(new_stat ~= nil, "stat should return table after utime")
-- Allow some tolerance for filesystem time resolution
assert(math.abs(new_stat.mtime - new_mtime) <= 1, "utime should set modification time")

-- Test 4: Set both mtime and atime
local new_mtime2 = new_mtime + 50
local new_atime = new_mtime2 + 25
local result = posix.utime(test_file, new_mtime2, new_atime)
assert(result == 0, "utime should succeed with both times")

-- Test 5: Verify both times were set
local final_stat = posix.stat(test_file)
assert(math.abs(final_stat.mtime - new_mtime2) <= 1, "utime should set mtime")
assert(math.abs(final_stat.atime - new_atime) <= 1, "utime should set atime")

-- Test 6: Set time on non-existent file (should fail)
local result = posix.utime("/nonexistent/file", 1234567890)
assert(result == nil, "utime on non-existent file should return nil")

-- Cleanup
posix.unlink(test_file)
