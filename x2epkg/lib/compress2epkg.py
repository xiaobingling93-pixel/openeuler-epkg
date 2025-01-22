import sys
import json
import os


def run_epkg_hash(path):
    local_path = os.getcwd()
    hash_script = os.path.join(local_path, "../src/hash.py")
    result = os.popen(f"python3 {hash_script} {path}").read().strip()
    return result


def update_package_json():
    with open(os.path.join(epkg_conversion_dir, "info", "package.json"), "r") as f:
        content = f.read()
    metadata = json.loads(content)
    metadata["hash"] = run_epkg_hash(epkg_conversion_dir)  # /root/epkg_conversion contain fs and info
    epkg_file_name = f"{metadata['hash']}__{metadata['name']}__{metadata['version']}__{metadata['release']}.epkg"
    metadata["hash_version"] = "1"
    with open(os.path.join(epkg_conversion_dir, "info", "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
    return epkg_file_name


if __name__ == '__main__':
    output_path = sys.argv[1]
    home_path = os.getenv('HOME', '~')
    epkg_conversion_dir = f"{home_path}/epkg_conversion"

    epkg_name = update_package_json()
    os.makedirs(f"{output_path}/store/{epkg_name[:2]}/", exist_ok=True)
    os.system(f"tar --zstd -cvf {output_path}/store/{epkg_name[:2]}/{epkg_name} -C {epkg_conversion_dir} .")