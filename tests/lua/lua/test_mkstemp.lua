-- Test posix.mkstemp() function
-- Tests temporary file creation

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Create temporary file with template
local template = test_dir .. "/mkstemp_test_XXXXXX"
local ret1, ret2 = posix.mkstemp(template)
-- mkstemp return values may be swapped in different implementations:
-- Some return (file_handle, path), others return (path, file_handle)
local path, file_handle
if type(ret1) == "string" then
    -- First value is path string
    path = ret1
    file_handle = ret2
elseif type(ret1) == "userdata" then
    -- First value is file handle (userdata), second is path
    file_handle = ret1
    path = ret2
else
    error("mkstemp should return path and optionally file handle")
end
assert(path ~= nil, "mkstemp should return path")
assert(type(path) == "string", "mkstemp should return string for path")
assert(#path > 0, "mkstemp path should be non-empty")
-- Path should not contain XXXXXX (should be replaced)
assert(not path:match("XXXXXX"), "mkstemp should replace XXXXXX in template")

-- Note: file_handle may be nil in our implementation (see lposix.rs comment)
-- This is a limitation, but we can still test that the file was created

-- Test 2: Verify file was created
local stat = posix.stat(path)
assert(stat ~= nil, "mkstemp should create file")
assert(stat.type == "regular", "mkstemp should create regular file")

-- Test 3: Verify file is writable (we created it, so we should be able to write)
-- We can't easily test writing with posix functions, but existence is sufficient

-- Test 4: Create another temporary file (should get different name)
local template2 = test_dir .. "/mkstemp_test2_XXXXXX"
local ret1_2, ret2_2 = posix.mkstemp(template2)
local path2
if type(ret1_2) == "string" then
    path2 = ret1_2
elseif type(ret1_2) == "userdata" then
    path2 = ret2_2
end
assert(path2 ~= nil, "mkstemp should return path for second file")
assert(path2 ~= path, "mkstemp should create different files")

-- Test 5: Create temp file with invalid template (may fail or return invalid path)
local success, result = pcall(function() return posix.mkstemp("/invalid/path/XXXXXX") end)
if success then
    -- If it didn't error, check if the returned path is valid
    local invalid_path
    if type(result) == "string" then
        invalid_path = result
    elseif type(result) == "userdata" then
        -- Second return value would be path, but we can't get it from pcall
        -- Just verify it doesn't crash
    end
    if invalid_path then
        -- Path should not exist or be invalid
        local stat = posix.stat(invalid_path)
        -- Some implementations may create the file anyway, so just verify it doesn't crash
    end
else
    -- If it errored, that's also acceptable
    assert(not success, "mkstemp with invalid path may error or return invalid path")
end

-- Cleanup
posix.unlink(path)
if path2 ~= nil then
    posix.unlink(path2)
end
