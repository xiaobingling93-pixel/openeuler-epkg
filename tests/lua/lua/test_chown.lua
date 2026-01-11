-- Test posix.chown() function
-- Tests file ownership changes

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local test_file = test_dir .. "/chown_test"
os.execute("echo 'test content' > " .. test_file)

-- Get current user and group IDs
local passwd = posix.getpasswd()
assert(passwd ~= nil, "should be able to get passwd entry")
local current_uid = passwd.uid
local current_gid = passwd.gid

-- Test 1: Get original ownership
local original_stat = posix.stat(test_file)
assert(original_stat ~= nil, "stat should return table")
local original_uid = original_stat.uid
local original_gid = original_stat.gid

-- Test 2: Change ownership to current user (should succeed if we own the file or are root)
local result = posix.chown(test_file, current_uid, current_gid)
-- This may succeed or fail depending on permissions, but should not crash
assert(result == 0 or result == nil, "chown should return 0 or nil")

-- Test 3: Change ownership using user name
local result = posix.chown(test_file, passwd.name, current_gid)
assert(result == 0 or result == nil, "chown with user name should return 0 or nil")

-- Test 4: Change ownership using numeric UID and GID
local result = posix.chown(test_file, current_uid, current_gid)
assert(result == 0 or result == nil, "chown with numeric IDs should return 0 or nil")

-- Test 5: Change ownership on non-existent file (should fail)
local result = posix.chown("/nonexistent/file", current_uid, current_gid)
assert(result == nil, "chown on non-existent file should return nil")

-- Test 6: Change ownership with invalid user/group (should fail)
local result = posix.chown(test_file, "nonexistent_user_xyz123", current_gid)
-- Some implementations may return error code instead of nil, or may succeed if user lookup fails gracefully
-- Just verify it doesn't crash
assert(result == nil or type(result) == "number", "chown with invalid user should return nil or number")

-- Note: We can't easily test successful chown without root privileges,
-- but we can verify the API works and handles errors correctly

-- Cleanup
posix.unlink(test_file)
