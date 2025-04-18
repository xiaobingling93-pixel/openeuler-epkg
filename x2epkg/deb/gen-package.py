import os
import sys
import json

keywords_map = {
    "Package": "name",
    "Version": "version",
    "Maintainer": "maintainer",
    "Essential": "essential",
    "Important": "important",
    "Protected": "protected",
    "Cnf-Visible-Pkgname": "cnfVisiblePkgname",
    "Original-Maintainer": "originalMaintainer",
    "Build-Depends": "buildRequires",
    "Description": "description",
    "Homepage": "homepage",
    "Architecture": "arch",
    'Multi-Arch': 'multiArch',
    'Source': 'sourcePkg',
    "Depends": "requires",
    "Pre-Depends": "requiresPre",
    "Provides": "provides",
    "Conflicts": "conflicts",
    "Recommends": "recommends",
    "Suggests": "suggests",
    "Enhances": "enhances",
    "Breaks": "breaks",
    "Replaces": "replaces",
    "Installed-Size": "size",
    "Section": "section",
    "Priority": "priority",
    "Original-Vcs-Git": "originalVcsGit",
    "Original-Vcs-Browser": "originalVcsBrowser",
    "Conffiles": "conffiles",
    "Built-Using": "builtUsing"
}


def get_basic_info():
    with open(pkginfo_path, "r") as file:
        lines = file.readlines()
    json_data = {}
    _keywords = ""
    _value = ""
    for line in lines:
        if line.startswith("#"):
            continue
        if line.startswith(" "):
            _value += line.strip() + os.linesep
            if _keywords == "":
                print("parse failed from deb control")
                break
            json_data[_keywords] = _value
            continue
        if ": " not in line:
            print(f"unknown text in control, text is {line.strip()}")
            continue
        k, _value = line.split(": ", 1)
        _keywords = k.strip()
        if _keywords in ["Essential", "Important", "Protected",
                         "Cnf-Visible-Pkgname"] and _value.strip() == "yes":  # bool值优先级字段作为priority的一个选项，即最高优先级
            json_data["priority"] = keywords_map[_keywords]
        if ", " in _value:
            for single in _value.split(", "):
                json_data.setdefault(k.strip(), []).append(single.strip())
        elif _keywords == "Priority" and "priority" in json_data:
            continue
        else:
            json_data.setdefault(_keywords, _value.strip())
    return json_data


def gen_metadata():
    for old_key, new_key in keywords_map.items():
        if old_key in metadata:
            metadata[new_key] = metadata[old_key]
            del metadata[old_key]
    if "-" in metadata["version"]:
        metadata["version"], metadata["release"] = metadata["version"].rsplit("-", 1)
    else:
        metadata["release"] = '0'
    if ":" in metadata["version"]:
        # the ':' exist in the version, should be divided into epoch
        metadata["epoch"], metadata["version"] = metadata["version"].split(":", 1)
    if "\n" in metadata["description"].strip():
        metadata["summary"], metadata["description"] = metadata["description"].split("\n", 1)
    if "epoch" not in metadata:
        metadata["epoch"] = 0
    if "arch" in metadata:
        metadata["arch"] = debian_to_standard_arch(metadata["arch"])

def debian_to_standard_arch(debian_arch):
    """
    Convert Debian architecture names to standard arch names.
    Only includes non-trivial mappings where key ≠ value.

    Args:
        debian_arch (str): Debian architecture name (e.g., 'amd64', 'armel')

    Returns:
        str: Standardized architecture name
    """
    arch_map = {
        # x86
        'i386': 'x86',
        'i486': 'x86',
        'i586': 'x86',
        'i686': 'x86',
        'amd64': 'x86_64',

        # ARM
        'armel': 'arm',
        'armhf': 'armv7',
        'arm64': 'aarch64',
        'arm': 'armv5',

        # PowerPC
        'powerpc': 'ppc',
        'ppc64el': 'ppc64le'
    }

    # Handle case sensitivity and return the mapped value or original if not found
    return arch_map.get(debian_arch.lower(), debian_arch)

def get_conf_files():
    # 读取conffiles的文件，可以作为conffiles字段的内容
    conf_files_path = pkginfo_path.replace("control", "conffiles")
    if not os.path.exists(conf_files_path):
        return
    with open(conf_files_path, "r") as conf_f:
        conf_files = conf_f.read().strip().split(os.linesep)
    return conf_files


if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    pkg_name = sys.argv[3]
    metadata = get_basic_info()
    conffiles = get_conf_files()
    if conffiles is not None:
        metadata.setdefault("conffiles", conffiles)
    for keywords in ["Depends", "Build-Depends", "Pre-Depends", "Provides", "Conflicts", "Recommends", "Suggests", "Enhances"]:
        if keywords in metadata and isinstance(metadata[keywords], str):
            metadata[keywords] = [metadata[keywords]]
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
