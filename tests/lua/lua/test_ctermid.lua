-- Test posix.ctermid() function
-- Tests controlling terminal name retrieval

-- Test 1: Get controlling terminal name
local cterm = posix.ctermid()
assert(cterm ~= nil, "ctermid() should return string")
assert(type(cterm) == "string", "ctermid should return string")
assert(#cterm > 0, "ctermid should return non-empty string")
-- Controlling terminal typically starts with /dev/
assert(cterm:sub(1, 5) == "/dev/", "ctermid should start with /dev/")

-- Test 2: Verify ctermid is consistent across calls
local cterm2 = posix.ctermid()
assert(cterm2 == cterm, "ctermid should return consistent value")
