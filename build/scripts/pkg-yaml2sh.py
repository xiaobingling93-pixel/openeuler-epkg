# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

import os
import sys
import yaml

def parse(yaml_path):
    with open(yaml_path, 'r') as file:
        pkg_meta = yaml.safe_load(file)
    return pkg_meta

def generate_pkgvars(pkg_meta, build_meta, build_scripts_dir):
    build_requires = list(set(build_meta["buildRequires"] + pkg_meta["buildRequires"]))
    
    with open(os.path.join(build_scripts_dir, "pkgvars.sh"), "w") as f:
        f.write("#!/usr/bin/env bash" + os.linesep*2)

        for k,v in pkg_meta.items():
            if k == "meta" or k == "phase":
                continue
            elif k == "buildRequires":
                v = str(build_requires).replace('[', '(').replace(']', ')').replace(',', '')
            elif k == "sources" or k == "patches":
                v = str(list(v.values())).replace('[', '(').replace(']', ')').replace(',', '').replace('\'', '\"')
            else:
                v = '\"' + str(v) + '\"'
            f.write(k + "=" + v + os.linesep)

def generate_phase(pkg_meta, build_scripts_dir):
    if "phase" not in pkg_meta.keys():
        return

    phase_content = pkg_meta["phase"]
    with open(os.path.join(build_scripts_dir, "phase.sh"), "w") as f:
        f.write("#!/usr/bin/env bash" + os.linesep*2)

        for function_name, function_text in phase_content.items():
            f.write(function_name + "() {" + os.linesep + function_text + os.linesep + "}" + os.linesep) 

if __name__ == '__main__':
    if len(sys.argv) != 4:
        print("Usage: python parse_yaml.py <yaml_file> <epkg_project_dir> <build_scripts_dir> ")
        sys.exit(1)

    # Load argv
    yaml_path=sys.argv[1]
    project_dir=sys.argv[2]
    build_scripts_dir=sys.argv[3]
    
    # Parse yaml
    pkg_meta = parse(sys.argv[1])
    build_meta = parse(os.path.join(project_dir, "build/build-system", str(pkg_meta["buildSystem"]) + ".yaml"))

    # Generate scripts - pkgvars.sh & phase.sh
    generate_pkgvars(pkg_meta, build_meta, build_scripts_dir)
    generate_phase(pkg_meta, build_scripts_dir)