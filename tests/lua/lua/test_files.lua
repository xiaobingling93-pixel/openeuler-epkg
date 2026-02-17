-- Test posix.files() function
-- Tests directory iterator function

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Create test files
os.execute("touch " .. test_dir .. "/file1")
os.execute("touch " .. test_dir .. "/file2")
os.execute("mkdir -p " .. test_dir .. "/subdir")

-- Test 1: Get files iterator for directory
local files_iter = posix.files(test_dir)
assert(files_iter ~= nil, "files() should return function")
assert(type(files_iter) == "function", "files should return iterator function")

-- Test 2: Iterate through directory entries
local entries = {}
local entry = files_iter()
while entry ~= nil do
    assert(type(entry) == "string", "iterator should return string entries")
    table.insert(entries, entry)
    entry = files_iter()
end

-- Should have at least our test files (plus . and ..)
assert(#entries >= 2, "should have at least test files in directory")

-- Test 3: Verify specific files are in the listing
local found_file1 = false
local found_file2 = false
local found_subdir = false
for _, e in ipairs(entries) do
    if e == "file1" then found_file1 = true end
    if e == "file2" then found_file2 = true end
    if e == "subdir" then found_subdir = true end
end
assert(found_file1, "file1 should be in files() listing")
assert(found_file2, "file2 should be in files() listing")
assert(found_subdir, "subdir should be in files() listing")

-- Test 4: Get files iterator for current directory (no argument)
local current_iter = posix.files()
assert(current_iter ~= nil, "files() without argument should return function")
assert(type(current_iter) == "function", "files should return iterator function")

-- Test 5: Get files iterator for non-existent directory (should error or return nil)
local success, err = pcall(function() posix.files("/nonexistent/directory") end)
-- Some implementations may error, others may return nil iterator
if success then
    -- If it didn't error, the iterator should return nil immediately
    local iter = posix.files("/nonexistent/directory")
    if iter then
        local entry = iter()
        assert(entry == nil, "iterator for non-existent directory should return nil")
    end
else
    -- If it errored, that's also acceptable
    assert(not success, "files() on non-existent directory should error or return nil iterator")
end

-- Cleanup
posix.unlink(test_dir .. "/file1")
posix.unlink(test_dir .. "/file2")
posix.rmdir(test_dir .. "/subdir")
