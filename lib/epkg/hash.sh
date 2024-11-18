#!/usr/bin/env bash

# Store hash results in a key-value format
declare -A rpm_hash_cache

# XXX: shall unpack and compute over all files, shall define algorithm version
# so that can be verified by epkg-store command.
calculate_base32_hash() {
    local input_file=$1

    # Calculate the sha256 hash of the input file
    local sha256_hash
    sha256_hash=$(sha256sum "$input_file" | awk '{print $1}')

    # Convert the sha256 hash from hex to binary
    local binary_hash
    # nix use compresshash make 32bits -> 20bits, here just get pre-20bits
    binary_hash=$(echo "$sha256_hash" | xxd -r -p | head -c 20 | base32 ) 

    # Output the base32 hash
    echo "$binary_hash"
}

rpm_hash() 
{
    
    local rpm_file=$1

    # Check if the hash for this rpm_file is already calculated
    if [[ -n "${rpm_hash_cache[$rpm_file]}" ]]; then
        echo "${rpm_hash_cache[$rpm_file]}"
        return
    fi

    local temp_cpio=$(mktemp)
    # Convert RPM to CPIO
    rpm2cpio "$rpm_file" > "$temp_cpio"

    # Calculate hash using calculate_base32_hash function from hash.sh
    local hash=$(calculate_base32_hash "$temp_cpio")

    # Remove temporary CPIO file
    rm "$temp_cpio"
    # Store the result in the cache
    rpm_hash_cache[$rpm_file]=$hash

    echo "$hash"
}
