use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::Receiver;
use flate2::read::GzDecoder;
use liblzma::read::XzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use memmap2::Mmap;
use color_eyre::eyre::{Result, Context};
use log::warn;

use crate::models::repodata_indice;
use crate::models::PackageFormat;

// Search options for RPM filelists
#[derive(Clone)]
pub struct SearchOptions {
    pub ignore_case: bool,
    #[allow(dead_code)]
    pub exact_match: bool,
    #[allow(dead_code)]
    pub show_version: bool,
    #[allow(dead_code)]
    pub show_path: bool,
    pub files: bool,
    pub paths: bool,
    pub regexp: bool,
    pub pattern: String,
    pub u8_pattern: Vec<u8>,
    pub regex_pattern: Option<Arc<regex::bytes::Regex>>,
    pub format: PackageFormat,
}

// Constants for package metadata patterns
static PKGNAME_PATTERN: &[u8] = b"pkgname: ";
static SUMMARY_PATTERN: &[u8] = b"summary: ";

pub fn search_repo_cache(options: &mut SearchOptions) -> Result<()> {
    let repodata_indice = repodata_indice();
    let mut any_filelists = false;
    let mut consumer_handles = Vec::new();
    let mut producer_handles = Vec::new();

    for repo_index in repodata_indice.values() {
        let repo_dir = PathBuf::from(&repo_index.repo_dir_path);

        // Pass the package format from the repository to the search options
        options.format = repo_index.format;

        for shard in repo_index.repo_shards.values() {
            if options.files || options.paths {
                if let Some(filelists) = &shard.filelists {
                    let filelists_path = repo_dir.join(&filelists.filename);
                    if filelists_path.exists() {
                        // Start processing filelists in a new thread and collect the handles
                        let (consumer_handle, producer_handle) = search_filelists(filelists_path, options)
                            .with_context(|| format!("Failed to search filelists in {}", repo_index.repodata_name))?;
                        consumer_handles.push(consumer_handle);
                        producer_handles.push(producer_handle);
                        any_filelists = true;
                    } else {
                        warn!("Filelists not found at {}", filelists_path.display());
                    }
                }
            } else {
                let filename = shard.packages.filename.clone();
                search_packages(&repo_dir.join(&filename), options)
                    .with_context(|| format!("Failed to search package info in {}", repo_index.repodata_name))?;
            }
        }
    }

    if !any_filelists && (options.files || options.paths) {
        warn!("No filelists found in any repository");
    }

    // Wait for all producer threads to complete first
    for handle in producer_handles {
        handle.join().unwrap()?;
    }

    // Then wait for all consumer threads to complete
    for handle in consumer_handles {
        handle.join().unwrap()?;
    }

    Ok(())
}

pub fn search_filelists(filelists_path: PathBuf, options: &mut SearchOptions) -> Result<(thread::JoinHandle<Result<()>>, thread::JoinHandle<Result<()>>)> {
    // Create a bounded channel for producer-consumer communication
    // Using a bounded channel provides backpressure to prevent excessive memory usage
    // while maintaining zero-copy semantics with Arc<Mutex<FixedBuffer>>
    let (tx, rx) = crossbeam_channel::bounded::<Arc<Mutex<FixedBuffer>>>(1);

    // Create a buffer pool for this producer-consumer pair
    let buffer_pool = Arc::new(SharedBufferPool::new(BUFFER_COUNT, BUFFER_SIZE));

    // Clone options and buffer pool for the threads
    let options_arc = Arc::new(options.clone());
    let producer_buffer_pool = Arc::clone(&buffer_pool);

    // Start the producer thread with fixed buffer chunked processing
    let producer_handle = start_filelists_producer(filelists_path.clone(), tx, producer_buffer_pool);

    // Clone buffer pool for the consumer thread
    let consumer_buffer_pool = Arc::clone(&buffer_pool);

    // Determine if we're dealing with RPM XML format or simple format
    let is_rpm_xml = filelists_path.to_str().unwrap_or("").contains(".xml");

    // Start a new thread to process the chunks with appropriate processor based on format
    let consumer_handle = thread::spawn(move || {
        let options = &*options_arc;

        if is_rpm_xml {
            // Process RPM XML format
            process_rpm_filelists(rx, options, consumer_buffer_pool)
        } else {
            // Process simple format (pkgname path)
            process_simple_filelists(rx, options, consumer_buffer_pool)
        }
    });

    // Return both thread handles so they can be joined later
    Ok((consumer_handle, producer_handle))
}

// Fixed buffer that never reallocates and tracks valid data range
struct FixedBuffer {
    data: Vec<u8>,   // The underlying data buffer (fixed size)
    used: usize,     // How much of the buffer is used (valid data range)
}

impl FixedBuffer {
    // Create a new fixed buffer with pre-allocated capacity
    fn new(capacity: usize) -> Self {
        FixedBuffer {
            data: vec![0; capacity],
            used: 0,
        }
    }

    // Clear the buffer for reuse without deallocation
    fn clear(&mut self) {
        self.used = 0;
    }

    // Get a slice of the used data
    fn as_slice(&self) -> &[u8] {
        &self.data[0..self.used]
    }

    // Get mutable slice for the entire buffer
    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    // Get available space for writing
    fn available_space(&self) -> usize {
        self.data.len() - self.used
    }

    // Set the used size directly
    fn set_used(&mut self, size: usize) {
        assert!(size <= self.data.len());
        self.used = size;
    }

    // Get the used size directly
    fn nr_used(&mut self) -> usize {
        self.used
    }

    // Copy data into the buffer at a specific position without reallocation
    fn copy_from_slice(&mut self, src: &[u8], start_pos: usize) {
        let end_pos = start_pos + src.len();
        assert!(end_pos <= self.data.len(), "Buffer overflow");
        self.data[start_pos..end_pos].copy_from_slice(src);
        self.used = self.used.max(end_pos);
    }

    // Copy data into the buffer at the beginning without reallocation
    fn copy_at_start(&mut self, src: &[u8]) {
        self.copy_from_slice(src, 0);
    }
}

// Constants for buffer management
const BUFFER_SIZE: usize = 128 * 1024; // 128KB buffers
const BUFFER_COUNT: usize = 4;         // Total number of buffers in the pool (shared between producer and consumer)

// Shared buffer pool for zero-copy processing
struct SharedBufferPool {
    buffers: Vec<Arc<Mutex<FixedBuffer>>>,
    producer_idx: AtomicUsize,
    #[allow(dead_code)]
    consumer_idx: AtomicUsize,
    #[allow(dead_code)]
    buffer_count: usize,
}

impl SharedBufferPool {
    fn new(buffer_count: usize, buffer_size: usize) -> Self {
        assert!(buffer_count >= 4, "Buffer count must be at least 4");
        let mut buffers = Vec::with_capacity(buffer_count);
        for _ in 0..buffer_count {
            buffers.push(Arc::new(Mutex::new(FixedBuffer::new(buffer_size))));
        }
        SharedBufferPool {
            buffers,
            producer_idx: AtomicUsize::new(0),
            consumer_idx: AtomicUsize::new(0),
            buffer_count,
        }
    }

    // Get the current producer buffer
    fn get_producer_buffer(&self) -> Arc<Mutex<FixedBuffer>> {
        let idx = self.producer_idx.load(Ordering::SeqCst);
        Arc::clone(&self.buffers[idx % self.buffers.len()])
    }

    // Get the next producer buffer (for partial elements)
    fn get_next_producer_buffer(&self) -> Arc<Mutex<FixedBuffer>> {
        let idx = self.producer_idx.load(Ordering::SeqCst);
        Arc::clone(&self.buffers[(idx + 1) % self.buffers.len()])
    }

    // Advance the producer index
    fn advance_producer(&self) {
        let current = self.producer_idx.load(Ordering::SeqCst);
        let next = (current + 1) % self.buffers.len();

        self.producer_idx.store(next, Ordering::SeqCst);
    }
}

/*
 * ┌───────────────────────────────────────────────────────────────────────────┐
 * │                     ZERO-COPY CIRCULAR BUFFER DESIGN                      │
 * └───────────────────────────────────────────────────────────────────────────┘
 *
 * This implementation uses a circular buffer pool with advancing indices to achieve
 * true zero-copy data flow between producer and consumer threads. The design
 * eliminates unnecessary memory copies while maintaining thread safety.
 *
 * ┌─────────┬─────────┬─────────┬─────────┐
 * │ Buffer0 │ Buffer1 │ Buffer2 │ Buffer3 │
 * └─────────┴─────────┴─────────┴─────────┘
 *         ^                   ^
 *   consumer_idx            producer_idx
 *
 * Key features:
 *
 * 1. CIRCULAR BUFFER MECHANICS:
 *    - Both producer and consumer threads access all buffers in the pool
 *    - They maintain separate indices that advance through the buffer pool
 *    - consumer_idx is always 1 behind producer_idx (modulo buffer count)
 *    - Atomic operations ensure thread-safe index advancement
 *
 * 2. BUFFER ROLES BASED ON RELATIVE POSITION:
 *    - producer_idx: Current buffer being filled by producer
 *    - producer_idx+1: Next buffer for producer (for partial lines)
 *    - producer_idx-1: Pending in channel
 *    - consumer_idx: Current buffer being processed by consumer
 *
 * 3. ZERO-COPY DATA FLOW:
 *    - Producer fills a buffer and advances its index
 *    - Consumer processes the same buffer when its index reaches it
 *    - No copying between producer and consumer buffers
 *    - Minimal copying only for combining partial XML elements
 *
 * 4. SYNCHRONIZATION & BACKPRESSURE:
 *    - Producer waits if it would overwrite a buffer still in use by consumer
 *    - Consumer waits if producer hasn't filled the next buffer yet
 *    - Natural backpressure prevents memory exhaustion
 *
 * 5. MEMORY EFFICIENCY:
 *    - Fixed number of pre-allocated buffers (BUFFER_COUNT)
 *    - Fixed buffer size (BUFFER_SIZE) bounds memory usage
 *    - Reuse of buffers eliminates allocation/deallocation overhead
 *
 * This design significantly reduces memory pressure and improves throughput
 * by eliminating unnecessary copies while maintaining thread safety through
 * careful coordination of buffer access between threads.
 */

// Buffer pools are now created per producer-consumer pair instead of globally

// Helper function to find the last occurrence of a byte in a slice
fn rfind_byte(data: &[u8], byte: u8) -> Option<usize> {
    if data.is_empty() {
        return None;
    }

    // Search backwards for the byte
    for i in (0..data.len()).rev() {
        if data[i] == byte {
            return Some(i);
        }
    }

    None
}

// Start a producer thread to read and send file contents in chunks using zero-copy with Arc<Mutex<FixedBuffer>>
// Uses a dedicated buffer pool with circular buffer semantics for true zero-copy
fn start_filelists_producer(
    filelists_path: PathBuf,
    tx: crossbeam_channel::Sender<Arc<Mutex<FixedBuffer>>>,
    buffer_pool: Arc<SharedBufferPool>
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        // Track current producer buffer and its position
        let mut current_buffer = buffer_pool.get_producer_buffer();
        let mut partial_size = 0;

        // Open and prepare the file reader
        let file = File::open(&filelists_path)?;
        let mut reader: Box<dyn std::io::Read> = if filelists_path.to_string_lossy().ends_with(".gz") {
            Box::new(GzDecoder::new(file))
        } else if filelists_path.to_string_lossy().ends_with(".xz") {
            Box::new(XzDecoder::new_parallel(file))
        } else if filelists_path.to_string_lossy().ends_with(".zst") {
            Box::new(ZstdDecoder::new(file)?)
        } else {
            Box::new(file)
        };

        // Process the file in chunks
        loop {
            // Lock the current buffer
            let mut locked_buffer = current_buffer.lock().unwrap();

            // If this is a fresh buffer, clear it first
            if partial_size == 0 {
                locked_buffer.clear();
            }

            // Calculate available space in the buffer after any partial data
            let available = locked_buffer.available_space();
            if available == 0 {
                // Buffer is full, send notification to consumer and move to next buffer
                drop(locked_buffer);

                // Send notification through the channel (not the buffer itself)
                if tx.send(Arc::clone(&current_buffer)).is_err() {
                    // Channel is closed, receiver has terminated
                    return Ok(());
                }

                // Advance to the next buffer
                buffer_pool.advance_producer();
                current_buffer = buffer_pool.get_producer_buffer();
                partial_size = 0; // Start fresh with the new buffer
                continue;
            }

            // Read directly into the buffer after any partial data
            let buffer_slice = locked_buffer.as_mut_slice();
            let bytes_read = reader.read(&mut buffer_slice[partial_size..])?;

            // Update used size in the buffer
            if bytes_read > 0 {
                locked_buffer.set_used(partial_size + bytes_read);
            }

            // Check if we're done reading
            if bytes_read == 0 {
                // If we have any data in the buffer, send it
                if locked_buffer.nr_used() > 0 {
                    drop(locked_buffer);
                    if tx.send(Arc::clone(&current_buffer)).is_err() {
                        return Ok(());
                    }
                } else {
                    drop(locked_buffer);
                }
                break;
            }

            // Find a good chunk boundary (preferably at a newline)
            let data = locked_buffer.as_slice();
            if let Some(boundary) = rfind_byte(data, b'\n') {
                let boundary = boundary + 1; // Include the newline

                // Calculate size of partial data at the end (after boundary)
                let new_partial_size = if boundary < data.len() {
                    data.len() - boundary
                } else {
                    0
                };

                // If there's partial data, prepare to move it to the next buffer
                if new_partial_size > 0 {
                    // Get the next producer buffer
                    let next_buffer = buffer_pool.get_next_producer_buffer();
                    let mut next_locked = next_buffer.lock().unwrap();

                    // Clear the next buffer and copy partial data to the beginning
                    next_locked.clear();
                    next_locked.copy_at_start(&data[boundary..]);
                    next_locked.set_used(new_partial_size);

                    // Update the valid data size for the current buffer (exclude partial)
                    locked_buffer.set_used(boundary);

                    // Release the next buffer lock
                    drop(next_locked);
                }

                // Release the current buffer lock
                drop(locked_buffer);

                // Send notification through the channel (not the buffer itself)
                if tx.send(Arc::clone(&current_buffer)).is_err() {
                    // Channel is closed, receiver has terminated
                    return Ok(());
                }

                // Advance to the next buffer and update partial size
                buffer_pool.advance_producer();
                current_buffer = buffer_pool.get_producer_buffer();
                partial_size = new_partial_size;
            } else {
                // No complete boundary found, continue reading
                partial_size = locked_buffer.nr_used();
                drop(locked_buffer);

                // If the buffer is almost full and no newline was found,
                // we should send it anyway to avoid blocking
                if partial_size > BUFFER_SIZE - 1024 {
                    // Send notification through the channel (not the buffer itself)
                    if tx.send(Arc::clone(&current_buffer)).is_err() {
                        return Ok(());
                    }
                    buffer_pool.advance_producer();
                    current_buffer = buffer_pool.get_producer_buffer();
                    partial_size = 0;
                }
            }
        }

        Ok(())
    })
}

// Helper function to find line boundaries in a chunk of data
fn find_line_boundaries(data: &[u8], start_pos: usize, end_pos: usize) -> (usize, usize) {
    // Find start of line (previous newline + 1 or 0)
    let line_start = if start_pos == 0 {
        0
    } else {
        memchr::memrchr(b'\n', &data[..start_pos])
            .map(|pos| pos + 1)
            .unwrap_or(0)
    };

    // Find end of line using find_next_newline
    let line_end = find_next_newline(data, end_pos);

    (line_start, line_end)
}

// Process simple filelists with format "pkgname path" per line
// Uses fast memmem search first, then more refined matching if needed
fn process_simple_filelists(
    rx: Receiver<Arc<Mutex<FixedBuffer>>>,
    options: &SearchOptions,
    _buffer_pool: Arc<SharedBufferPool> // We don't currently use this but include for symmetry and future use
) -> Result<()> {
    // Create a memmem finder for fast substring searching
    let finder = memchr::memmem::Finder::new(&options.u8_pattern);

    // Process chunks as they arrive
    while let Ok(arc_chunk) = rx.recv() {
        // Lock the buffer to access its contents
        let mut chunk_guard = arc_chunk.lock().unwrap();
        let chunk_data = chunk_guard.as_slice();

        // Find all matches in the chunk
        for match_pos in finder.find_iter(chunk_data) {
            // For each match, find the line boundaries using find_line_boundaries
            let (line_start, line_end) = find_line_boundaries(chunk_data, match_pos, match_pos + 1);

            // Extract the full line containing the match
            let line = &chunk_data[line_start..line_end];

            // Process just this line
            process_simple_line(line, options)?;
        }

        chunk_guard.clear();
        // Release the lock
        drop(chunk_guard);
    }

    Ok(())
}

// Process a single line from a simple filelist format ("pkgname path" or "path pkgname/section" on Deb)
fn process_simple_line(
    line: &[u8],
    options: &SearchOptions
) -> Result<()> {
    // Split the line into pkgname and path
    if let Some(space_pos) = memchr::memchr(b' ', line) {
        // Handle different package formats
        let (pkgname, path) = match options.format {
            // For Deb format, the order is "path pkgname/section"
            PackageFormat::Deb => {
                let path = &line[..space_pos];
                // Strip leading spaces from pkgname
                let mut pkgname_start = space_pos + 1;
                while pkgname_start < line.len() && line[pkgname_start] == b' ' {
                    pkgname_start += 1;
                }
                let pkgname = &line[pkgname_start..];
                (pkgname, path)
            },
            // For all other formats, the order is "pkgname path"
            _ => {
                let pkgname = &line[..space_pos];
                let path = &line[space_pos + 1..];
                (pkgname, path)
            }
        };

        // PATH MATCHING REQUIREMENTS:
        //
        // 1. DISTRO FILELIST FORMATS:
        //    - Debian/Arch: relative paths (usr/bin/ls, usr/lib/libc.so)
        //    - RPM: absolute paths (/usr/bin/ls, /usr/lib/libc.so)
        //    - User shall not care differences among distros
        //
        // 2. USER PATTERN EXPECTATIONS:
        //    - /usr/bin/ls should match usr/bin/ls (Debian/Arch) AND /usr/bin/ls (RPM)
        //    - usr/bin/ls should match usr/bin/ls (Debian/Arch) AND /usr/bin/ls (RPM)
        //    - /bin/ls should match usr/bin/ls (Debian/Arch) AND /bin/ls (RPM)
        //    - bin/ls should match usr/bin/ls (Debian/Arch) AND /bin/ls (RPM)
        //
        // 3. PATH SEPARATOR SEMANTICS:
        //    - Leading / provides clear path boundary: /bin/ip won't match sbin/ip
        //    - Without leading /, bin/ip could match sbin/ip (unexpected!)
        //    - Users expect copy/pasted '/usr/bin/ls' to just work w/o hand removing leading /
        //
        // 4. MATCHING STRATEGY:
        //    - For user patterns starting with /: normalize filelist path to absolute
        //    - For user patterns NOT starting with /: try both relative and absolute
        //    - This ensures /usr/bin/ls works everywhere, while bin/ls is flexible
        //
        // 5. EXAMPLES:
        //    User pattern: /usr/bin/ls
        //    - Debian filelist: usr/bin/ls -> normalize to /usr/bin/ls -> MATCH
        //    - RPM filelist: /usr/bin/ls -> use as-is -> MATCH
        //
        //    User pattern: usr/bin/ls
        //    - Debian filelist: usr/bin/ls -> try as-is -> MATCH
        //    - RPM filelist: /usr/bin/ls -> try as-is -> NO MATCH, try relative -> MATCH
        //
        //    User pattern: bin/ls
        //    - Debian filelist: usr/bin/ls -> try as-is -> NO MATCH, try relative -> MATCH
        //    - RPM filelist: /usr/bin/ls -> try as-is -> NO MATCH, try relative -> MATCH
        //
        // 6. CAVEATS:
        //    - Must handle both relative and absolute filelist paths
        //    - Must preserve user's intent for path boundaries
        //    - Must work with copy/paste from system commands
        //    - Must not create false matches (sbin/ip when searching /bin/ip)

        // Both Debian/Archlinux strip leading '/' from path, so add it back before comparing
        let mut abs_path_vec = Vec::with_capacity(1 + path.len());
        abs_path_vec.push(b'/');
        abs_path_vec.extend_from_slice(path);
        let indeed_match = check_match_path(&abs_path_vec, options);
        if indeed_match {
            print_path(pkgname, &abs_path_vec, options);
        }
    }

    Ok(())
}

fn print_path(pkgname: &[u8], path: &[u8], _options: &SearchOptions) {
    if let (Ok(pkg_str), Ok(path_str)) = (std::str::from_utf8(pkgname), std::str::from_utf8(path)) {
        println!("{} {}", pkg_str, path_str);
    }
}

// Helper function to check if a path matches the pattern according to options
fn check_match_path(path: &[u8], options: &SearchOptions) -> bool {
    if options.files {
        // For --files, check if the filename matches
        if let Some(mut fname_pos) = memchr::memrchr(b'/', path) {
            if !options.u8_pattern.is_empty() && options.u8_pattern[0] != b'/' {
                fname_pos += 1;
            } else {
                // pattern="/bash" => filename starts with "bash"
            }
            let filename = &path[fname_pos..];
            match_pattern(filename, options)
        } else {
            match_pattern(path, options)
        }
    } else if options.paths {
        // For --paths, check if the path matches
        match_pattern(path, options)
    } else {
        // Default case, check if the path contains the pattern
        memchr::memmem::Finder::new(&options.u8_pattern).find(path).is_some()
    }
}

// Helper function to match pattern against content based on options
fn match_pattern(content: &[u8], options: &SearchOptions) -> bool {
    if options.regexp {
        // Use regex for matching if available
        if let Some(regex) = &options.regex_pattern {
            return regex.is_match(content);
        }
    }

    // Fall back to simple substring search
    if !options.ignore_case {
        memchr::memmem::Finder::new(&options.u8_pattern).find(content).is_some()
    } else {
        // Case-insensitive search
        let content_lower = content.to_ascii_lowercase();
        memchr::memmem::Finder::new(&options.u8_pattern).find(&content_lower).is_some()
    }
}

// Process RPM filelists using memmem pattern matching with chunked data
fn process_rpm_filelists(
    rx: Receiver<Arc<Mutex<FixedBuffer>>>,
    options: &SearchOptions,
    _buffer_pool: Arc<SharedBufferPool> // We don't currently use this but include for symmetry and future use
) -> Result<()> {
    let mut current_pkgname = Vec::<u8>::new();

    let finder = memchr::memmem::Finder::new(&options.u8_pattern);

    // Use the buffer pool's consumer buffer instead of directly receiving from channel
    while let Ok(arc_chunk) = rx.recv() {
        // Lock the buffer to access its contents
        let mut chunk_guard = arc_chunk.lock().unwrap();

        // Process the chunk directly - we use a scoped block to ensure the lock is released quickly
        {
            let chunk_data = chunk_guard.as_slice();
            process_rpm_filelists_with_memmem(&mut current_pkgname, chunk_data, &finder, options)?;
        }

        chunk_guard.clear();
        // Release the lock
        drop(chunk_guard);
	}

    Ok(())
}

/* Example input:
<package pkgid="e01a85beb0abfbb377f060882d281d3052e0cbadf77d67c9ff1d4533c42f0d17" name="CUnit" arch="x86_64">
  <version epoch="0" ver="2.1.3" rel="24.oe2403"/>
  <file>/etc/ima/digest_lists.tlv/0-metadata_list-compact_tlv-CUnit-2.1.3-24.oe2403.x86_64</file>
  <file>/etc/ima/digest_lists/0-metadata_list-compact-CUnit-2.1.3-24.oe2403.x86_64</file>
  <file>/usr/lib64/libcunit.so.1</file>
  <file>/usr/lib64/libcunit.so.1.0.1</file>
  <file type="dir">/usr/share/CUnit</file>
  <file>/usr/share/CUnit/CUnit-List.dtd</file>
  <file>/usr/share/CUnit/CUnit-List.xsl</file>
*/
fn process_rpm_filelists_with_memmem(current_pkgname: &mut Vec<u8>, chunk_data: &[u8], finder: &memchr::memmem::Finder, options: &SearchOptions) -> Result<()> {
    for match_pos in finder.find_iter(chunk_data) {
        // For each match, find the line boundaries using find_line_boundaries
        let (line_start, line_end) = find_line_boundaries(chunk_data, match_pos, match_pos + 1);

        // Extract the full line containing the match
        let line = &chunk_data[line_start..line_end];

        // Process the file line directly here instead of calling process_rpm_file_line
        let file_path = if let Some(rest) = line.strip_prefix(b"  <file>") {
                            if let Some(rest) = rest.strip_suffix(b"</file>") {
                                rest
                            } else {
                                line
                            }
                        } else if line.starts_with(b"  <file type=\"") {
                            // Handle any <file type="..."> pattern
                            if let Some(rest) = line.strip_suffix(b"</file>") {
                                // Find the closing quote of the type attribute
                                if let Some(quote_pos) = memchr::memchr(b'"', &line[14..]) {
                                    // Skip past the type attribute and the closing >
                                    let content_start = 14 + quote_pos + 2; // 14 for "  <file type=\"", quote_pos for the type value, +2 for " and >
                                    if content_start < rest.len() {
                                        &rest[content_start..]
                                    } else {
                                        line
                                    }
                                } else {
                                    line
                                }
                            } else {
                                line
                            }
                        } else {
                            line
                        };

        // If we have a match, print the result
        let indeed_match = check_match_path(file_path, options);
        if indeed_match {
            // Update the package name if we can find it in the chunk
            if let Some(pkg_name) = rfind_pkgname_in_xml(&chunk_data[0..line_start]) {
                *current_pkgname = pkg_name.into();
            }
            // Use the current package name for output
            print_path(current_pkgname, file_path, options);
        }
    }

    // Record last pkgname before leaving this chunk
    // This adds ~10KB/128KB=8% backscan cost, however can handle package with huge filelist.
    if let Some(pkg_name) = rfind_pkgname_in_xml(chunk_data) {
        *current_pkgname = pkg_name.into();
    }

    Ok(())
}

fn rfind_pkgname_in_xml(data: &[u8]) -> Option<String> {
    // Constants for XML tags and attributes
    static NAME_STR: &[u8] = b"name";
    static QUOTE: u8 = b'\"';
    static EQUAL_SIGN: u8 = b'=';

    // Start from the end of the data and search backwards
    // We're looking for the last occurrence of name="..."
    let mut pos = data.len();

    // Keep searching backwards for equals signs until we find one that's part of name="
    while pos > 0 {
        // Find the last equals sign before the current position
        if let Some(eq_pos) = memchr::memrchr(EQUAL_SIGN, &data[..pos]) {
            // Check if this equals sign is part of name="
            if eq_pos >= NAME_STR.len() &&
               &data[eq_pos - NAME_STR.len()..eq_pos] == NAME_STR &&
               eq_pos + 1 < data.len() &&
               data[eq_pos + 1] == QUOTE {

                // We found name=", now extract the value
                let name_start = eq_pos + 2; // Skip the = and "

                // Find the closing quote
                if let Some(quote_pos) = memchr::memchr(QUOTE, &data[name_start..]) {
                    // Extract the name between quotes
                    if let Ok(name) = std::str::from_utf8(&data[name_start..(name_start + quote_pos)]) {
                        return Some(name.to_string());
                    }
                }
            }

            // Move position back to continue searching
            pos = eq_pos;
        } else {
            // No more equals signs found
            break;
        }
    }

    None
}

// Common structure to hold the state during search operations
#[allow(dead_code)]
struct PackagesSearchState<'a> {
    current_pkgname: &'a [u8],
    current_summary: &'a [u8],
    stdout: BufWriter<std::io::Stdout>,
}

#[allow(dead_code)]
impl<'a> PackagesSearchState<'a> {
    fn new() -> Self {
        PackagesSearchState {
            current_pkgname: &b""[..],
            current_summary: &b""[..],
            stdout: BufWriter::new(std::io::stdout()),
        }
    }

    fn print_match(&mut self) -> Result<()> {
        writeln!(
            self.stdout,
            "{} - {}",
            String::from_utf8_lossy(self.current_pkgname),
            String::from_utf8_lossy(self.current_summary)
        )?;
        Ok(())
    }
}

// Helper function to extract the longest literal string from a regex pattern if possible
pub fn extract_literal_string(pattern: &str) -> Option<String> {
    // Special regex characters that break literal sequences
    let special_chars = ['.', '*', '+', '?', '|', '^', '$', '\\'];

    // Track nesting level of parentheses and brackets
    let mut paren_level = 0;
    let mut bracket_level = 0;
    let mut brace_level = 0;

    // Track the current and longest literal sequences
    let mut current_literal = String::new();
    let mut longest_literal = String::new();

    // Process each character in the pattern
    for c in pattern.chars() {
        match c {
            '(' => {
                paren_level += 1;
                if paren_level == 1 && !current_literal.is_empty() {
                    // Save the current literal if it's longer than what we have
                    if current_literal.len() > longest_literal.len() {
                        longest_literal = current_literal.clone();
                    }
                    current_literal.clear();
                }
            },
            ')' => {
                if paren_level > 0 {
                    paren_level -= 1;
                }
            },
            '[' => {
                bracket_level += 1;
                if bracket_level == 1 && !current_literal.is_empty() {
                    // Save the current literal if it's longer than what we have
                    if current_literal.len() > longest_literal.len() {
                        longest_literal = current_literal.clone();
                    }
                    current_literal.clear();
                }
            },
            ']' => {
                if bracket_level > 0 {
                    bracket_level -= 1;
                }
            },
            '{' => {
                brace_level += 1;
                if brace_level == 1 && !current_literal.is_empty() {
                    // Save the current literal if it's longer than what we have
                    if current_literal.len() > longest_literal.len() {
                        longest_literal = current_literal.clone();
                    }
                    current_literal.clear();
                }
            },
            '}' => {
                if brace_level > 0 {
                    brace_level -= 1;
                }
            },
            // If we're at the top level (not in any parentheses or brackets)
            _ if paren_level == 0 && bracket_level == 0 && brace_level == 0 => {
                if special_chars.contains(&c) {
                    // We hit a special character, end the current literal
                    if !current_literal.is_empty() {
                        if current_literal.len() > longest_literal.len() {
                            longest_literal = current_literal.clone();
                        }
                        current_literal.clear();
                    }
                } else {
                    // Add to the current literal sequence
                    current_literal.push(c);
                }
            },
            _ => {}
        }
    }

    // Check if the final literal sequence is the longest
    if !current_literal.is_empty() && current_literal.len() > longest_literal.len() {
        longest_literal = current_literal;
    }

    // Return the longest literal if we found one
    if longest_literal.is_empty() {
        None
    } else {
        Some(longest_literal)
    }
}

// Check if a position is at the start of a line
fn is_line_start(data: &[u8], pos: usize, _pattern: &[u8]) -> bool {
    pos == 0 || data[pos - 1] == b'\n'
}

// Find and extract a pattern value from a line
fn find_and_extract_pattern(data: &[u8], search_end: usize, pattern: &[u8], search_backwards: bool) -> Option<(Vec<u8>, usize)> {
    let pos = if search_backwards {
        memchr::memmem::rfind(&data[..search_end], pattern)
    } else {
        memchr::memmem::find(&data[search_end..], pattern).map(|p| search_end + p)
    };

    if let Some(pos) = pos {
        // Check if it's at the start of a line
        if is_line_start(data, pos, pattern) {
            let value_start = pos + pattern.len();
            let value_end = find_next_newline(data, value_start);
            let mut value = Vec::new();
            value.extend_from_slice(&data[value_start..value_end]);
            return Some((value, pos));
        }
        return Some((Vec::new(), pos)); // Found pattern but not at line start
    }
    None
}

// Search for package name and summary in a chunk
fn search_package_metadata(
    chunk: &[u8],
    search_end: usize,
    current_pkgname: &mut Vec<u8>,
    current_summary: &mut Vec<u8>,
) -> (bool, bool) {
    let mut found_pkgname = false;
    let mut found_summary = false;

    // First look for pkgname (searching backwards)
    if let Some((pkg_value, pkg_pos)) = find_and_extract_pattern(chunk, search_end, PKGNAME_PATTERN, true) {
        if !pkg_value.is_empty() {
            current_pkgname.clear();
            current_pkgname.extend_from_slice(&pkg_value);
            found_pkgname = true;

            // Now search forward from the pkgname line for the summary
            if let Some((sum_value, _)) = find_and_extract_pattern(chunk, pkg_pos, SUMMARY_PATTERN, false) {
                if !sum_value.is_empty() {
                    current_summary.clear();
                    current_summary.extend_from_slice(&sum_value);
                    found_summary = true;
                }
            }
        }
    }

    (found_pkgname, found_summary)
}

pub fn search_packages(packages_path: &Path, options: &mut SearchOptions) -> Result<()> {
    // Memory map the file
    let file = File::open(packages_path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    let user_pattern = options.pattern.as_bytes();
    let mut stdout = BufWriter::new(std::io::stdout());

    // Keep track of the last seen package name and summary
    let mut current_pkgname = Vec::new();
    let mut current_summary = Vec::new();

    // Start position for our search
    let mut pos = 0;

    // Process the entire mmap
    while pos < mmap.len() {
        // First search for our pattern
        if let Some(pattern_pos) = memchr::memmem::find(&mmap[pos..], user_pattern) {
            let pattern_pos = pos + pattern_pos;

            // Find the line containing this pattern
            let (line_start, line_end) = find_line_boundaries(&mmap, pattern_pos, pattern_pos + 1);
            let line = &mmap[line_start..line_end];

            let regex_matches = match_pattern(line, options);
            if regex_matches {
                // We found a match, now search backward for the most recent pkgname and summary
                let (found_pkgname, found_summary) = search_package_metadata(
                    &mmap,
                    line_start,
                    &mut current_pkgname,
                    &mut current_summary
                );

                // Print the match if we found both pkgname and summary
                if found_pkgname && found_summary {
                    writeln!(
                        stdout,
                        "{} - {}",
                        String::from_utf8_lossy(&current_pkgname),
                        String::from_utf8_lossy(&current_summary)
                    )?;
                }
            }

            // Move position past this match
            pos = line_end + 1;
        } else {
            // No more matches in the file
            break;
        }
    }

    Ok(())
}

// Helper function to find the next newline character
#[inline]
fn find_next_newline(data: &[u8], start: usize) -> usize {
    memchr::memchr(b'\n', &data[start..])
        .map(|pos| start + pos)
        .unwrap_or(data.len())
}
