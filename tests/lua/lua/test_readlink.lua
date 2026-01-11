-- Test posix.readlink() function
-- Tests symbolic link target reading

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/readlink_target"
local symlink_path = test_dir .. "/readlink_symlink"
os.execute("echo 'test content' > " .. test_file)
os.execute("ln -s " .. test_file .. " " .. symlink_path)

-- Test 1: Read symbolic link target
local target = posix.readlink(symlink_path)
assert(target ~= nil, "readlink should return string for valid symlink")
assert(type(target) == "string", "readlink should return string")
assert(target == test_file or target:match(test_file), "readlink should return symlink target")

-- Test 2: Read link on regular file (should fail)
local result = posix.readlink(test_file)
assert(result == nil, "readlink on regular file should return nil")

-- Test 3: Read link on non-existent file (should fail)
local result = posix.readlink("/nonexistent/symlink")
assert(result == nil, "readlink on non-existent file should return nil")

-- Cleanup
posix.unlink(symlink_path)
posix.unlink(test_file)
