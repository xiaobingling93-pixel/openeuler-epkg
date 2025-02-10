import os
import sys
import json


def get_basic_info():
    # TODO(method)
    with open(pkginfo_path, "r") as f:
        lines = f.readlines()
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


def get_shell_result(cmd):
    result = os.popen(cmd).read().strip()
    if result == "":
        return []
    return result.split(os.linesep)  # return list


def gen_metadata():
    keywords_map = {
        "pkgname": "name",
        "pkgver": "version",
        "depend": "requires",
        "makedepend": "buildRequires",
        "pkgdesc": "description",
        "url": "homepage",
        "conflict": "conflicts"
    }
    for old_key, new_key in keywords_map.items():
        if old_key in metadata:
            metadata[new_key] = metadata[old_key]
            del metadata[old_key]
    rm_keywords = ["pkgbase", "replaces", "size", "builddate", "packager"]
    for _key in rm_keywords:
        if _key in metadata:
            del metadata[_key]
    metadata["release"] = 1



if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    backup_rpm_path = sys.argv[3]     # /tmp/****/xxx.rpm
    metadata = get_basic_info()
    for keywords in ["depend", "makedepend"]:
        if keywords in metadata and isinstance(metadata[keywords], str):
            metadata[keywords] = [metadata[keywords]]
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
