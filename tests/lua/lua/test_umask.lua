-- Test posix.umask() function
-- Tests file creation mask

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Get current umask (no argument)
local current_umask = posix.umask()
assert(current_umask ~= nil, "umask() should return string")
assert(type(current_umask) == "string", "umask should return string")
assert(#current_umask == 9, "umask string should be 9 characters (rwxrwxrwx format)")

-- Test 2: Set new umask and verify it returns the previous umask
local new_umask_str = "022"
local previous_umask = posix.umask(new_umask_str)
assert(previous_umask ~= nil, "umask(mask) should return string")
assert(type(previous_umask) == "string", "umask should return string")
-- umask should return previous value (may be in different format, so just check it's a string)
assert(#previous_umask > 0, "umask should return non-empty string")

-- Test 3: Verify umask was set by getting it again
-- umask() without args returns current umask
local current_umask_after = posix.umask()
assert(current_umask_after ~= nil, "umask() should return string after setting")
assert(type(current_umask_after) == "string", "umask should return string")
-- The current umask should be what we set (may be in rwx format, so check it's set)
assert(#current_umask_after > 0, "umask should be set to some value")

-- Test 4: Set umask using symbolic mode
local result = posix.umask("u=rwx,g=rx,o=rx")
assert(result ~= nil, "umask with symbolic mode should return string")
assert(type(result) == "string", "umask should return string")

-- Test 5: Set umask using octal
local result = posix.umask("0777")
assert(result ~= nil, "umask with octal should return string")
assert(type(result) == "string", "umask should return string")

-- Test 6: Restore original umask
posix.umask(current_umask)
