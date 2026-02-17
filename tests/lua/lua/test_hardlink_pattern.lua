-- Test hard link checking and restoration pattern
-- Tests checking ino/dev to verify hard links and restoring them

local test_dir = "/tmp/epkg_posix_test"
os.execute("mkdir -p " .. test_dir)

-- Test 1: Real-world pattern: check if files are same hard link using ino/dev
local real_file = test_dir .. "/real_file"
local archive_file = test_dir .. "/archive_file"
os.execute("echo 'content' > " .. real_file)

-- Create hard link
posix.link(real_file, archive_file)

-- Verify they are same hard link
local stat_real = posix.stat(real_file)
local stat_archive = posix.stat(archive_file)
assert(stat_real.ino == stat_archive.ino, "should have same inode")
assert(stat_real.dev == stat_archive.dev, "should have same device")

-- Test 2: Real-world pattern: check if hard link was removed, restore it
local source_file = test_dir .. "/source"
local link_file = test_dir .. "/link_file"
os.execute("echo 'source content' > " .. source_file)

-- Create initial hard link
posix.link(source_file, link_file)

-- Remove the link
posix.unlink(link_file)

-- Check if link was removed and restore
local stat_source = posix.stat(source_file)
local stat_link = posix.stat(link_file)
if stat_link == nil then
    -- Link was removed, restore it
    posix.link(source_file, link_file)
end

-- Verify link was restored
local stat_link2 = posix.stat(link_file)
assert(stat_link2 ~= nil, "link should be restored")
assert(stat_link2.ino == stat_source.ino, "should have same inode")

-- Test 3: Real-world pattern: check if hard link is broken (different ino/dev)
local file1 = test_dir .. "/file1"
local file2 = test_dir .. "/file2"
-- Clean up if exists
os.execute("rm -f " .. file1 .. " " .. file2)
os.execute("echo 'content1' > " .. file1)
os.execute("echo 'content2' > " .. file2)

-- Get initial inodes
local stat1_orig = posix.stat(file1)
local stat2_orig = posix.stat(file2)

-- Remove file2 and create hard link from file1
posix.unlink(file2)
posix.link(file1, file2)

-- Verify they are same hard link
local stat1 = posix.stat(file1)
local stat2 = posix.stat(file2)
assert(stat1.ino == stat2.ino, "should have same inode")
assert(stat1.dev == stat2.dev, "should have same device")

-- Remove file2 and create new file with same name (breaks link)
posix.unlink(file2)
-- Create new file - this should get a new inode
local f = io.open(file2, "w")
if f then
    f:write("new content\n")
    f:close()
end

-- Check if hard link is broken
local stat1_new = posix.stat(file1)
local stat2_new = posix.stat(file2)
-- New file should have different inode (unless filesystem reuses it very quickly)
if stat2_new and (stat1_new.ino ~= stat2_new.ino or stat1_new.dev ~= stat2_new.dev) then
    -- Hard link is broken, restore it
    posix.unlink(file2)
    posix.link(file1, file2)
    -- Verify link was restored
    local stat2_final = posix.stat(file2)
    assert(stat2_final ~= nil, "file2 should exist after restore")
    assert(stat2_final.ino == stat1_new.ino, "should have same inode after restore")
else
    -- If inodes are same (unlikely but possible), just verify they're linked
    assert(stat2_new ~= nil, "file2 should exist")
end

-- Test 4: Real-world pattern: remove .rpmsave file if it exists
local save_file = test_dir .. "/file.rpmsave"
os.execute("echo 'save content' > " .. save_file)

if posix.access(save_file) then
    posix.unlink(save_file)
end

-- Verify save file was removed
local stat_save = posix.stat(save_file)
assert(stat_save == nil, "save file should be removed")

-- Cleanup
posix.unlink(real_file)
posix.unlink(archive_file)
posix.unlink(source_file)
posix.unlink(link_file)
posix.unlink(file1)
posix.unlink(file2)
