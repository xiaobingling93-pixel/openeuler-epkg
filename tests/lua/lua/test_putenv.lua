-- Test posix.putenv() function
-- Tests environment variable setting via putenv

-- Test 1: Set environment variable using putenv
local test_var = "EPKG_TEST_PUTENV"
local test_value = "test_putenv_value"
local env_string = test_var .. "=" .. test_value
local result = posix.putenv(env_string)
assert(result == 0, "putenv should succeed")

-- Test 2: Verify variable was set using getenv
local value = posix.getenv(test_var)
assert(value ~= nil, "putenv variable should be retrievable")
assert(value == test_value, "putenv variable should have correct value")

-- Test 3: Overwrite existing variable
local new_value = "new_putenv_value"
local new_env_string = test_var .. "=" .. new_value
local result = posix.putenv(new_env_string)
assert(result == 0, "putenv should succeed for overwrite")

local value = posix.getenv(test_var)
assert(value == new_value, "putenv should overwrite existing variable")

-- Test 4: Test with empty value
local empty_env_string = test_var .. "="
local result = posix.putenv(empty_env_string)
assert(result == 0, "putenv should succeed with empty value")

local value = posix.getenv(test_var)
assert(value == "", "putenv should set empty value")

-- Cleanup: Remove the test variable
posix.unsetenv(test_var)
