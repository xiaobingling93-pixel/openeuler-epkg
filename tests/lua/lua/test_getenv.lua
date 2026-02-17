-- Test posix.getenv() function
-- Tests environment variable access

-- Test 1: Get existing environment variable
local path = posix.getenv("PATH")
assert(path ~= nil, "PATH environment variable should exist")
assert(type(path) == "string", "PATH should be a string")
assert(#path > 0, "PATH should not be empty")

-- Test 2: Get non-existent environment variable
local nonexistent = posix.getenv("EPKG_TEST_NONEXISTENT_VAR")
assert(nonexistent == nil, "non-existent variable should return nil")

-- Test 3: Get all environment variables (no argument)
local all_env = posix.getenv()
assert(all_env ~= nil, "getenv() should return table")
assert(type(all_env) == "table", "getenv() should return table type")

-- Verify some common environment variables exist
assert(all_env.PATH ~= nil, "PATH should be in environment table")
assert(all_env.HOME ~= nil, "HOME should be in environment table")
assert(all_env.USER ~= nil, "USER should be in environment table")

-- Test 4: Set and get custom environment variable
posix.setenv("EPKG_TEST_VAR", "test_value", true)
local value = posix.getenv("EPKG_TEST_VAR")
assert(value == "test_value", "custom variable should be retrievable")

-- Test 5: Overwrite existing variable
posix.setenv("EPKG_TEST_VAR", "new_value", true)
local value = posix.getenv("EPKG_TEST_VAR")
assert(value == "new_value", "variable should be overwritten")

-- Test 6: Set without overwrite (should not change)
posix.setenv("EPKG_TEST_VAR", "should_not_change", false)
local value = posix.getenv("EPKG_TEST_VAR")
assert(value == "new_value", "variable should not change when overwrite=false")

-- Cleanup
posix.unsetenv("EPKG_TEST_VAR")
