# encoding:utf-8
import datetime
import os
import shutil
import json
import hashlib
import tarfile
import zstandard as zstd

def clean_exist_dir(targets):
    for target_path in targets:
        if os.path.exists(target_path) and os.path.isdir(target_path):
            shutil.rmtree(target_path)
        os.mkdir(target_path)


def extract_tar_zst(archive_path, extract_dir, zstd_support):
    if zstd_support:
        # extract with command
        result = os.system(f'tar -xf {archive_path} --zstd -C {extract_dir} "./info/package.json"')
        if result != 0:
            print("can't decompress the file :", archive_path)
    else:
        # extract with python module
        with open(archive_path, "rb") as archive:
            dctx = zstd.ZstdDecompressor()
            stream_reader= dctx.stream_reader(archive)
            tar = tarfile.open(fileobj=stream_reader, mode="r|*")
            tar.extractall(path=extract_dir)
            tar.close()
    return os.path.join(extract_dir, "info/package.json")


def dump_format_json(json_path, content_json):
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(content_json, f, ensure_ascii=False, indent=4)

def compress_file_to_zst(file_path):
    with open(file_path, "rb") as infile:
        with open(f"{file_path}.txt.zst", "wb") as outfile:
            compressor = zstd.ZstdCompressor()
            compressor.copy_stream(infile, outfile)
    hash_value = get_file_hash(f"{file_path}.txt.zst")
    current_time = datetime.datetime.now()
    current_date = current_time.strftime("%Y%m%d")
    target_file_name = f"{file_path}-{current_date}-{hash_value[:8]}.txt.zst"
    os.system(f"mv {file_path}.txt.zst {target_file_name}")
    os.remove(file_path)
    return target_file_name, hash_value, current_time

def compress_dir_to_zst(target_file_path, source_dir_path):
    """
    目录压缩成zst文件
    Args:
        target_file_path:
        source_dir_path:

    Returns:
        被压缩的目标文件地址
    """
    normalized_path = source_dir_path.rstrip('/')
    work_dir = os.path.dirname(normalized_path)
    source_dir_name = os.path.basename(normalized_path)
    os.system(f"tar --use-compress-program=zstd -cf {target_file_path} -C {work_dir} {source_dir_name}")
    hash_value = get_file_hash(target_file_path)
    current_time = datetime.datetime.now()
    current_date = current_time.strftime("%Y%m%d")
    hash_file_path = target_file_path.replace(".tar.zst", f"-{current_date}-{hash_value[:8]}.tar.zst")
    os.system(f"mv {target_file_path} {hash_file_path}")
    return hash_file_path, hash_value, current_time


def get_file_hash(file_path):
    with open(file_path, "rb") as f:
        sha256obj = hashlib.sha256()
        sha256obj.update(f.read())
        return sha256obj.hexdigest()
