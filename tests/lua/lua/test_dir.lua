-- Test posix.dir() function
-- Tests directory listing

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Create test files
os.execute("touch " .. test_dir .. "/file1")
os.execute("touch " .. test_dir .. "/file2")
os.execute("mkdir -p " .. test_dir .. "/subdir")

-- Test 1: List directory entries
local entries = posix.dir(test_dir)
assert(entries ~= nil, "dir() should return table")
assert(type(entries) == "table", "dir() should return table type")

-- Verify we got some entries
local entry_count = 0
for i, entry in ipairs(entries) do
    entry_count = entry_count + 1
    assert(type(entry) == "string", "entry should be string")
    assert(#entry > 0, "entry should not be empty")
end
assert(entry_count > 0, "should have at least some entries")

-- Test 2: Verify specific files exist in listing
local found_file1 = false
local found_file2 = false
local found_subdir = false
for i, entry in ipairs(entries) do
    if entry == "file1" then found_file1 = true end
    if entry == "file2" then found_file2 = true end
    if entry == "subdir" then found_subdir = true end
end
assert(found_file1, "file1 should be in directory listing")
assert(found_file2, "file2 should be in directory listing")
assert(found_subdir, "subdir should be in directory listing")

-- Test 3: List current directory (no argument)
local current_entries = posix.dir()
assert(current_entries ~= nil, "dir() without argument should return table")
assert(type(current_entries) == "table", "dir() should return table type")

-- Test 4: List non-existent directory (should return nil)
local result = posix.dir("/nonexistent/directory")
assert(result == nil, "dir() on non-existent directory should return nil")

-- Cleanup: Remove test files and directories using posix functions
posix.unlink(test_dir .. "/file1")
posix.unlink(test_dir .. "/file2")
posix.rmdir(test_dir .. "/subdir")
