-- Test io.open(), file:write(), file:close() operations
-- Tests file I/O operations as used in real-world RPM scripts

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Basic file write operations
local test_file = test_dir .. "/io_test"
local file = io.open(test_file, "w")
assert(file ~= nil, "io.open should succeed for write mode")
file:write("test content\n")
file:write("second line\n")
file:close()

-- Verify file was created and has content
local stat = posix.stat(test_file)
assert(stat ~= nil, "file should exist after write")
assert(stat.type == "regular", "should be a regular file")
assert(stat.size > 0, "file should have content")

-- Test 2: Read back the file
local file = io.open(test_file, "r")
assert(file ~= nil, "io.open should succeed for read mode")
local content = file:read("*all")
file:close()
assert(content ~= nil, "file content should be readable")
assert(content:match("test content"), "file should contain written content")

-- Test 3: Real-world pattern: create script file in temp directory
local temp_path = test_dir .. "/pretrans_script"
local script_content = "#!/bin/sh\necho 'pretrans script'\n"
local file = io.open(temp_path, "w")
if file then
    file:write(script_content)
    file:close()
end

-- Verify script was created
local stat = posix.stat(temp_path)
assert(stat ~= nil, "script file should exist")
assert(stat.size > 0, "script file should have content")

-- Test 4: Append mode
local append_file = test_dir .. "/append_test"
local file1 = io.open(append_file, "w")
file1:write("first line\n")
file1:close()

local file2 = io.open(append_file, "a")
assert(file2 ~= nil, "io.open should succeed for append mode")
file2:write("second line\n")
file2:close()

-- Verify both lines are present
local file3 = io.open(append_file, "r")
local content = file3:read("*all")
file3:close()
assert(content:match("first line"), "should contain first line")
assert(content:match("second line"), "should contain second line")

-- Test 5: Real-world pattern: append+ mode with read and conditional write
local shells_file = test_dir .. "/shells"
local nl = '\n'
local sh = '/bin/sh'..nl
local bash = '/bin/bash'..nl
local f = io.open(shells_file, "a+")
if f then
    local shells = nl..f:read('*all')..nl
    if not shells:find(nl..sh) then f:write(sh) end
    if not shells:find(nl..bash) then f:write(bash) end
    f:close()
end

-- Verify shells were written
local f2 = io.open(shells_file, "r")
if f2 then
    local content = f2:read("*all")
    f2:close()
    assert(content:find("/bin/sh"), "should contain /bin/sh")
    assert(content:find("/bin/bash"), "should contain /bin/bash")
end

-- Test 6: Real-world pattern: read single line from file
local proc_file = test_dir .. "/proc_test"
local f = io.open(proc_file, "w")
if f then
    f:write("1\n")
    f:close()
end

local cf = io.open(proc_file, "r")
if cf then
    local value = cf:read()
    assert(value == "1", "should read single line")
    cf:close()
end

-- Test 5: Error handling - try to open non-existent directory
local success, err = pcall(function()
    local file = io.open("/nonexistent/dir/file", "w")
    if file then
        file:close()
    end
end)
-- This may or may not error depending on implementation
-- Just verify pcall works

-- Cleanup
posix.unlink(test_file)
posix.unlink(temp_path)
posix.unlink(append_file)
posix.unlink(shells_file)
posix.unlink(proc_file)
