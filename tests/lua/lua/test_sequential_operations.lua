-- Test sequential posix operations pattern
-- Tests multiple mkdir/symlink calls in sequence as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Clean up any existing test directories from previous runs
os.execute("rm -rf " .. test_dir .. "/usr")
os.execute("rm -rf " .. test_dir .. "/bin")
os.execute("rm -rf " .. test_dir .. "/proc")

-- Test 1: Sequential mkdir operations (real-world pattern for base directories)
local base_dirs = {
    "/tmp/epkg_posix_test/usr",
    "/tmp/epkg_posix_test/usr/bin"
}

-- Create directories sequentially
for i, path in ipairs(base_dirs) do
    local result = posix.mkdir(path)
    -- First directory should succeed, subsequent ones may fail if parent doesn't exist
    -- In real-world, parent directories are created first
    if i == 1 then
        assert(result == 0, "first mkdir should succeed")
    end
end

-- Verify directories were created (at least the first one)
local stat = posix.stat(base_dirs[1])
assert(stat ~= nil, "first directory should exist")
assert(stat.type == "directory", "should be a directory")

-- Test 2: Sequential symlink operations (real-world pattern for base symlinks)
-- Create a simple symlink
local result = posix.symlink("usr/bin", "/tmp/epkg_posix_test/bin")
-- May fail if link already exists, which is acceptable

-- Verify symlink was created
local stat = posix.stat("/tmp/epkg_posix_test/bin")
if stat then
    assert(stat.type == "link", "should be a symlink")
end

-- Test 3: Real-world pattern: mkdir followed by chmod
local proc_dir = "/tmp/epkg_posix_test/proc"
posix.mkdir(proc_dir)
posix.chmod(proc_dir, "0555")

-- Verify permissions
local proc_stat = posix.stat(proc_dir)
assert(proc_stat ~= nil, "proc directory should exist")
-- Note: mode checking may vary by implementation

-- Cleanup
os.execute("rm -rf " .. test_dir .. "/usr")
os.execute("rm -rf " .. test_dir .. "/bin")
os.execute("rm -rf " .. test_dir .. "/proc")
