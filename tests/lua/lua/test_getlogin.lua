-- Test posix.getlogin() function
-- Tests login name retrieval

-- Test 1: Get login name
local login = posix.getlogin()
-- Note: getlogin() may return nil if not available (e.g., in some environments)
-- This is valid behavior, so we just check the type if it's not nil
if login ~= nil then
    assert(type(login) == "string", "getlogin should return string or nil")
    assert(#login > 0, "getlogin should return non-empty string if not nil")
    -- Login name should be alphanumeric (possibly with underscores/hyphens)
    assert(login:match("^[%w%-_]+$"), "login name should be alphanumeric")
end

-- Test 2: Verify consistency with getpasswd if available
local passwd = posix.getpasswd()
if passwd ~= nil and login ~= nil then
    -- Login name might match passwd name, but this is not guaranteed
    -- (e.g., if user logged in as different user)
    -- So we just verify both exist
    assert(passwd.name ~= nil, "passwd.name should exist")
end
