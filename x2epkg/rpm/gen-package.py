import os
import re
import sys
import json


def get_basic_info():
    epoch = os.popen("rpm -qp --qf %{epoch} " + rpm_path).read().strip()
    if epoch == "(none)":
        epoch = "0"
    basic_data = os.popen("rpm -qp --qf '\"name\": \"%{NAME}\", \"version\": \"%{version}\", \"arch\": \"%{arch}\","
                          " \"license\": \"%{license}\", \"release\": \"%{release}\", \"buildTime\": \"%{buildtime}\","
                          " \"buildHost\": \"%{buildhost}\", \"homepage\": \"%{url}\", \"size\": \"%{size}\"' " + rpm_path).read()
    lines = basic_data.splitlines()  # 按行分割字符串
    filtered_lines = [line for line in lines if not line.lower().startswith('warning')]  # 过滤掉以 warning 开头的行
    basic_data = os.linesep.join(filtered_lines)
    json_data = json.loads("{" + basic_data + "}")
    json_data["epoch"] = epoch
    keys = ["summary", "description", "group", "platform", "changelogTime", "changelogName", "changelogText",
            "sourceRpm", "sourcePkgId", "cookie"]
    for k in keys:
        k_info = os.popen("rpm -qp --qf \"%{" + k.lower() + "}\" " + rpm_path).read()
        if k_info == "(none)":
            continue
        if k == "sourceRpm":
            k = "sourcePkg"
        json_data[k] = k_info
    content = os.popen(f"rpm -qp --info " + rpm_path).read()
    signature = re.search("Signature.*: (.*)", content).group(1)
    json_data["signature"] = signature
    return json_data


def get_shell_result(cmd):
    result = os.popen(cmd).read().strip()
    if result == "":
        return []
    return result.split(os.linesep)  # return list


def remove_duplicates(lst):
    seen = set()
    result = []
    for item in lst:
        if item not in seen:
            result.append(item)
            seen.add(item)
    return result


if __name__ == '__main__':
    rpm_path = sys.argv[1]
    output_path = sys.argv[2]
    backup_rpm_path = sys.argv[3]     # /tmp/****/xxx.rpm
    metadata = get_basic_info()
    for keywords in ["requires", "provides", "conflicts", "suggests", "recommends", "supplements", "enhances"]:
        items = get_shell_result(f"rpm -q --{keywords} {rpm_path}")
        if items:
            metadata[keywords] = remove_duplicates(items)

    with open(os.path.join(output_path, "package.json"), "w") as f:
        json.dump(metadata, f, indent=2, sort_keys=True)
