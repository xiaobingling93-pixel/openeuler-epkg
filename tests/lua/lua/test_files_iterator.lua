-- Test posix.files() iterator with string manipulation
-- Tests posix.files() iterator pattern as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Real-world pattern: iterate with posix.files() and process files
local policy_dir = test_dir .. "/policy"
os.execute("mkdir -p " .. policy_dir)
os.execute("touch " .. policy_dir .. "/backend1.config")
os.execute("touch " .. policy_dir .. "/backend2.config")

local config_dir = test_dir .. "/config"
os.execute("mkdir -p " .. config_dir)

-- Real-world pattern: iterate over files and create symlinks
local files_iter = posix.files(policy_dir)
if files_iter then
    for fn in files_iter do
        if fn ~= "." and fn ~= ".." then
            -- Extract backend name using string manipulation
            local backend = fn:gsub(".*/", ""):gsub("%.config", "")
            local cfgfn = config_dir .. "/" .. backend .. ".config"
            -- In real script: posix.unlink(cfgfn) then posix.symlink(...)
            -- For test, just verify we can process the files
            assert(backend ~= nil, "backend should be extracted")
        end
    end
end

-- Test 2: Real-world pattern: iterate and filter files
local source_dir = test_dir .. "/source"
-- Clean up if exists from previous test
os.execute("rm -rf " .. source_dir)
os.execute("mkdir -p " .. source_dir)
os.execute("touch " .. source_dir .. "/file1.txt")
os.execute("touch " .. source_dir .. "/file2.txt")
os.execute("touch " .. source_dir .. "/file3.dat")

local txt_count = 0
local files_iter2 = posix.files(source_dir)
if files_iter2 then
    for fn in files_iter2 do
        if fn ~= "." and fn ~= ".." then
            if fn:match("%.txt$") then
                txt_count = txt_count + 1
            end
        end
    end
end
assert(txt_count == 2, "should find 2 .txt files")

-- Test 3: Real-world pattern: string manipulation with gsub
local test_path = "/some/path/to/file.config"
local backend = test_path:gsub(".*/", ""):gsub("%.config", "")
assert(backend == "file", "should extract 'file' from path")

-- Test 4: Real-world pattern: iterate and process with path building
local data_dir = test_dir .. "/data"
os.execute("mkdir -p " .. data_dir)
os.execute("touch " .. data_dir .. "/item1")
os.execute("touch " .. data_dir .. "/item2")

local processed = 0
local files_iter3 = posix.files(data_dir)
if files_iter3 then
    for fn in files_iter3 do
        if fn ~= "." and fn ~= ".." then
            local full_path = data_dir .. "/" .. fn
            local stat = posix.stat(full_path)
            if stat and stat.type == "regular" then
                processed = processed + 1
            end
        end
    end
end
assert(processed == 2, "should process 2 files")

-- Cleanup
os.execute("rm -rf " .. policy_dir)
os.execute("rm -rf " .. config_dir)
os.execute("rm -rf " .. source_dir)
os.execute("rm -rf " .. data_dir)
