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
    "Pre-Depends": "requiresPre",
    "Provides": "provides",
    "Conflicts": "conflicts",
    "Recommends": "recommends",
    "Suggests": "suggests",
    "Enhances": "enhances",
    "Installed-Size": "installedSize",
    "Section": "section",
    "Priority": "priority"
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
        if ", " in _value:
            for single in _value.split(", "):
                json_data.setdefault(k.strip(), []).append(single.strip())
        else:
            json_data[_keywords] = _value.strip()
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
    metadata["release"] = metadata["release"] + ".noble"
    if ":" in metadata["version"]:
        # the ':' exist in the version, should be divided into epoch
        _, metadata["version"] = metadata["version"].split(":", 1)
    if "\n" in metadata["description"].strip():
        metadata["summary"], metadata["description"] = metadata["description"].split("\n", 1)
    metadata["epoch"] = 0


if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    pkg_name = sys.argv[3]
    metadata = get_basic_info()
    for keywords in ["Depends", "Build-Depends", "Pre-Depends", "Provides", "Conflicts", "Recommends", "Suggests", "Enhances"]:
        if keywords in metadata and isinstance(metadata[keywords], str):
            metadata[keywords] = [metadata[keywords]]
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
