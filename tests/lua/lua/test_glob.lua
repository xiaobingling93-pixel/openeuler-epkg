-- Test rpm.glob() function
-- Tests pattern matching for file paths

local test_dir = "/tmp/epkg_glob_test"
os.execute("rm -rf " .. test_dir)
os.execute("mkdir -p " .. test_dir)
os.execute("mkdir -p " .. test_dir .. "/subdir1")
os.execute("mkdir -p " .. test_dir .. "/subdir2")

-- Create test files
os.execute("echo 'test1' > " .. test_dir .. "/file1.txt")
os.execute("echo 'test2' > " .. test_dir .. "/file2.txt")
os.execute("echo 'test3' > " .. test_dir .. "/file3.log")
os.execute("echo 'test4' > " .. test_dir .. "/another.txt")
os.execute("echo 'test5' > " .. test_dir .. "/subdir1/nested.txt")
os.execute("echo 'test6' > " .. test_dir .. "/subdir2/other.log")

-- Test 1: Basic glob - all files in directory
local files = rpm.glob(test_dir .. "/*")
assert(files ~= nil, "glob should return a table")
assert(type(files) == "table", "glob should return table")
assert(#files > 0, "glob should find files")

-- Verify we found expected items
local found = {}
for i, f in ipairs(files) do
    found[f] = true
end
assert(found[test_dir .. "/file1.txt"] ~= nil or found[test_dir .. "/file2.txt"] ~= nil, "glob should find test files")
assert(found[test_dir .. "/subdir1"] ~= nil, "glob should find subdir1")
assert(found[test_dir .. "/subdir2"] ~= nil, "glob should find subdir2")

-- Test 2: Pattern matching - *.txt
local txt_files = rpm.glob(test_dir .. "/*.txt")
assert(txt_files ~= nil, "glob with *.txt pattern should return table")
assert(#txt_files >= 3, "glob should find at least 3 txt files")

-- Verify all results are .txt files
for i, f in ipairs(txt_files) do
    assert(f:match("%.txt$") ~= nil, f .. " should be a .txt file")
end

-- Test 3: Pattern matching - *.log
local log_files = rpm.glob(test_dir .. "/*.log")
assert(log_files ~= nil, "glob with *.log pattern should return table")
assert(#log_files >= 1, "glob should find at least 1 log file")

-- Verify all results are .log files
for i, f in ipairs(log_files) do
    assert(f:match("%.log$") ~= nil, f .. " should be a .log file")
end

-- Test 4: Recursive glob - **
local all_files = rpm.glob(test_dir .. "/**/*")
assert(all_files ~= nil, "recursive glob should return table")
assert(#all_files > 0, "recursive glob should find files")

-- Verify nested file is found
local found_nested = false
for i, f in ipairs(all_files) do
    if f:match("nested%.txt$") then
        found_nested = true
        break
    end
end
assert(found_nested, "recursive glob should find nested.txt")

-- Test 5: Pattern with ? single character wildcard
local single_char = rpm.glob(test_dir .. "/file?.txt")
assert(single_char ~= nil, "glob with ? wildcard should return table")
assert(#single_char >= 2, "glob should find files matching file?.txt")

-- Test 6: Pattern with [] character class
local bracket_pattern = rpm.glob(test_dir .. "/file[12].txt")
assert(bracket_pattern ~= nil, "glob with [] should return table")
assert(#bracket_pattern == 2, "glob should find exactly 2 files matching file[12].txt")

-- Verify correct files found
local found_file1 = false
local found_file2 = false
for i, f in ipairs(bracket_pattern) do
    if f:match("file1%.txt$") then found_file1 = true end
    if f:match("file2%.txt$") then found_file2 = true end
end
assert(found_file1 and found_file2, "glob should find file1.txt and file2.txt")

-- Test 7: No matches without NOCHECK - should return empty table
local nomatch = rpm.glob(test_dir .. "/*.nonexistent")
assert(nomatch ~= nil, "glob with no matches should return table (may be empty)")
assert(type(nomatch) == "table", "glob with no matches should return table")
assert(#nomatch == 0, "glob with no matches should return empty table")

-- Test 8: No matches with NOCHECK flag - should return pattern
local nomatch_noc = rpm.glob(test_dir .. "/*.nonexistent", "c")
assert(nomatch_noc ~= nil, "glob with NOCHECK and no matches should return table")
assert(type(nomatch_noc) == "table", "glob with NOCHECK should return table")
assert(#nomatch_noc == 1, "glob with NOCHECK should return single item")
assert(nomatch_noc[1] == test_dir .. "/*.nonexistent", "NOCHECK should return original pattern")

-- Test 9: NOCHECK with multiple flags - 'c' in middle
local nomatch_noc2 = rpm.glob(test_dir .. "/*.nope", "xc")
assert(nomatch_noc2 ~= nil, "glob with NOCHECK flag 'c' in string should work")
assert(nomatch_noc2[1] == test_dir .. "/*.nope", "NOCHECK should return pattern regardless of other flags")

-- Test 10: Matches with NOCHECK - should return matches (not pattern)
local matches_noc = rpm.glob(test_dir .. "/*.txt", "c")
assert(matches_noc ~= nil, "glob with NOCHECK and matches should return table")
assert(#matches_noc >= 3, "glob with NOCHECK should find files")
assert(matches_noc[1] ~= test_dir .. "/*.txt", "with matches, should return files not pattern")

-- Test 11: Invalid pattern - should raise error
local ok, err = pcall(function()
    rpm.glob(test_dir .. "/*[invalid")
end)
assert(ok == false, "glob with invalid pattern should raise error")
assert(err ~= nil, "error should be returned")

-- Test 12: Empty pattern behavior - should return empty table for empty directory
local empty_dir = test_dir .. "/empty_dir"
os.execute("mkdir -p " .. empty_dir)
local empty_files = rpm.glob(empty_dir .. "/*")
assert(empty_files ~= nil, "glob on empty directory should return table")
assert(type(empty_files) == "table", "glob should return table")
assert(#empty_files == 0, "glob on empty directory should return empty table")

-- Test 13: Pattern matching directories only
local dirs = rpm.glob(test_dir .. "/*/")
assert(dirs ~= nil, "glob with */ pattern should return table")
assert(#dirs >= 2, "glob with */ should find at least 2 directories")

-- Cleanup
os.execute("rm -rf " .. test_dir)
