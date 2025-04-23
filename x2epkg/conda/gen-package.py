import os
import sys
import json
import yaml

keywords_map = {
        "name": "name",
        "version": "version",
        "depends": "requires",
        "requirements": "requirements",
        "summary": "summary",
        "license": "license",
        "description": "description",
        "home": "homepage",
        "platform": "platform",
        "arch": "arch",
        "subdir": "subdir",
        "constrains": "constrains",
        "timestamp": "buildTime",
        "recipe-maintainers": "recipeMaintainers"
    }


def get_meta_yaml_info():
    """
    read recipe/meta.yaml data
    Returns: meta.yaml dict data
    """
    yaml_path = os.path.join(pkginfo_path, "recipe/meta.yaml")
    if not os.path.exists(yaml_path):
        return {}
    with open(yaml_path, "r") as file:
        content = yaml.safe_load(file.read())
    return content


def get_json_data(path):
    """
    获取json数据
    Args:
        path: json地址

    Returns:

    """
    if not os.path.exists(path):
        return {}
    with open(path, "r") as json_path:
        content = json_path.read()
    return json.loads(content)


def get_basic_info():
    basic_data = {}
    for json_name in ["index.json", "paths.json", "about.json"]:
        basic_data.update(get_json_data(os.path.join(pkginfo_path, json_name)))
    meta_yaml_info = get_meta_yaml_info()
    if "requirements" in meta_yaml_info:
        basic_data.setdefault("requirements", meta_yaml_info.get("requirements"))
    return basic_data


def gen_metadata():
    for old_key, new_key in keywords_map.items():
        if old_key in metadata and new_key != old_key:
            metadata[new_key] = metadata[old_key]
            del metadata[old_key]
    if "-" not in metadata["version"]:
        metadata["release"] = "0"
    else:
        metadata["version"], metadata["release"] = metadata["version"].rsplit("-", 1)
    metadata["epoch"] = 0


if __name__ == '__main__':
    pkginfo_path = sys.argv[1]
    output_path = sys.argv[2]
    backup_pkg_path = sys.argv[3]     # /tmp/****/xxx.pkg.tar.zst

    metadata = get_basic_info()
    gen_metadata()

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
