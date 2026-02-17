-- Test posix.setuid() function
-- Tests user ID setting (error cases, as we likely don't have root)

-- Test 1: Set uid using numeric ID (will likely fail without root)
local current_ids = posix.getprocessid()
local current_uid = current_ids.uid

-- Try to set to current uid (should succeed if we have permission)
local result = posix.setuid(current_uid)
-- May succeed or fail depending on permissions
assert(result == 0 or result == nil, "setuid should return 0 or nil")

-- Test 2: Set uid using user name
local passwd = posix.getpasswd()
if passwd ~= nil then
    local result = posix.setuid(passwd.name)
    -- May succeed or fail depending on permissions
    assert(result == 0 or result == nil, "setuid with name should return 0 or nil")
end

-- Test 3: Set uid with invalid user name (should fail)
local result = posix.setuid("nonexistent_user_xyz123")
assert(result == nil, "setuid with invalid user should return nil")

-- Test 4: Set uid with invalid type (should error)
local success, err = pcall(function() posix.setuid({}) end)
assert(not success, "setuid with invalid type should error")
