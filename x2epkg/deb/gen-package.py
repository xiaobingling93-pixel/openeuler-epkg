import os
import sys
import json

keywords_map = {
    "Package": "name",
    "Version": "version",
    "Maintainer": "packager",
    "Build-Depends": "buildRequires",
    "Description": "description",
    "Homepage": "homepage",
    "Architecture": "arch",
    "Source": "source",
    "Depends": "requires",
    "Provides": "provides",
    "Conflicts": "conflicts",
    "Recommends": "recommends",
    "Suggests": "suggests",
    "Enhances": "enhance"
}


def get_basic_info():
    with open(pkginfo_path, "r") as file:
        lines = file.readlines()
    json_data = {}
    for line in lines:
        _keywords = ""
        value = ""
        if line.startswith("#"):
            continue
        if line.startswith(" "):
            value += line
            if _keywords == "":
                print("parse failed from deb control")
                break
            json_data[_keywords] = value
            continue
        if ": " not in line:
            print(f"unknown text in control, text is {line.strip()}")
            continue
        k, _value = line.split(": ", 1)
        _keywords = k.strip()
        if ", " in _value:
            for single in _value.split(", "):
                json_data.setdefault(k.strip(), []).append(single.strip())
        else:
            json_data[_keywords] = _value.strip()
    return json_data


def gen_version():
    version = pkg_name.replace(metadata["name"] + "_", "").rsplit("_", 1)[0]
    if version.endswith("-" + metadata["release"]):
        metadata["version"] = version.rsplit("-", 1)[0]


def gen_metadata():
    for old_key, new_key in keywords_map.items():
        if old_key in metadata:
            metadata[new_key] = metadata[old_key]
            del metadata[old_key]
    rm_keywords = ["Section", "Priority", "Installed-Size"]
    for _key in rm_keywords:
        if _key in metadata:
            del metadata[_key]
    if "-" not in metadata["version"]:
        metadata["release"] = 1
    else:
        metadata["version"], metadata["release"] = metadata["version"].rsplit("-", 1)
    if ":" in metadata["version"]:
        # the ':' exist in the version, should be remove
        gen_version()


if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    pkg_name = sys.argv[3]
    metadata = get_basic_info()
    for keywords in ["Depends", "Build-Depends", "Provides", "Conflicts", "Recommends", "Suggests", "Enhances"]:
        if keywords in metadata and isinstance(metadata[keywords], str):
            metadata[keywords] = [metadata[keywords]]
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
