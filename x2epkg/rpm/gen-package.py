import os
import sys
import json


def get_basic_info(full_name):
    # TODO(method)
    name, epoch, version, arch, release = full_name.split()
    return {"name": name, "epoch": epoch, "version": version, "arch": arch, "release": release}


def get_shell_result(cmd):
    return os.popen(cmd).read().strip().split(os.linesep)  # return list


if __name__ == '__main__':
    rpm_name = sys.argv[1]
    output_path = sys.argv[2]
    backup_rpm_path = sys.argv[3]     # /tmp/****/xxx.rpm
    metadata = get_basic_info(rpm_name)
    for keywords in ["requires", "provides", "conflicts", "suggests", "recommends", "supplements", "enhances"]:
        items = get_shell_result(f"rpm -q --{keywords} {rpm_name}")
        if items:
            metadata[keywords] = items

    with open(os.path.join(output_path, "package.json"), "w") as f:
        f.write(json.dumps(metadata, sort_keys=True))