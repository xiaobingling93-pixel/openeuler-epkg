# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

import os
import sys
import yaml
import shutil

# Const Var
epkg_global_common_path = "/opt/epkg/users/public/envs/common"
epkg_user_common_path = os.path.join(os.environ.get('HOME'), ".epkg/envs/common")
if os.path.exists(epkg_global_common_path):
    epkg_manager_path = "/opt/epkg"
elif os.path.exists(epkg_user_common_path):
    epkg_manager_path = os.path.join(os.environ.get('HOME'), ".epkg")
else:
    print("Not Found epkg Manager. Maybe exec epkg installer.sh")

# Epkg Build path
workspace = os.path.join(epkg_manager_path, "build/workspace")
scripts_path = os.path.join(workspace, "scripts")
sources_path = os.path.join(workspace, "sources")
patches_path = os.path.join(workspace, "patches")
src_path = os.path.join(workspace, "src")
fs_path = os.path.join(workspace, "fs")

def init_workspace():
    # remove
    if os.path.exists(workspace):
        shutil.rmtree(workspace)

    # mkdir
    if not os.path.exists(scripts_path):
        os.makedirs(scripts_path)
    if not os.path.exists(sources_path):
        os.makedirs(sources_path)
    if not os.path.exists(patches_path):
        os.makedirs(patches_path)
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
    build_meta = parse(os.path.join(epkg_manager_path, "build/build-system", str(build_system) + ".yaml"))
    sytem_build_requires = build_meta["buildRequires"]
    build_requires = sytem_build_requires + pkg_meta["buildRequires"]

    with open(os.path.join(scripts_path, "pkgvars.sh"), "w") as f:
        f.write("#!/usr/bin/env bash" + os.linesep*2)
        f.write("# path vars " + os.linesep)
        # f.write("epkg_build_workspace=" + workspace + os.linesep)
        # f.write("epkg_scripts_path=" + scripts_path + os.linesep)
        # f.write("epkg_sources_path=" + sources_path + os.linesep)
        # f.write("epkg_patches_path=" + patches_path + os.linesep)
        # f.write("epkg_src_path=" + src_path + os.linesep)
        f.write("epkg_fs_path=" + fs_path + os.linesep)
        f.write("# pkg vars " + os.linesep)
        f.write("name=" + pkg_meta["name"] + os.linesep)
        f.write("version=" + pkg_meta["version"] + os.linesep)
        f.write("build_system=" + build_system + os.linesep)
        f.write("build_requires=\"" + ' '.join(build_requires) + "\"" + os.linesep)
        f.write("# makeFlags vars" + os.linesep)
        f.write("makeFlags=" + build_meta["makeFlags"] + os.linesep)
        f.write("installFlags=" + build_meta["installFlags"] + os.linesep)

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

def download(URLs: list, save_path: str):
    for url in URLs:    
        file_path = os.path.join(save_path, os.path.basename(url))
        print("Downloading " + file_path)
        # download
        os.system(f"wget -q {url} -P {save_path}")

def generate_patch_cmd(patch_urls: dict):
    patch_content = pkg_meta["name"]+"_patch() {\n@cmd}\n\n"
    cmd_content = ""
    for patch_url in patch_urls.values():
        file_path = os.path.join(patches_path, os.path.basename(patch_url))
        cmd_content = cmd_content + '\t' + "patch -p1 -N < " + file_path + os.linesep 
    patch_content = patch_content.replace("@cmd", cmd_content)

    # add phase.sh $pkgname_patch content
    with open(os.path.join(scripts_path, "phase.sh"), 'a') as file:
        file.write(patch_content)

def generate_prep_cmd(prep_cmds):
    prep_content = pkg_meta["name"]+"_prep() {\n@cmd}\n\n"
    cmd_content=""
    for prep_cmd in prep_cmds:
        cmd_content = cmd_content + '\t' + prep_cmd + os.linesep
    prep_content = prep_content.replace("@cmd", cmd_content)
    
    # add phase.sh $pkgname_prep content
    with open(os.path.join(scripts_path, "phase.sh"), 'a') as file:
        file.write(prep_content)

def unzip_code():
    for source_tar in os.listdir(sources_path):
        unzip_file(os.path.join(sources_path, source_tar))

if __name__ == '__main__':
    if len(sys.argv) != 2:
        print("Usage: python parse_yaml.py <yaml_file>")
        sys.exit(1)

    # init workspace
    init_workspace()

    # parse yaml & generate scripts
    pkg_meta=parse(sys.argv[1])
    generate_pkgvars(pkg_meta)

    # download & unzip $ patch
    download(list(pkg_meta["source"].values()), sources_path)
    download(list(pkg_meta["patches"].values()), patches_path)

    if "prep" in pkg_meta and pkg_meta["prep"]:
        generate_prep_cmd(pkg_meta["prep"])
    if "patches" in pkg_meta and pkg_meta["patches"]:
        generate_patch_cmd(pkg_meta["patches"])
    unzip_code()
