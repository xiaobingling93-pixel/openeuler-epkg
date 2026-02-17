-- Test posix.setgid() function
-- Tests group ID setting (error cases, as we likely don't have root)

-- Test 1: Set gid using numeric ID (will likely fail without root)
local current_ids = posix.getprocessid()
local current_gid = current_ids.gid

-- Try to set to current gid (should succeed if we have permission)
local result = posix.setgid(current_gid)
-- May succeed or fail depending on permissions
assert(result == 0 or result == nil, "setgid should return 0 or nil")

-- Test 2: Set gid using group name
local passwd = posix.getpasswd()
if passwd ~= nil then
    local group = posix.getgroup(passwd.gid)
    if group ~= nil then
        local result = posix.setgid(group.name)
        -- May succeed or fail depending on permissions
        assert(result == 0 or result == nil, "setgid with name should return 0 or nil")
    end
end

-- Test 3: Set gid with invalid group name (should fail)
local result = posix.setgid("nonexistent_group_xyz123")
assert(result == nil, "setgid with invalid group should return nil")

-- Test 4: Set gid with invalid type (should error)
local success, err = pcall(function() posix.setgid({}) end)
assert(not success, "setgid with invalid type should error")
