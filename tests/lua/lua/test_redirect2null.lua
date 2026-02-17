-- Test posix.redirect2null() function
-- Tests redirecting file descriptors to /dev/null

local test_dir = "/tmp/epkg_redirect2null_test"
os.execute("rm -rf " .. test_dir)
os.execute("mkdir -p " .. test_dir)

-- Test 1: redirect2null should fail without fork() first
local ok, err_msg = pcall(function()
    posix.redirect2null(2)
end)
assert(ok == false, "redirect2null should fail without fork()")
assert(err_msg ~= nil, "error message should be provided")
-- mlua returns error as userdata, check if string representation matches
local err_str = type(err_msg) == "string" and err_msg or tostring(err_msg)
assert(err_str:match("not permitted in this context"), "error should indicate context not permitted")
print("Test 1 passed: redirect2null fails without fork()")

-- Test 2: redirect2null after fork() in child process
local pid = posix.fork()
if pid == 0 then
    -- In child process: redirect stderr to /dev/null, then exec
    posix.redirect2null(2)
    posix.exec("/bin/echo", "test message")
elseif pid > 0 then
    -- In parent process: wait for child
    local result, err, errno = posix.wait(pid)
    assert(result ~= -1, "wait should succeed")
    print("Test 2 passed: redirect2null after fork() in child")
else
    error("fork failed")
end

-- Test 3: redirect2null with stdout (fd 1)
pid = posix.fork()
if pid == 0 then
    posix.redirect2null(1)
    -- This output should go to /dev/null
    posix.exec("/bin/echo", "this should disappear")
elseif pid > 0 then
    posix.wait(pid)
    print("Test 3 passed: redirect2null stdout in child")
else
    error("fork failed")
end

-- Test 4: redirect2null with stderr (fd 2) in a real scenario
pid = posix.fork()
if pid == 0 then
    posix.redirect2null(2)
    posix.exec("/bin/sh", "-c", "echo 'error message' >&2; echo 'normal output'")
elseif pid > 0 then
    posix.wait(pid)
    print("Test 4 passed: redirect2null stderr in real scenario")
else
    error("fork failed")
end

-- Test 5: Verify the expected pattern from context works
-- This is the pattern used in actual RPM spec files
if posix.redirect2null ~= nil and posix.fork ~= nil and posix.exec ~= nil and posix.wait ~= nil then
    pid = posix.fork()
    if pid == 0 then
        posix.redirect2null(2)
        posix.exec("/bin/echo", "pattern from spec file works")
    elseif pid > 0 then
        local result, err, errno = posix.wait(pid)
        assert(result ~= -1, "wait should succeed")
        print("Test 5 passed: full pattern from spec file works")
    else
        error("fork failed")
    end
end

-- Cleanup
os.execute("rm -rf " .. test_dir)

print("")
print("All posix.redirect2null() tests passed!")
