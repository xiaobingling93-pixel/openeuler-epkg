-- Test stat checking patterns before property access
-- Tests the common patterns: if st and st.type == "link" then ... and if posix.stat(path, "type") == "link" then ...
-- as used extensively in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Pattern using full stat table: if st and st.type == "link" then
local test_file = test_dir .. "/stat_check_file"
local test_link = test_dir .. "/stat_check_link"
os.execute("echo 'content' > " .. test_file)
os.execute("ln -s " .. test_file .. " " .. test_link)

local st = posix.stat(test_link)
if st and st.type == "link" then
    os.remove(test_link)
end

-- Verify symlink was removed
local stat_after = posix.stat(test_link)
assert(stat_after == nil, "symlink should be removed after conditional check")

-- Test 2: Pattern using field selector: if posix.stat(path, "type") == "link" then
local test_file2 = test_dir .. "/stat_check_file2"
local test_link2 = test_dir .. "/stat_check_link2"
os.execute("echo 'content' > " .. test_file2)
os.execute("ln -s " .. test_file2 .. " " .. test_link2)

if posix.stat(test_link2, "type") == "link" then
    os.remove(test_link2)
end

-- Verify symlink was removed
local stat_after2 = posix.stat(test_link2)
assert(stat_after2 == nil, "symlink should be removed after field selector check")

-- Test 3: Check stat result before accessing properties (directory pattern)
local test_dir_path = test_dir .. "/stat_check_dir"
os.execute("mkdir -p " .. test_dir_path)

st = posix.stat(test_dir_path)
if st and st.type == "directory" then
    -- Directory exists, do something
    local entries = posix.dir(test_dir_path)
    assert(entries ~= nil, "should be able to list directory")
end

-- Test 4: Check stat result for non-existent path
local nonexistent = test_dir .. "/nonexistent"
st = posix.stat(nonexistent)
if st and st.type == "link" then
    os.remove(nonexistent)
end
-- Should not error even if path doesn't exist

-- Test 5: Real-world pattern: check type and remove, then create directory (using field selector)
local path = test_dir .. "/check_path"
os.execute("ln -s /nonexistent " .. path)

if posix.stat(path, "type") == "link" then
    os.remove(path)
    posix.mkdir(path)
end

-- Verify it's now a directory
local stat2 = posix.stat(path)
assert(stat2 ~= nil, "path should exist")
assert(stat2.type == "directory", "should be a directory")

-- Test 6: Real-world pattern: check multiple paths in loop (using full stat table)
local paths = {
    test_dir .. "/path1",
    test_dir .. "/path2",
    test_dir .. "/path3"
}

-- Create some paths as symlinks
os.execute("ln -s /nonexistent " .. paths[1])
os.execute("mkdir -p " .. paths[2])
-- Leave path3 non-existent

for i, p in ipairs(paths) do
    st = posix.stat(p)
    if st and st.type == "link" then
        os.remove(p)
    end
end

-- Verify path1 (symlink) was removed
local stat1 = posix.stat(paths[1])
assert(stat1 == nil, "symlink should be removed")

-- Verify path2 (directory) still exists
local stat2_check = posix.stat(paths[2])
assert(stat2_check ~= nil, "directory should still exist")
assert(stat2_check.type == "directory", "should be a directory")

-- Test 7: Real-world pattern: check type in loop (using field selector)
local paths2 = {
    test_dir .. "/path4",
    test_dir .. "/path5"
}

-- Create mix of symlinks and directories
os.execute("ln -s /nonexistent " .. paths2[1])
os.execute("mkdir -p " .. paths2[2])

for i, p in ipairs(paths2) do
    if posix.stat(p, "type") == "link" then
        os.remove(p)
        posix.mkdir(p)
    end
end

-- Verify path4 is now directory
local stat4 = posix.stat(paths2[1])
assert(stat4 ~= nil, "path4 should exist")
assert(stat4.type == "directory", "path4 should be directory")

-- Test 8: Real-world pattern: check stat with specific field selector
local test_path = test_dir .. "/type_check"
os.execute("echo 'test' > " .. test_path)

local path_type = posix.stat(test_path, "type")
if path_type == "regular" then
    -- File is regular file
    local size = posix.stat(test_path, "size")
    assert(size > 0, "file should have size")
end

-- Test 9: Real-world pattern: nested conditional checking
local complex_path = test_dir .. "/complex"
os.execute("mkdir -p " .. complex_path)

st = posix.stat(complex_path)
if st and st.type == "directory" then
    local entries = posix.dir(complex_path)
    if entries then
        for i, filename in ipairs(entries) do
            local file_path = complex_path .. "/" .. filename
            local file_st = posix.stat(file_path)
            if file_st and file_st.type == "regular" then
                -- Process regular file
            end
        end
    end
end

-- Cleanup
posix.unlink(test_file)
posix.unlink(test_file2)
os.execute("rm -rf " .. test_dir_path)
os.execute("rm -rf " .. path)
os.execute("rm -rf " .. paths[2])
os.execute("rm -rf " .. paths2[1])
os.execute("rm -rf " .. paths2[2])
os.execute("rm -rf " .. test_path)
os.execute("rm -rf " .. complex_path)
