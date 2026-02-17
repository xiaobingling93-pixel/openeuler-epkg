-- Test posix.access() in conditional patterns
-- Tests posix.access() used in real-world RPM scripts for conditional execution

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Real-world pattern: if not posix.access() then create file
local config_file = test_dir .. "/config"
if not posix.access(config_file) then
    local cf = io.open(config_file, "w")
    if cf then
        cf:write("DEFAULT\n")
        cf:close()
    end
end

-- Verify file was created
local stat = posix.stat(config_file)
assert(stat ~= nil, "config file should exist")

-- Test 2: Real-world pattern: if posix.access(..., "x") then execute
local test_script = test_dir .. "/test_script"
os.execute("echo '#!/bin/sh\necho test' > " .. test_script)
os.execute("chmod +x " .. test_script)

if posix.access(test_script, "x") then
    -- Script is executable, would execute it here
    local result = os.execute(test_script .. " >/dev/null 2>&1")
    -- os.execute returns exit status, 0 means success, but may be true/false in some implementations
    assert(result == 0 or result == true, "executable script should run")
end

-- Test 3: Real-world pattern: check access before operations
local state_file = test_dir .. "/state"
if not posix.access(state_file) then
    local sf = io.open(state_file, "w")
    if sf then
        sf:write("DEFAULT\n")
        sf:close()
    end
end

-- Verify state file was created
local stat2 = posix.stat(state_file)
assert(stat2 ~= nil, "state file should exist")

-- Test 4: Real-world pattern: check executable and execute command
local test_bin = test_dir .. "/test_bin"
os.execute("echo '#!/bin/sh\necho done' > " .. test_bin)
os.execute("chmod +x " .. test_bin)

if posix.access(test_bin, "x") then
    local result = os.execute(test_bin .. " >/dev/null 2>&1")
    -- os.execute returns exit status, 0 means success, but may be true/false in some implementations
    assert(result == 0 or result == true, "executable should run")
end

-- Cleanup
posix.unlink(config_file)
posix.unlink(test_script)
posix.unlink(state_file)
posix.unlink(test_bin)
