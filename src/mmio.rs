use std::fs::File;
use std::ops::Range;
use memmap2::Mmap;

#[derive(Debug)]
pub struct FileMapper {
    file: File,
    mmap: Mmap,
}

impl FileMapper {
    pub fn new(file_path: &str) -> std::io::Result<Self> {
        let file = File::open(file_path)?;
        // Memory map the file (unsafe because we must ensure the file isn't modified externally)
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Self { file, mmap })
    }

    /// Get the entire mapped data
    pub fn data(&self) -> &[u8] {
        &self.mmap
    }

    /// Get a specific range of the mapped data
    /// Panics if range is out of bounds
    pub fn range(&self, range: Range<usize>) -> &[u8] {
        &self.mmap[range]
    }

    /// Safe range access with bounds checking
    pub fn checked_range(&self, range: Range<usize>) -> Option<&[u8]> {
        if range.end <= self.mmap.len() {
            Some(&self.mmap[range])
        } else {
            None
        }
    }
}

// // Example usage
// fn main() -> std::io::Result<()> {
//     let mapper = FileMapper::new("example.txt")?;
//
//     // Access first 100 bytes
//     if let Some(data) = mapper.checked_range(0..100) {
//         println!("First 100 bytes: {:?}", data);
//     }
//
//     // Process the entire file in chunks
//     let chunk_size = 4096;
//     for chunk in mapper.data().chunks(chunk_size) {
//         // Process each chunk
//         println!("Chunk length: {}", chunk.len());
//     }
//
//     Ok(())
// }
