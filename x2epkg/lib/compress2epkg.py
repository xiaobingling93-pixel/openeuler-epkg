import sys
import json
import os
from pathlib import Path


def get_hash(path):
    return "hash2of2path"


# TODO(main flow: get hash from path)
def get_entry_hash_param(entry: Path) -> (bytes, str):
    try:
        # 使用 os.lstat 而不是 os.stat 以避免解析符号链接
        metadata = entry.lstat()

        if entry.is_symlink():
            target = os.readlink(entry)
            return str.encode(target), "S_IFLNK"
        elif entry.is_file():
            with entry.open("rb") as file:
                content = file.read()
            return content, "S_IFREG"
        elif entry.is_block_device():
            dev_id = metadata.st_dev.to_bytes(8, byteorder='big')
            return dev_id, "S_IFBLK"
        elif entry.is_char_device():
            dev_id = metadata.st_rdev.to_bytes(8, byteorder='big')
            return dev_id, "S_IFCHR"
        elif entry.is_dir():
            return b'', "S_IFDIR"
        elif entry.is_socket():
            return b'', "S_IFSOCK"
        elif entry.is_fifo():
            return b'', "S_IFIFO"
        else:
            raise ValueError(f"Encountered an unknown file type at: {entry}")

    except Exception as e:
        raise RuntimeError(f"Failed to get metadata for {entry}: {e}")


def update_package_json():
    with open(os.path.join(output_path, "package.json"), "r") as f:
        content = f.read()
    metadata = json.loads(content)
    metadata["hash"] = get_hash(epkg_conversion_dir)  # /root/epkg_conversion contain fs and info
    epkg_file_name = f"{metadata['hash']}__{metadata['name']}__{metadata['version']}__{metadata['release']}.epkg"
    metadata["hash_version"] = "1"
    with open(os.path.join(output_path, "package.json"), "w") as f:
        f.write(json.dumps(metadata, sort_keys=True))
    return epkg_file_name


if __name__ == '__main__':
    output_path = sys.argv[1]
    home_path = os.getenv('HOME', '~')
    epkg_conversion_dir = f"{home_path}/epkg_conversion"

    epkg_name = update_package_json()
    os.mkdir(f"{output_path}/store/{epkg_name[:2]}/")
    os.system(f"tar --zstd -cvf {output_path}/store/${epkg_name[:2]}/${epkg_name} -C ${epkg_conversion_dir} .")