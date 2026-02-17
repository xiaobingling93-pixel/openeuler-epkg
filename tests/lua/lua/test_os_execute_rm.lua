-- Test os.execute("rm -rf") pattern
-- Tests shell command execution for recursive removal as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Basic os.execute("rm -rf") pattern
local test_dir_path = test_dir .. "/rm_test_dir"
os.execute("mkdir -p " .. test_dir_path)
os.execute("touch " .. test_dir_path .. "/file1")
os.execute("mkdir -p " .. test_dir_path .. "/subdir")
os.execute("touch " .. test_dir_path .. "/subdir/file2")

-- Verify directory exists
local stat = posix.stat(test_dir_path)
assert(stat ~= nil, "directory should exist")
assert(stat.type == "directory", "should be a directory")

-- Remove using os.execute("rm -rf")
local result = os.execute("rm -rf " .. test_dir_path)
-- os.execute returns exit status, 0 means success, but may be true/false in some implementations
assert(result == 0 or result == true, "rm -rf should succeed")

-- Verify directory was removed
local stat_after = posix.stat(test_dir_path)
assert(stat_after == nil, "directory should be removed")

-- Test 2: Real-world pattern: check if directory exists, then remove
local check_dir = test_dir .. "/check_rm_dir"
os.execute("mkdir -p " .. check_dir)
os.execute("touch " .. check_dir .. "/file")

local st = posix.stat(check_dir)
if st and st.type == "directory" then
    os.execute("rm -rf " .. check_dir)
end

-- Verify directory was removed
local stat_check = posix.stat(check_dir)
assert(stat_check == nil, "directory should be removed after conditional rm")

-- Test 3: Real-world pattern: remove multiple paths
local paths = {
    test_dir .. "/rm_path1",
    test_dir .. "/rm_path2",
    test_dir .. "/rm_path3"
}

-- Create directories
for i, path in ipairs(paths) do
    os.execute("mkdir -p " .. path)
    os.execute("touch " .. path .. "/file" .. i)
end

-- Remove all using loop
for i, path in ipairs(paths) do
    local st = posix.stat(path)
    if st and st.type == "directory" then
        os.execute("rm -rf " .. path)
    end
end

-- Verify all were removed
for i, path in ipairs(paths) do
    local stat = posix.stat(path)
    assert(stat == nil, "path should be removed")
end

-- Test 4: Real-world pattern: remove with variable path (like nodejs_sitelib)
local sitelib_dir = test_dir .. "/sitelib"
local package_dir1 = sitelib_dir .. "/package1"
local package_dir2 = sitelib_dir .. "/package2"
os.execute("mkdir -p " .. package_dir1)
os.execute("mkdir -p " .. package_dir2)
os.execute("touch " .. package_dir1 .. "/file1")
os.execute("touch " .. package_dir2 .. "/file2")

-- Remove specific package directories
local packages = {"package1", "package2"}
for i, pkg in ipairs(packages) do
    local path = sitelib_dir .. "/" .. pkg
    local st = posix.stat(path)
    if st and st.type == "directory" then
        os.execute("rm -rf " .. path)
    end
end

-- Verify packages were removed
local stat1 = posix.stat(package_dir1)
local stat2 = posix.stat(package_dir2)
assert(stat1 == nil, "package1 should be removed")
assert(stat2 == nil, "package2 should be removed")

-- Verify sitelib directory still exists
local stat_sitelib = posix.stat(sitelib_dir)
assert(stat_sitelib ~= nil, "sitelib directory should still exist")

-- Cleanup
os.execute("rm -rf " .. sitelib_dir)
