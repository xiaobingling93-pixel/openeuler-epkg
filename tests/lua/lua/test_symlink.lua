-- Test posix.symlink() function
-- Tests symbolic link creation

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/symlink_target"
local symlink_path = test_dir .. "/symlink_link"
os.execute("echo 'test content' > " .. test_file)

-- Test 1: Create symbolic link
local result = posix.symlink(test_file, symlink_path)
assert(result == 0, "symlink should succeed")

-- Test 2: Verify symlink exists and points to target
local stat = posix.stat(symlink_path)
assert(stat ~= nil, "symlink should exist")
assert(stat.type == "link", "symlink should have type 'link'")

-- Test 3: Read symlink target
local target = posix.readlink(symlink_path)
assert(target ~= nil, "readlink should return target")
assert(target == test_file, "symlink should point to target file")

-- Test 4: Create symlink to non-existent file (should succeed - symlinks can point to non-existent files)
local broken_symlink = test_dir .. "/broken_symlink"
local result = posix.symlink("/nonexistent/file", broken_symlink)
assert(result == 0, "symlink to non-existent file should succeed")

-- Test 5: Verify broken symlink exists
local stat = posix.stat(broken_symlink)
assert(stat ~= nil, "broken symlink should exist")
assert(stat.type == "link", "broken symlink should have type 'link'")

-- Test 6: Create symlink when target already exists (should fail)
local result = posix.symlink(test_file, symlink_path)
assert(result == nil, "symlink when target exists should return nil")

-- Cleanup
posix.unlink(broken_symlink)
posix.unlink(symlink_path)
posix.unlink(test_file)
