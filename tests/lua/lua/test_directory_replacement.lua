-- Test directory replacement pattern
-- Tests the .rpmmoved pattern used in real-world RPM scripts for directory replacement
-- See: https://fedoraproject.org/wiki/Packaging:Directory_Replacement

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Basic directory replacement pattern
local directories = {
    test_dir .. "/dir1",
    test_dir .. "/dir2"
}

-- Create directories
for i, path in ipairs(directories) do
    os.execute("mkdir -p " .. path)
    os.execute("touch " .. path .. "/file" .. i)
end

-- Real-world pattern: rename directories before replacement
for i, path in ipairs(directories) do
    local st = posix.stat(path)
    if st and st.type == "directory" then
        local moved_path = path .. ".rpmmoved"
        -- Remove .rpmmoved if it exists from previous test
        if posix.stat(moved_path) ~= nil then
            os.execute("rm -rf " .. moved_path)
        end
        local status = os.rename(path, moved_path)
        assert(status == true, "rename should succeed")
    end
end

-- Verify directories were renamed
for i, path in ipairs(directories) do
    local moved_path = path .. ".rpmmoved"
    local stat = posix.stat(moved_path)
    assert(stat ~= nil, "renamed directory should exist")
    assert(stat.type == "directory", "should be a directory")

    -- Verify files are still in renamed directory
    local file_stat = posix.stat(moved_path .. "/file" .. i)
    assert(file_stat ~= nil, "file should exist in renamed directory")
end

-- Test 2: Directory replacement with suffix increment when target exists
local path1 = test_dir .. "/replace1"
local path2 = test_dir .. "/replace2"
-- Clean up if exists from previous test
os.execute("rm -rf " .. path1)
os.execute("rm -rf " .. path2)
os.execute("rm -rf " .. path2 .. ".rpmmoved*")
os.execute("mkdir -p " .. path1)
os.execute("mkdir -p " .. path2)

-- First rename
local moved1 = path2 .. ".rpmmoved"
os.rename(path2, moved1)

-- Try to rename path1 to path2.rpmmoved (should fail, then increment)
local moved2 = path2 .. ".rpmmoved"
local status = os.rename(path1, moved2)
if not status then
    -- If rename failed, increment suffix
    local suffix = 0
    local moved_success = false
    while not moved_success do
        suffix = suffix + 1
        local temp_name = moved2 .. "." .. suffix
        moved_success = os.rename(moved1, temp_name)
    end
    os.rename(path1, moved2)

    -- Verify incremented path exists
    local stat2 = posix.stat(moved2 .. "." .. suffix)
    assert(stat2 ~= nil, "incremented path should exist")
end

-- Verify final state
local stat1 = posix.stat(moved2)
assert(stat1 ~= nil, "renamed path should exist")

-- Test 3: Real-world pattern: iterate over directory and move files
local source_dir = test_dir .. "/source"
local dest_dir = test_dir .. "/dest"
os.execute("mkdir -p " .. source_dir)
os.execute("mkdir -p " .. dest_dir)
os.execute("touch " .. source_dir .. "/file1")
os.execute("touch " .. source_dir .. "/file2")

-- Check if source is directory and move files
st = posix.stat(source_dir)
if st and st.type == "directory" then
    local entries = posix.dir(source_dir)
    if entries then
        for i, filename in ipairs(entries) do
            if filename ~= "." and filename ~= ".." then
                local src_path = source_dir .. "/" .. filename
                local dst_path = dest_dir .. "/" .. filename
                os.rename(src_path, dst_path)
            end
        end
    end
end

-- Verify files were moved
local file1_stat = posix.stat(dest_dir .. "/file1")
local file2_stat = posix.stat(dest_dir .. "/file2")
assert(file1_stat ~= nil, "file1 should be in dest")
assert(file2_stat ~= nil, "file2 should be in dest")

local src_file1_stat = posix.stat(source_dir .. "/file1")
assert(src_file1_stat == nil, "file1 should not be in source")

-- Cleanup
for i, path in ipairs(directories) do
    os.execute("rm -rf " .. path .. ".rpmmoved")
end
os.execute("rm -rf " .. moved2)
os.execute("rm -rf " .. moved2 .. ".1")
os.execute("rm -rf " .. source_dir)
os.execute("rm -rf " .. dest_dir)
