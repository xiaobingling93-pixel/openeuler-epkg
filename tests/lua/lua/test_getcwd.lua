-- Test posix.getcwd() function
-- Tests current working directory retrieval

-- Test 1: Get current working directory
local cwd = posix.getcwd()
assert(cwd ~= nil, "getcwd() should return string")
assert(type(cwd) == "string", "getcwd should return string")
assert(#cwd > 0, "getcwd should return non-empty string")
assert(cwd:sub(1, 1) == "/", "getcwd should return absolute path starting with /")

-- Test 2: Verify getcwd matches expected directory
-- We can't easily test chdir without affecting the test environment,
-- but we can verify the path is reasonable
assert(cwd ~= "", "getcwd should not return empty string")
