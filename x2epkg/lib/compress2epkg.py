import sys
import json
import os
from collections import OrderedDict

# keywords sequence
desired_order = ['name', 'version', 'summary', 'epoch', 'license', 'release', 'homepage', 'arch', 'hash',
                 'hashVersion', 'source', 'description', 'buildRequires', 'requires', 'requiresPre', 'requiresPreun',
                 'requiresPost', 'requiresPostun', "provides", "conflicts", "suggests", "recommends", "supplements",
                 "enhances", "breaks", "replaces", "packager", "originUrl", "size", "section", "priority",
                 "buildTime", "buildHost"]

def run_epkg_hash(path):
    local_path = os.getcwd()
    hash_script = os.path.join(local_path, "../src/hash.py")
    result = os.popen(f"python3 {hash_script} {path}").read().strip()
    return result


def update_package_json():
    with open(os.path.join(epkg_conversion_dir, "info", "package.json"), "r") as f:
        content = f.read()
    metadata = json.loads(content)
    if "ubuntu" in origin_url:
        metadata['release'] += ".noble"
    metadata["hash"] = run_epkg_hash(epkg_conversion_dir)  # /root/epkg_conversion contain fs and info
    epkg_file_name = f"{metadata['hash']}__{metadata['name']}__{metadata['version']}__{metadata['release']}.epkg"
    metadata["hashVersion"] = "1"
    metadata.setdefault("originUrl", origin_url)
    # 按顺序构建有序字典
    ordered_data = OrderedDict()
    for key in desired_order:
        if key in metadata:
            ordered_data[key] = metadata[key]

    # 写入JSON文件
    with open(os.path.join(epkg_conversion_dir, "info", "package.json"), "w") as f:
        json.dump(ordered_data, f, indent=2)
    return epkg_file_name


if __name__ == '__main__':
    output_path = sys.argv[1]
    origin_url = sys.argv[2]
    home_path = os.getenv('HOME', '~')
    epkg_conversion_dir = f"{home_path}/epkg_conversion"

    epkg_name = update_package_json()
    os.makedirs(f"{output_path}/store/{epkg_name[:2]}/", exist_ok=True)
    os.system(f"tar --zstd -cf {output_path}/store/{epkg_name[:2]}/{epkg_name} -C {epkg_conversion_dir} .")
