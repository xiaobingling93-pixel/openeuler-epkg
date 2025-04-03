#!/usr/bin/env python3
# -*- coding: utf-8 -*-

"""
文件名: create_repo.py

描述:
    这是一个python脚本，输入epkg包仓的store目录和yaml配置文件，输出repo目录。
    eg: python3 create_repo.py -s /root/store -c /etc/create_repo.yaml。

作者: huawei
创建日期: 2025-03-11
最后修改日期: 2025-03-27
版本: 0.1.0
"""

import argparse
import os
import tempfile
from tqdm import tqdm
from pathlib import Path
from file_utils import clean_exist_dir, extract_tar_zst, dump_format_json, compress_file_to_zst, compress_dir_to_zst
from create_index import IndexJson

def parsing_args():
    parser = argparse.ArgumentParser(description="create repo参数")
    parser.add_argument(
        "-s", "--store",
        dest="store",
        default=None,
        required=True,
        help="输入epkg包仓的store目录",
        type=Path,
    )
    parser.add_argument(
        "-c","--config",
        dest="config",
        default=None,
        required=True,
        help="输入repo清单配置文件的地址",
        type=Path,
    )
    return parser.parse_args()

def check_epkg_store_dir(store_dir):
    if not os.path.isdir(store_dir):
        raise argparse.ArgumentTypeError(f"path \"{store_dir}\" not exist")
    if os.path.basename(store_dir) != 'store':
        raise argparse.ArgumentTypeError(f"path \"{store_dir}\" not standard epkg store path")

    invalid_dir_count = 0
    epkg_count = 0
    no_epkg_file_count = 0
    epkg_path_list = []
    for first_level in os.listdir(store_dir):
        parent_dir = os.path.join(store_dir, first_level)
        if not os.path.isdir(parent_dir):
            invalid_dir_count += 1
            print(f"Found invalid file: {parent_dir}")
            continue
        for file_name in os.listdir(parent_dir):
            file_path = os.path.join(parent_dir, file_name)
            if not os.path.isfile(file_path):
                invalid_dir_count += 1
                print(f"Found invalid file: {file_path}")
                continue
            _, ext = os.path.splitext(file_name)
            ext = ext.lower()
            if ext == '.epkg':
                epkg_count += 1
                epkg_path_list.append(file_path)
            else:
                print(f"Found invalid file: {file_path}")
                no_epkg_file_count += 1
    if invalid_dir_count > 0 or no_epkg_file_count > 0:
        raise argparse.ArgumentTypeError(f"path \"{store_dir}\" contains invalid files.")
    if epkg_count == 0:
        raise argparse.ArgumentTypeError(f"path \"{store_dir}\" contains no epkg packages.")
    print(f"Found {epkg_count} epkgs in path: {store_dir}.")
    return epkg_path_list


def get_json_path(epkg_path):
    path_parts = epkg_path.split(os.sep)
    path_parts[len(path_parts) - 3] = 'pkg-info'
    temp_epkg_path = os.path.join(os.sep, *path_parts)
    head, tail = os.path.split(temp_epkg_path)
    os.makedirs(head, exist_ok=True)

    name, _ = os.path.splitext(tail)
    new_tail = name + ".json"
    return os.path.join(head, new_tail)


def scan_epkgs():
    for epkg_path in epkg_path_list:
        extract_tar_zst(epkg_path, parent_dir)
        extracted_json_path = os.path.join(parent_dir, "info/package.json")
        target_json_path = get_json_path(epkg_path)
        os.system(f"mv {extracted_json_path} {target_json_path}")


def generate_repodata(repodata_dir):
    generate_index_json_in_repodata()
    generate_pkginfo_file_in_repodata(repodata_dir)
    generate_storepaths_file_in_repodata(repodata_dir)

def generate_index_json_in_repodata():
    index_json_reader = IndexJson()
    index_content = index_json_reader.get_index_json(config_path)
    dump_format_json(f"{parent_dir}/index.json", index_content)

def generate_pkginfo_file_in_repodata(parent_info_dir):
    pkg_info_dir = f"{os.path.dirname(parent_info_dir)}/pkg-info"
    scan_epkgs()
    compress_dir_to_zst(f"{parent_info_dir}/pkg-info.tar.zst", pkg_info_dir)

def generate_storepaths_file_in_repodata(parent_repodata_dir):
    store_path = f"{parent_repodata_dir}/store-paths"
    with open(store_path, "w") as f:
        for epkg_path in epkg_path_list:
            epkg_name = os.path.basename(epkg_path)
            f.write(epkg_name[:epkg_name.rfind('.')] + "\n")
    compress_file_to_zst(store_path)


if __name__ == "__main__":
    args = parsing_args()
    epkg_path_list = check_epkg_store_dir(args.store)
    config_path = args.config

    parent_dir = os.path.dirname(args.store)
    targets = [f'{parent_dir}/pkg-info', f'{parent_dir}/repodata']
    clean_exist_dir(targets)
    generate_repodata(f"{parent_dir}/repodata")
