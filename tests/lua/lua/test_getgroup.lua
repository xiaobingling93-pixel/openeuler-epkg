-- Test posix.getgroup() function
-- Tests group entry retrieval

-- First get current user's group
local passwd = posix.getpasswd()
assert(passwd ~= nil, "should be able to get passwd entry")
local current_gid = passwd.gid

-- Test 1: Get group entry by gid
local group = posix.getgroup(current_gid)
assert(group ~= nil, "getgroup(gid) should return table")
assert(type(group) == "table", "getgroup() should return table type")
assert(group.name ~= nil, "name should exist")
assert(group.gid ~= nil, "gid should exist")
assert(group.gid == current_gid, "gid should match")

-- Test 2: Get group entry by name
local group_name = group.name
local group_by_name = posix.getgroup(group_name)
assert(group_by_name ~= nil, "getgroup(name) should return table")
assert(group_by_name.name == group_name, "name should match")
assert(group_by_name.gid == current_gid, "gid should match")

-- Test 3: Verify members are stored as numeric indices
-- (Members may be empty, which is fine)
local has_members = false
for i = 1, 10 do
    if group[i] ~= nil then
        has_members = true
        assert(type(group[i]) == "string", "member should be string")
    end
end
-- It's OK if there are no members

-- Test 4: Get non-existent group (should return nil)
local nonexistent = posix.getgroup("nonexistent_group_xyz123")
assert(nonexistent == nil, "non-existent group should return nil")

-- Test 5: Get non-existent gid (should return nil)
local nonexistent_gid = posix.getgroup(999999)
assert(nonexistent_gid == nil, "non-existent gid should return nil")
