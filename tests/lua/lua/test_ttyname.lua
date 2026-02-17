-- Test posix.ttyname() function
-- Tests terminal name retrieval

-- Test 1: Get ttyname for stdin (fd 0)
local tty_stdin = posix.ttyname(0)
-- May be nil if not a TTY, which is valid
if tty_stdin ~= nil then
    assert(type(tty_stdin) == "string", "ttyname should return string or nil")
    assert(#tty_stdin > 0, "ttyname should return non-empty string if not nil")
    -- TTY names typically start with /dev/
    assert(tty_stdin:sub(1, 5) == "/dev/", "ttyname should start with /dev/")
end

-- Test 2: Get ttyname for stdout (fd 1)
local tty_stdout = posix.ttyname(1)
if tty_stdout ~= nil then
    assert(type(tty_stdout) == "string", "ttyname should return string or nil")
    -- stdin and stdout should have same TTY if both are TTYs
    if tty_stdin ~= nil then
        assert(tty_stdout == tty_stdin, "stdin and stdout should have same TTY")
    end
end

-- Test 3: Get ttyname for stderr (fd 2)
local tty_stderr = posix.ttyname(2)
if tty_stderr ~= nil then
    assert(type(tty_stderr) == "string", "ttyname should return string or nil")
    -- All three should have same TTY if they're all TTYs
    if tty_stdin ~= nil and tty_stdout ~= nil then
        assert(tty_stderr == tty_stdin, "stderr should have same TTY as stdin")
    end
end

-- Test 4: Get ttyname for invalid file descriptor (should return nil)
local tty_invalid = posix.ttyname(999)
assert(tty_invalid == nil, "ttyname for invalid fd should return nil")

-- Test 5: Get ttyname with default (fd 0)
local tty_default = posix.ttyname()
if tty_default ~= nil then
    assert(type(tty_default) == "string", "ttyname() should return string or nil")
    if tty_stdin ~= nil then
        assert(tty_default == tty_stdin, "ttyname() should match ttyname(0)")
    end
end
