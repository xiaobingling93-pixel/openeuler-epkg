-- Test posix.getpasswd() function
-- Tests password entry retrieval

-- Test 1: Get current user's passwd entry (no argument)
local passwd = posix.getpasswd()
assert(passwd ~= nil, "getpasswd() should return table")
assert(type(passwd) == "table", "getpasswd() should return table type")
assert(passwd.name ~= nil, "name should exist")
assert(passwd.uid ~= nil, "uid should exist")
assert(passwd.gid ~= nil, "gid should exist")
assert(passwd.dir ~= nil, "dir should exist")
assert(passwd.shell ~= nil, "shell should exist")

-- Test 2: Get passwd entry by name
local current_user = passwd.name
local passwd_by_name = posix.getpasswd(current_user)
assert(passwd_by_name ~= nil, "getpasswd(name) should return table")
assert(passwd_by_name.name == current_user, "name should match")
assert(passwd_by_name.uid == passwd.uid, "uid should match")

-- Test 3: Get passwd entry by uid
local passwd_by_uid = posix.getpasswd(passwd.uid)
assert(passwd_by_uid ~= nil, "getpasswd(uid) should return table")
assert(passwd_by_uid.uid == passwd.uid, "uid should match")
assert(passwd_by_uid.name == passwd.name, "name should match")

-- Test 4: Get specific field - name
local name = posix.getpasswd(current_user, "name")
assert(type(name) == "string", "name selector should return string")
assert(name == current_user, "name selector should match")

-- Test 5: Get specific field - uid
local uid = posix.getpasswd(current_user, "uid")
assert(type(uid) == "number", "uid selector should return number")
assert(uid == passwd.uid, "uid selector should match")

-- Test 6: Get specific field - gid
local gid = posix.getpasswd(current_user, "gid")
assert(type(gid) == "number", "gid selector should return number")
assert(gid == passwd.gid, "gid selector should match")

-- Test 7: Get specific field - dir
local dir = posix.getpasswd(current_user, "dir")
assert(type(dir) == "string", "dir selector should return string")
assert(dir == passwd.dir, "dir selector should match")

-- Test 8: Get specific field - shell
local shell = posix.getpasswd(current_user, "shell")
assert(type(shell) == "string", "shell selector should return string")
assert(shell == passwd.shell, "shell selector should match")

-- Test 9: Get non-existent user (should return nil)
local nonexistent = posix.getpasswd("nonexistent_user_xyz123")
assert(nonexistent == nil, "non-existent user should return nil")
