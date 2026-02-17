-- Test rpm.spawn() function
-- Tests spawning processes with optional redirections

local test_dir = "/tmp/epkg_spawn_test"
os.execute("rm -rf " .. test_dir)
os.execute("mkdir -p " .. test_dir)

-- Test 1: Spawn a simple command with success
local result, err, errno = rpm.spawn({"echo", "hello"})
assert(result == 0, "echo should return 0")
assert(err == nil, "echo should not error")
print("Test 1 passed: spawn simple command")

-- Test 2: Spawn a command that fails
result, err, errno = rpm.spawn({"false"})
assert(result == nil, "false should return nil on error")
assert(err ~= nil, "false should return error string")
assert(type(err) == "string", "error should be a string")
assert(errno ~= nil, "false should return errno")
print("Test 2 passed: spawn command that fails")

-- Test 3: Spawn with stdout redirection to file
local out_file = test_dir .. "/output.txt"
result, err, errno = rpm.spawn({"echo", "test output"}, {stdout = out_file})
assert(result == 0, "echo with stdout redirect should return 0")
local f = io.open(out_file, "r")
assert(f ~= nil, "output file should exist")
local content = f:read("*all")
f:close()
assert(content:match("test output"), "output file should contain expected content")
print("Test 3 passed: spawn with stdout redirection")

-- Test 4: Spawn with stderr redirection to file
local err_file = test_dir .. "/error.txt"
result, err, errno = rpm.spawn({"sh", "-c", "echo 'error message' >&2"}, {stderr = err_file})
assert(result == 0, "sh with stderr redirect should return 0")
local f = io.open(err_file, "r")
assert(f ~= nil, "error file should exist")
content = f:read("*all")
f:close()
assert(content:match("error message"), "error file should contain expected content")
print("Test 4 passed: spawn with stderr redirection")

-- Test 5: Spawn with stderr redirection to /dev/null
result, err, errno = rpm.spawn({"sh", "-c", "echo 'error message' >&2"}, {stderr = "/dev/null"})
assert(result == 0, "sh with stderr to /dev/null should return 0")
print("Test 5 passed: spawn with stderr to /dev/null")

-- Test 6: Spawn with both stdout and stderr redirection
local out_file2 = test_dir .. "/output2.txt"
local err_file2 = test_dir .. "/error2.txt"
result, err, errno = rpm.spawn({"sh", "-c", "echo 'out'; echo 'err' >&2"}, {stdout = out_file2, stderr = err_file2})
assert(result == 0, "sh with both redirects should return 0")
f = io.open(out_file2, "r")
content = f:read("*all")
f:close()
assert(content:match("out"), "output file should contain stdout")
f = io.open(err_file2, "r")
content = f:read("*all")
f:close()
assert(content:match("err"), "error file should contain stderr")
print("Test 6 passed: spawn with both stdout and stderr redirection")

-- Test 7: Empty command table should error
local ok, err_msg = pcall(function()
    rpm.spawn({})
end)
assert(ok == false, "empty command table should error")
assert(err_msg ~= nil, "error message should be provided")
-- mlua returns error as userdata, check if string representation matches
local err_str = type(err_msg) == "string" and err_msg or tostring(err_msg)
assert(err_str:match("command not supplied"), "error message should indicate command not supplied")
print("Test 7 passed: empty command table error")

-- Test 8: Invalid spawn directive should error
ok, err_msg = pcall(function()
    rpm.spawn({"echo", "test"}, {invalid = "value"})
end)
assert(ok == false, "invalid directive should error")
assert(err_msg ~= nil, "error message should be provided")
-- mlua returns error as userdata, check if string representation matches
local err_str = type(err_msg) == "string" and err_msg or tostring(err_msg)
assert(err_str:match("invalid spawn directive"), "error message should indicate invalid directive")
print("Test 8 passed: invalid spawn directive error")

-- Test 9: Spawn a command with arguments
result, err, errno = rpm.spawn({"sh", "-c", "echo 'arg1' 'arg2'"})
assert(result == 0, "sh with arguments should return 0")
print("Test 9 passed: spawn with multiple arguments")

-- Test 10: Verify the expected pattern from context works
-- This is the pattern used in actual RPM spec files
if rpm.spawn ~= nil then
    result, err, errno = rpm.spawn({"echo", "systemd-tmpfiles pattern works"}, {stderr='/dev/null'})
    assert(result == 0, "pattern from spec file should work")
    print("Test 10 passed: systemd-tmpfiles pattern from spec file")
end

-- Cleanup
os.execute("rm -rf " .. test_dir)

print("")
print("All rpm.spawn() tests passed!")
