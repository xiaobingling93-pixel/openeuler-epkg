-- Run posix lua function tests
-- This serves as a comprehensive test suite and documentation
-- Auto-discovers test_*.lua files in the lua/ subdirectory
-- Can run all tests or a single specific test

-- Check for specific test argument
local requested_test = arg and arg[2]  -- arg[1] is script path, arg[2] is test name

-- Strip .lua suffix if present
if requested_test then
    requested_test = requested_test:gsub("%.lua$", "")
end

print("=" .. string.rep("=", 60))
if requested_test then
    print("Running POSIX Lua Function Tests matching: " .. requested_test)
else
    print("Running POSIX Lua Function Tests")
end
print("=" .. string.rep("=", 60))

-- Get the directory where this script is located
-- First try to get from arg[1] (script name passed by rpmlua)
local script_dir
if arg and arg[1] then
    script_dir = arg[1]:match("(.*/)")
end
if not script_dir then
    -- Final fallback to current directory
    script_dir = "./"
end

-- Get lua tests directory
local lua_tests_dir = script_dir .. "lua/"

-- Auto-discover test files
local tests = {}
local entries = posix.dir(lua_tests_dir)
if entries then
    for i, entry in ipairs(entries) do
        if entry:match("^test_.*%.lua$") and entry ~= "test_all.lua" then
            -- Extract test name (without .lua extension)
            local test_name = entry:match("^test_(.*)%.lua$")
            if test_name then
                table.insert(tests, "test_" .. test_name)
            end
        end
    end
    -- Sort tests for consistent output
    table.sort(tests)
end

if #tests == 0 then
    print("ERROR: No test files found in " .. lua_tests_dir)
    error("No tests found")
end

-- Filter tests if a specific test is requested
local tests_to_run = {}
if requested_test then
    local found = false
    for _, test_name in ipairs(tests) do
        if test_name:find(requested_test, 1, true) then
            table.insert(tests_to_run, test_name)
            found = true
        end
    end
    if not found then
        print("ERROR: No tests matching pattern '" .. requested_test .. "' found. Available tests:")
        for _, test_name in ipairs(tests) do
            print("  " .. test_name:sub(6))  -- Remove "test_" prefix
        end
        error("No tests found matching pattern: " .. requested_test)
    end
else
    tests_to_run = tests
end

print("Found " .. #tests .. " test files")
if requested_test then
    print("Running " .. #tests_to_run .. " test(s) matching: " .. requested_test)
end
print("")

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

local failed = 0
local passed = 0

for _, test_name in ipairs(tests_to_run) do
    local test_file = lua_tests_dir .. test_name .. ".lua"

    local success, err = pcall(function()
        dofile(test_file)
    end)

    if success then
        print("PASS: " .. test_name)
        passed = passed + 1
    else
        print("FAIL: " .. test_name .. " - " .. tostring(err))
        failed = failed + 1
    end
end

print("\n" .. string.rep("=", 60))
print(string.format("Results: %d passed, %d failed", passed, failed))
print(string.rep("=", 60))

-- Cleanup: Remove test directory using posix functions
-- First remove all files and subdirectories
if posix.stat(test_dir) ~= nil then
    local entries = posix.dir(test_dir)
    for i, entry in ipairs(entries) do
        if entry ~= "." and entry ~= ".." then
            local path = test_dir .. "/" .. entry
            local stat = posix.stat(path)
            if stat ~= nil then
                if stat.type == "directory" then
                    posix.rmdir(path)
                else
                    posix.unlink(path)
                end
            end
        end
    end
    posix.rmdir(test_dir)
end

if failed > 0 then
    error(string.format("Tests failed: %d passed, %d failed", passed, failed))
end

