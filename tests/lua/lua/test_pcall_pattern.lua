-- Test pcall() pattern for error handling
-- Tests protected function calls as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Basic pcall with successful function
local function success_func()
    return "success"
end
local success, result = pcall(success_func)
assert(success == true, "pcall should return true for successful function")
assert(result == "success", "pcall should return function result")

-- Test 2: pcall with function that errors
local function error_func()
    error("test error")
end
local success, err = pcall(error_func)
assert(success == false, "pcall should return false for error")
assert(err ~= nil, "pcall should return error message")

-- Test 3: Real-world pattern: pcall with file operations that might fail
local function create_script()
    local temp_path = test_dir .. "/script_file"
    local file = io.open(temp_path, "w")
    if file then
        file:write("#!/bin/sh\necho 'test'\n")
        file:close()
        return true
    end
    return false
end

local success, result = pcall(create_script)
assert(success == true, "pcall should succeed for create_script")
assert(result == true, "create_script should return true")

-- Verify file was created
local stat = posix.stat(test_dir .. "/script_file")
assert(stat ~= nil, "script file should exist")

-- Test 4: Real-world pattern: pcall with mkdir that might fail
local function create_dir()
    os.execute("mkdir -p " .. test_dir .. "/pretrans_dir")
    return true
end

local success, result = pcall(create_dir)
assert(success == true, "pcall should succeed for create_dir")
assert(result == true, "create_dir should return true")

-- Test 5: Real-world pattern: pcall with operations that might fail in netinst
local function might_fail_operation()
    -- Simulate operation that might fail
    local path = "/nonexistent/path/operation"
    local file = io.open(path, "w")
    if file then
        file:write("test")
        file:close()
        return true
    end
    return false
end

local success, result = pcall(might_fail_operation)
-- Should not error even if operation fails
assert(success == true, "pcall should not error even if operation fails")
-- Result may be false, which is acceptable

-- Test 6: Real-world pattern: conditional execution based on pcall
local function conditional_operation()
    os.execute("mkdir -p " .. test_dir .. "/conditional_dir")
    return true
end

if pcall(conditional_operation) then
    -- Operation succeeded
    local stat = posix.stat(test_dir .. "/conditional_dir")
    assert(stat ~= nil, "conditional directory should exist")
else
    -- Operation failed (should not reach here in this test)
    assert(false, "should not reach else branch")
end

-- Cleanup
posix.unlink(test_dir .. "/script_file")
os.execute("rm -rf " .. test_dir .. "/pretrans_dir")
os.execute("rm -rf " .. test_dir .. "/conditional_dir")
