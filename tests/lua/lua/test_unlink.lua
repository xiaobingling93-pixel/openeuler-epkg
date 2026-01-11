-- Test posix.unlink() function
-- Tests file removal

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Create and remove file
local test_file = test_dir .. "/unlink_test"
os.execute("echo 'test content' > " .. test_file)
local result = posix.unlink(test_file)
assert(result == 0, "unlink should succeed for existing file")

-- Test 2: Verify file was removed
local stat = posix.stat(test_file)
assert(stat == nil, "unlink should remove file")

-- Test 3: Remove non-existent file (should fail)
local result = posix.unlink("/nonexistent/file")
assert(result == nil, "unlink on non-existent file should return nil")

-- Test 4: Remove symlink (should remove symlink, not target)
local target_file = test_dir .. "/unlink_target"
local symlink_file = test_dir .. "/unlink_symlink"
os.execute("echo 'target content' > " .. target_file)
os.execute("ln -s " .. target_file .. " " .. symlink_file)

local result = posix.unlink(symlink_file)
assert(result == 0, "unlink should succeed for symlink")

-- Verify symlink was removed but target still exists
local symlink_stat = posix.stat(symlink_file)
local target_stat = posix.stat(target_file)
assert(symlink_stat == nil, "unlink should remove symlink")
assert(target_stat ~= nil, "unlink should not remove symlink target")

-- Cleanup
posix.unlink(target_file)
