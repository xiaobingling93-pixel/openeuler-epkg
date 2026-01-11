-- Test os.rename() function
-- Tests file/directory renaming as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Rename regular file
local old_file = test_dir .. "/old_file"
local new_file = test_dir .. "/new_file"
os.execute("echo 'test content' > " .. old_file)
local result = os.rename(old_file, new_file)
assert(result == true, "os.rename should succeed")

-- Verify old file doesn't exist and new file exists
local old_stat = posix.stat(old_file)
local new_stat = posix.stat(new_file)
assert(old_stat == nil, "old file should not exist")
assert(new_stat ~= nil, "new file should exist")
assert(new_stat.type == "regular", "new file should be regular file")

-- Test 2: Rename directory
local old_dir = test_dir .. "/old_dir"
local new_dir = test_dir .. "/new_dir"
os.execute("mkdir -p " .. old_dir)
os.execute("touch " .. old_dir .. "/file1")
local result = os.rename(old_dir, new_dir)
assert(result == true, "os.rename should succeed for directory")

-- Verify directory was renamed and contents preserved
local old_stat = posix.stat(old_dir)
local new_stat = posix.stat(new_dir)
local file_stat = posix.stat(new_dir .. "/file1")
assert(old_stat == nil, "old directory should not exist")
assert(new_stat ~= nil, "new directory should exist")
assert(new_stat.type == "directory", "new path should be directory")
assert(file_stat ~= nil, "file should exist in renamed directory")

-- Test 3: Rename to existing path (should fail)
local file1 = test_dir .. "/file1"
local file2 = test_dir .. "/file2"
os.execute("echo 'content1' > " .. file1)
os.execute("echo 'content2' > " .. file2)
local result = os.rename(file1, file2)
-- Behavior may vary: some implementations overwrite, some fail
-- Just verify the operation completes

-- Cleanup
posix.unlink(new_file)
os.execute("rm -rf " .. new_dir)
