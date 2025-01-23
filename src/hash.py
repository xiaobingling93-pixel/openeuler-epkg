#!/usr/bin/python3
import os
import sys
import hashlib
import base64
from pathlib import Path

def b32_hash(content: str) -> str:
    """
    Computes a base32-encoded hash of the input string using SHA256 and XOR folding.

    This function performs the following steps:
    1. Compute hash: SHA256 hash of the input string. (Output: 32-byte bytes object)
    2. Compress hash: XOR folding to reduce to 20 bytes. (Output: 20-byte bytearray)
    3. Encode base32: Base32 encoding of the compressed hash. (Output: 32-character string)
    4. Convert lowercase: Lowercase conversion of the base32 string. (Output: 32-character lowercase string)

    Args:
        content (str): The input string to hash.

    Returns:
        str: A base32-encoded, lowercase string representing the compressed hash.
    """
    # Step 1: Compute SHA256 hash
    # - The hashlib.sha256() function computes the SHA256 hash of the input string.
    # - The .encode() method converts the string to bytes (required by hashlib).
    # - The .digest() method returns the hash as a bytes object (32 bytes long).
    sha256_hash = hashlib.sha256(content.encode()).digest()

    # Step 2: Compress hash
    # - A bytearray of size 20 is created to store the compressed hash.
    # - Each byte of the first 20 bytes of the SHA256 hash is XORed with the corresponding
    #   byte of the next 12 bytes. This ensures all bits contribute to the final hash.
    # - The result is a 20-byte compressed hash.
    compressed_hash = bytearray(20)
    for i in range(20):
        compressed_hash[i] = sha256_hash[i] ^ sha256_hash[i + 12]

    # Step 3: Encode base32
    # - The base64.b32encode() function encodes the compressed hash into a base32 string.
    # - The .decode() method converts the bytes object to a string.
    b32sum = base64.b32encode(compressed_hash).decode()

    # Step 4: Convert lowercase
    # - The base32 string is converted to lowercase using .lower().
    # - This ensures consistency in the output, as base32 encoding can produce uppercase
    #   letters by default.
    return b32sum.lower()

def epkg_store_hash(epkg_path: str) -> str:
    """Compute the hash of the contents of a directory."""
    dir_path = Path(epkg_path)
    paths = []

    # Collect all paths under $dir/fs/ or $dir/info/install/
    dir_path = Path(dir_path)
    fs_path = str(dir_path / "fs")
    install_path = str(dir_path / "info" / "install")

    for root, dirs, files in os.walk(dir_path):
        for name in files + dirs:
            path = str(Path(root) / name)
            if (path.startswith(fs_path) or path.startswith(install_path)):
                paths.append(Path(path))

    paths.sort()

    info = []

    for path in paths:
        ftype, fdata = get_path_info(path)
        info.append(str(path.relative_to(dir_path)))
        info.append(ftype)
        info.append(fdata)

    all_info = "\n".join(info)
    #  print(all_info)

    return b32_hash(all_info)

def get_path_info(path: Path):
    """Get the type, size, and content hash of a file or directory."""
    stat = os.lstat(path)

    if stat.st_mode & 0o170000 == 0o120000:  # Symlink
        ftype = "S_IFLNK"
        fdata = os.readlink(path)
    elif stat.st_mode & 0o170000 == 0o100000:  # Regular file
        ftype = "S_IFREG"
        fdata = " ".join(file_sha256_chunks(path, stat))
    elif stat.st_mode & 0o170000 == 0o060000:  # Block device
        ftype = "S_IFBLK"
        fdata = str(stat.st_dev)
    elif stat.st_mode & 0o170000 == 0o020000:  # Character device
        ftype = "S_IFCHR"
        fdata = str(stat.st_dev)
    elif stat.st_mode & 0o170000 == 0o040000:  # Directory
        ftype = "S_IFDIR"
        fdata = ""
    elif stat.st_mode & 0o170000 == 0o010000:  # FIFO
        ftype = "S_IFIFO"
        fdata = ""
    elif stat.st_mode & 0o170000 == 0o140000:  # Socket
        ftype = "S_IFSOCK"
        fdata = ""
    else:
        raise ValueError(f"Encountered an unknown file type at: {path}")

    return ftype, fdata

def file_sha256_chunks(file_path: Path, stat) -> list[str]:
    """Compute the SHA-256 hash for every 16 KB chunk of a file."""
    CHUNK_SIZE = 16 << 10  # 16 KB
    hashes = [str(stat.st_size)]

    with open(file_path, "rb") as file:
        while chunk := file.read(CHUNK_SIZE):
            sha256 = hashlib.sha256(chunk).hexdigest()
            hashes.append(sha256)

    return hashes

# Example usage
if __name__ == "__main__":
    for epkg_path in sys.argv[1:]:
        hash_result = epkg_store_hash(epkg_path)
        print(f"{hash_result}")
