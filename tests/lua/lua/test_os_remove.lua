-- Test os.remove() function
-- Tests file/symlink removal using os.remove (as used in real-world RPM scripts)
-- This complements posix.unlink() tests

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Remove regular file using os.remove()
local test_file = test_dir .. "/os_remove_test"
os.execute("echo 'test content' > " .. test_file)
local result = os.remove(test_file)
assert(result == true, "os.remove should succeed for existing file")

-- Verify file was removed
local stat = posix.stat(test_file)
assert(stat == nil, "os.remove should remove file")

-- Test 2: Remove symlink using os.remove() (common pattern in RPM scripts)
local target_file = test_dir .. "/os_remove_target"
local symlink_file = test_dir .. "/os_remove_symlink"
os.execute("echo 'target content' > " .. target_file)
os.execute("ln -s " .. target_file .. " " .. symlink_file)

-- Check if symlink exists and is a link (real-world pattern)
local st = posix.stat(symlink_file)
assert(st ~= nil, "symlink should exist")
assert(st.type == "link", "should be a symlink")

-- Remove symlink using os.remove() (as in real RPM scripts)
local result = os.remove(symlink_file)
assert(result == true, "os.remove should succeed for symlink")

-- Verify symlink was removed but target still exists
local symlink_stat = posix.stat(symlink_file)
local target_stat = posix.stat(target_file)
assert(symlink_stat == nil, "os.remove should remove symlink")
assert(target_stat ~= nil, "os.remove should not remove symlink target")

-- Test 3: Remove non-existent file (should return false or nil)
local result = os.remove("/nonexistent/file")
assert(result == false or result == nil, "os.remove on non-existent file should return false or nil")

-- Cleanup
posix.unlink(target_file)
