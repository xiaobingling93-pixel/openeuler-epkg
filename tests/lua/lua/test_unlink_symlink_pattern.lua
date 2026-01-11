-- Test posix.unlink() followed by posix.symlink() pattern
-- Tests replacing file with symlink as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Real-world pattern: unlink existing file, then create symlink
local target_file = test_dir .. "/target_file"
local config_file = test_dir .. "/config_file"
os.execute("echo 'old content' > " .. config_file)

-- Remove existing file and create symlink
posix.unlink(config_file)
posix.symlink(target_file, config_file)

-- Verify symlink was created
local stat = posix.stat(config_file)
assert(stat ~= nil, "symlink should exist")
assert(stat.type == "link", "should be a symlink")

-- Test 2: Real-world pattern: check if file exists, unlink, then symlink
local backend_config = test_dir .. "/backend.config"
local policy_path = test_dir .. "/policy"
os.execute("mkdir -p " .. policy_path)
os.execute("touch " .. backend_config)

-- Check if config exists, remove it, create symlink
if posix.stat(backend_config) ~= nil then
    posix.unlink(backend_config)
end
posix.symlink(policy_path .. "/backend.config", backend_config)

-- Verify symlink
local stat2 = posix.stat(backend_config)
assert(stat2 ~= nil, "backend symlink should exist")
assert(stat2.type == "link", "should be a symlink")

-- Test 3: Real-world pattern: unlink and symlink in loop
local configs = {
    {name = "config1", target = "target1"},
    {name = "config2", target = "target2"}
}

for i, cfg in ipairs(configs) do
    local cfgfn = test_dir .. "/" .. cfg.name .. ".config"
    local target_path = test_dir .. "/" .. cfg.target

    -- Create target
    os.execute("touch " .. target_path)

    -- Remove existing config if present
    if posix.stat(cfgfn) ~= nil then
        posix.unlink(cfgfn)
    end

    -- Create symlink
    posix.symlink(target_path, cfgfn)

    -- Verify
    local stat = posix.stat(cfgfn)
    assert(stat ~= nil, "config symlink should exist")
    assert(stat.type == "link", "should be a symlink")
end

-- Cleanup
posix.unlink(config_file)
posix.unlink(backend_config)
for i, cfg in ipairs(configs) do
    posix.unlink(test_dir .. "/" .. cfg.name .. ".config")
    posix.unlink(test_dir .. "/" .. cfg.target)
end
