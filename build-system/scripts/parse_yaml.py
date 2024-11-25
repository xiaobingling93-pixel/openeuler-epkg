# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

import os
import sys
import yaml
import shutil
import subprocess

epkg_path = "/root/epkg"
workspace = "/root/workspace"
scripts_path = workspace + '/' + "scripts"
tar_sources_path = workspace + '/' + "tar/sources"
tar_patches_path = workspace + '/' + "tar/patches"
src_path = workspace + '/' + "src"
fs_path = workspace + '/' + "fs"

def init_workspace():
    # remove
    if os.path.exists(workspace):
        shutil.rmtree(workspace)

    # mkdir
    if not os.path.exists(scripts_path):
        os.makedirs(scripts_path)
    if not os.path.exists(tar_sources_path):
        os.makedirs(tar_sources_path)
    if not os.path.exists(tar_patches_path):
        os.makedirs(tar_patches_path)
    if not os.path.exists(src_path):
        os.makedirs(src_path)
    if not os.path.exists(fs_path):
        os.makedirs(fs_path)

def parse(yaml_path):
    with open(yaml_path, 'r') as file:
        pkg_meta = yaml.safe_load(file)
    return pkg_meta

def generate_pkgvars(pkg_meta):
    build_system = pkg_meta["buildSystem"]
    build_meta = parse(os.path.join(epkg_path, "build-system/builder", str(build_system) + ".yaml"))
    sytem_build_requires = build_meta["buildRequires"]
    build_requires = sytem_build_requires + pkg_meta["buildRequires"]

    with open(os.path.join(scripts_path, "pkgvars.sh"), "w") as f:
        f.write("#!/usr/bin/env bash" + os.linesep*2)
        f.write("# path vars " + os.linesep)
        # f.write("epkg_build_workspace=" + workspace + os.linesep)
        # f.write("epkg_scripts_path=" + scripts_path + os.linesep)
        # f.write("epkg_tar_sources_path=" + tar_sources_path + os.linesep)
        # f.write("epkg_tar_patches_path=" + tar_patches_path + os.linesep)
        f.write("epkg_src_path=" + src_path + os.linesep)
        f.write("epkg_fs_path=" + fs_path + os.linesep)
        f.write("# pkg vars " + os.linesep)
        f.write("name=" + pkg_meta["name"] + os.linesep)
        f.write("version=" + pkg_meta["version"] + os.linesep)
        f.write("build_system=" + build_system + os.linesep*2)
        f.write("# epkg build env create " + os.linesep)
        f.write("source /root/.bashrc" + os.linesep)
        # f.write("epkg env activate build" + os.linesep)
        f.write("epkg env create build" + os.linesep)
        f.write("epkg install " + ' '.join(build_requires) + os.linesep)
        f.write("# makeFlags vars" + os.linesep)
        f.write("makeFlags=" + build_meta["makeFlags"] + os.linesep)
        f.write("installFlags=" + build_meta["installFlags"] + os.linesep)

def mv_build_sh(pkg_meta):
    build_system = pkg_meta["buildSystem"]

    build_system_script_src=os.path.join(epkg_path, "build-system/builder", str(build_system) + ".sh")
    generic_build_script_src=os.path.join(epkg_path, "build-system/scripts/generic-build.sh")
    phase_script_src=os.path.join(epkg_path, "build-system/scripts/phase.sh")
    shutil.copy(build_system_script_src, scripts_path)
    shutil.copy(generic_build_script_src, scripts_path)
    shutil.copy(phase_script_src, scripts_path)

def unzip_file(filename: str):
    if filename.endswith(".tar.gz") or filename.endswith(".tgz"):
        ret = os.popen(f"tar -xzvf {filename} -C {src_path}").read()
    elif filename.endswith(".tar.xz"):
        ret = os.popen(f"tar -xvf {filename} -C {src_path}").read()
    elif filename.endswith(".tar.bz2"):
        ret = os.popen(f"tar -xjf {filename} -C {src_path}").read()
    elif filename.endswith(".zip"):
        ret = os.popen(f"unzip -o {filename} -d {src_path}").read()
    else:
        print("unknow zip file type!")

def get_sources_and_patches(sources_url: list, patches_url: list):
    # download
    for source_url in sources_url:
        os.system(f"wget {source_url} -P {tar_sources_path}")
    for patch_url in patches_url:
        os.system(f"wget {patch_url} -P {tar_patches_path}")

def unzip_code():
    for source_tar in os.listdir(tar_sources_path):
        unzip_file(os.path.join(tar_sources_path, source_tar))
    
    for patch_tar in os.listdir(tar_patches_path):
        patch_command = ['patch', '-d', os.path.join(src_path, os.listdir(src_path)[0]), '-p1', '-i', os.path.join(tar_patches_path, patch_tar)]
        subprocess.run(patch_command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        print("Patch applied successfully.")

if __name__ == '__main__':
    if len(sys.argv) != 2:
        print("Usage: python parse_yaml.py <yaml_file>")
        sys.exit(1)

    # init workspace
    init_workspace()

    # parse yaml & generate scripts
    pkg_meta=parse(sys.argv[1])
    generate_pkgvars(pkg_meta)
    mv_build_sh(pkg_meta)

    # download & unzip $ patch
    get_sources_and_patches(list(pkg_meta["source"].values()), list(pkg_meta["patches"].values()))
    unzip_code()
