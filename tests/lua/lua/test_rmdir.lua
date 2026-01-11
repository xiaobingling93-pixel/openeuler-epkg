-- Test posix.rmdir() function
-- Tests directory removal

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Create and remove directory
local new_dir = test_dir .. "/rmdir_test"
os.execute("mkdir -p " .. new_dir)
local result = posix.rmdir(new_dir)
assert(result == 0, "rmdir should succeed for empty directory")

-- Test 2: Verify directory was removed
local stat = posix.stat(new_dir)
assert(stat == nil, "rmdir should remove directory")

-- Test 3: Remove non-existent directory (should fail)
local result = posix.rmdir("/nonexistent/directory")
assert(result == nil, "rmdir on non-existent directory should return nil")

-- Test 4: Remove directory with contents (should fail)
local dir_with_contents = test_dir .. "/rmdir_with_contents"
os.execute("mkdir -p " .. dir_with_contents)
os.execute("touch " .. dir_with_contents .. "/file")
local result = posix.rmdir(dir_with_contents)
assert(result == nil, "rmdir on directory with contents should return nil")

-- Cleanup
os.execute("rm -rf " .. dir_with_contents)
