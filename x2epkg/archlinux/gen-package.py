import os
import sys
import json


def get_basic_info():
    # TODO(method)
    with open(pkginfo_path, "r") as file:
        lines = file.readlines()
    json_data = {}
    for line in lines:
        if line.startswith("#"):
            continue
        if " = " in line:
            k, value = line.split(" = ", 1)
            value = value.strip()
            if k in json_data and isinstance(json_data[k], str):
                json_data[k] = [json_data[k], value]
            elif k in json_data and isinstance(json_data[k], list):
                json_data[k].append(value)
            else:
                json_data[k.strip()] = value
    return json_data


def gen_metadata():
    keywords_map = {
        "pkgname": "name",
        "pkgver": "version",
        "depend": "requires",
        "makedepend": "buildRequires",
        "pkgdesc": "description",
        "url": "homepage",
        "conflict": "conflicts",
        "builddate": "buildTime"
    }
    for old_key, new_key in keywords_map.items():
        if old_key in metadata:
            metadata[new_key] = metadata[old_key]
            del metadata[old_key]
    if "-" not in metadata["version"]:
        metadata["release"] = "0"
    else:
        metadata["version"], metadata["release"] = metadata["version"].rsplit("-", 1)
    metadata["epoch"] = 0
    rm_keywords = ["pkgbase", "xdata"]
    for _key in rm_keywords:
        if _key in metadata:
            del metadata[_key]


if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    backup_pkg_path = sys.argv[3]     # /tmp/****/xxx.pkg.tar.zst
    metadata = get_basic_info()
    for keywords in ["depend", "makedepend"]:
        if keywords in metadata and isinstance(metadata[keywords], str):
            metadata[keywords] = [metadata[keywords]]
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
