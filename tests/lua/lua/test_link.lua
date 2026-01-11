-- Test posix.link() function
-- Tests hard link creation

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/link_source"
local link_path = test_dir .. "/link_target"
os.execute("echo 'test content' > " .. test_file)

-- Test 1: Create hard link
local result = posix.link(test_file, link_path)
assert(result == 0, "link should succeed for existing file")

-- Test 2: Verify both files exist and have same inode
local stat1 = posix.stat(test_file)
local stat2 = posix.stat(link_path)
assert(stat1 ~= nil, "source file should exist")
assert(stat2 ~= nil, "linked file should exist")
assert(stat1.ino == stat2.ino, "hard link should have same inode")

-- Test 3: Verify both files have same content
-- (We can't easily read file content with posix functions, but inode match is sufficient)

-- Test 4: Create link to non-existent file (should fail)
local result = posix.link("/nonexistent/file", link_path)
assert(result == nil, "link to non-existent file should return nil")

-- Test 5: Create link when target already exists (should fail)
local result = posix.link(test_file, link_path)
assert(result == nil, "link when target exists should return nil")

-- Cleanup
posix.unlink(link_path)
posix.unlink(test_file)
