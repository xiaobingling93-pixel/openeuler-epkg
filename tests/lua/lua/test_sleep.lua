-- Test posix.sleep() function
-- Tests sleep functionality

-- Test 1: Sleep for a short duration and verify it returns remaining time (should be 0)
local start_time = os.time()
local remaining = posix.sleep(1)
local end_time = os.time()
local elapsed = end_time - start_time

-- Sleep should take approximately 1 second (allow some tolerance)
assert(elapsed >= 1, "sleep(1) should take at least 1 second")
assert(elapsed <= 2, "sleep(1) should not take more than 2 seconds")
-- Remaining should be 0 if sleep completed normally
assert(remaining == 0, "sleep should return 0 if completed normally")

-- Test 2: Sleep for 0 seconds (should return immediately)
local remaining = posix.sleep(0)
assert(remaining == 0, "sleep(0) should return 0")

-- Note: We can't easily test interrupted sleep without signals,
-- but the basic functionality is tested above
