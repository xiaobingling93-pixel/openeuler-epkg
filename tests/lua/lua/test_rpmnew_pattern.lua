-- Test os.remove() with .rpmnew pattern
-- Tests removing .rpmnew files as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Real-world pattern: remove .rpmnew files in loop
local rpmnew_files = {
    test_dir .. "/passwd.rpmnew",
    test_dir .. "/shadow.rpmnew",
    test_dir .. "/group.rpmnew",
    test_dir .. "/gshadow.rpmnew"
}

-- Create .rpmnew files
for i, path in ipairs(rpmnew_files) do
    os.execute("echo 'content' > " .. path)
end

-- Remove .rpmnew files
for i, name in ipairs({"passwd", "shadow", "group", "gshadow"}) do
    os.remove(test_dir .. "/" .. name .. ".rpmnew")
end

-- Verify files were removed
for i, path in ipairs(rpmnew_files) do
    local stat = posix.stat(path)
    assert(stat == nil, ".rpmnew file should be removed")
end

-- Test 2: Real-world pattern: check and remove .rpmnew if exists
local config_rpmnew = test_dir .. "/config.rpmnew"
os.execute("echo 'config' > " .. config_rpmnew)

local st = posix.stat(config_rpmnew)
if st and st.type == "regular" then
    os.remove(config_rpmnew)
end

-- Verify file was removed
local stat = posix.stat(config_rpmnew)
assert(stat == nil, "config.rpmnew should be removed")

-- Test 3: Real-world pattern: remove .rpmsave files
local save_files = {
    test_dir .. "/file1.rpmsave",
    test_dir .. "/file2.rpmsave"
}

-- Create .rpmsave files
for i, path in ipairs(save_files) do
    os.execute("echo 'save' > " .. path)
end

-- Remove .rpmsave files
for i, path in ipairs(save_files) do
    if posix.access(path) then
        posix.unlink(path)
    end
end

-- Verify files were removed
for i, path in ipairs(save_files) do
    local stat = posix.stat(path)
    assert(stat == nil, ".rpmsave file should be removed")
end
