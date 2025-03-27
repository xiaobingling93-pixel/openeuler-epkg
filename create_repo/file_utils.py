# encoding:utf-8
import os
import shutil
import json
import tarfile
import zstandard as zstd

def clean_exist_dir(targets):
    for target_path in targets:
        if os.path.exists(target_path) and os.path.isdir(target_path):
            shutil.rmtree(target_path)
        os.mkdir(target_path)


def extract_tar_zst(archive_path, extract_dir):
    with open(archive_path, "rb") as archive:
        dctx = zstd.ZstdDecompressor()
        stream_reader = dctx.stream_reader(archive)
        tar = tarfile.open(fileobj=stream_reader, mode="r|*")
        tar.extractall(path=extract_dir)
        tar.close()


def dump_format_json(json_path, content_json):
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(content_json, f, ensure_ascii=False, indent=4)

def compress_file_to_zst(file_path):
    with open(file_path, "rb") as infile:
        with open(f"{file_path}.zst", "wb") as outfile:
            compressor = zstd.ZstdCompressor()
            compressor.copy_stream(infile, outfile)
    os.remove(file_path)

def compress_dir_to_zst(target_file_path, source_dir_path):
    normalized_path = source_dir_path.rstrip('/')
    work_dir = os.path.dirname(normalized_path)
    source_dir_name = os.path.basename(normalized_path)
    os.system(f"tar --use-compress-program=zstd -cf {target_file_path} -C {work_dir} {source_dir_name}")
