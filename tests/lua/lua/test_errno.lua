-- Test posix.errno() function
-- Tests error number and message retrieval

-- Test 1: Get errno (should return error message and number)
local err_msg, err_num = posix.errno()
assert(err_msg ~= nil, "errno() should return error message")
assert(type(err_msg) == "string", "errno should return string for message")
assert(err_num ~= nil, "errno() should return error number")
assert(type(err_num) == "number", "errno should return number")
assert(err_num >= 0, "errno number should be non-negative")

-- Test 2: Trigger an error and check errno
-- Try to access a non-existent file to set errno
posix.stat("/nonexistent/file/path/that/does/not/exist")
local err_msg2, err_num2 = posix.errno()
assert(err_msg2 ~= nil, "errno() should return error message after error")
assert(err_num2 ~= nil, "errno() should return error number after error")
-- Error number should be ENOENT (2) or similar
assert(err_num2 > 0, "errno should be positive after file not found error")

-- Test 3: Verify errno message is non-empty
assert(#err_msg2 > 0, "errno message should be non-empty")
