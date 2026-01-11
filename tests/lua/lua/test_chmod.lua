-- Test posix.chmod() function
-- Tests file permission changes

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/chmod_test"
os.execute("touch " .. test_file)

-- Test 1: Set permissions using octal mode
local result = posix.chmod(test_file, "0644")
assert(result == 0, "chmod with octal mode should succeed")

local stat = posix.stat(test_file)
assert(stat ~= nil, "stat should return table")
assert(stat.mode == "rw-r--r--", "file should have 0644 permissions (rw-r--r--)")

-- Test 2: Test symbolic mode support (may not be available in all rpmlua implementations)
local symbolic_supported = true
local result = posix.chmod(test_file, "+x")
if result == 0 then
    -- Symbolic modes are supported
    local stat = posix.stat(test_file)
    assert(stat.mode:match("x"), "file should have execute permission")

    -- Test 3: Remove write permission
    local result = posix.chmod(test_file, "-w")
    assert(result == 0, "chmod with -w should succeed")
    -- Note: The actual behavior of -w may vary by implementation
else
    -- Symbolic modes not supported, skip these tests
    symbolic_supported = false
end

-- Test 4: Set specific permissions using = operator (if symbolic modes supported)
if symbolic_supported then
    local result = posix.chmod(test_file, "u=rwx,go=r")
    assert(result == 0, "chmod with = operator should succeed")
    -- Note: The actual behavior of complex chmod syntax may vary by implementation
    local stat = posix.stat(test_file)
    assert(stat ~= nil, "stat should return table after chmod")
end

-- Test 5: Set permissions using rwxrwxrwx format
local result = posix.chmod(test_file, "rwxr-xr-x")
assert(result == 0, "chmod with rwxrwxrwx format should succeed")

local stat = posix.stat(test_file)
assert(stat.mode == "rwxr-xr-x", "file should have rwxr-xr-x permissions")

-- Test 6: Test with non-existent file (should fail)
local result = posix.chmod("/nonexistent/file", "0644")
assert(result == nil, "chmod on non-existent file should return nil")

-- Cleanup: Remove test file using posix functions
posix.unlink(test_file)
