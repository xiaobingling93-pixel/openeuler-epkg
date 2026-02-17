-- Test posix.chdir() function
-- Tests directory changing

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Save current directory
local original_cwd = posix.getcwd()
assert(original_cwd ~= nil, "should be able to get current directory")

-- Test 2: Change to test directory
local result = posix.chdir(test_dir)
assert(result == 0, "chdir should succeed for existing directory")

-- Test 3: Verify we're in the new directory
local new_cwd = posix.getcwd()
assert(new_cwd ~= nil, "should be able to get current directory after chdir")
assert(new_cwd == test_dir, "chdir should change to specified directory")

-- Test 4: Change back to original directory
local result = posix.chdir(original_cwd)
assert(result == 0, "chdir should succeed to restore original directory")

-- Test 5: Verify we're back
local restored_cwd = posix.getcwd()
assert(restored_cwd == original_cwd, "chdir should restore original directory")

-- Test 6: Change to non-existent directory (should fail)
local result = posix.chdir("/nonexistent/directory/path")
assert(result == nil, "chdir to non-existent directory should return nil")

-- Verify we're still in original directory
local final_cwd = posix.getcwd()
assert(final_cwd == original_cwd, "failed chdir should not change directory")
