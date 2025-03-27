import os
import sys
import json


def get_basic_info():
    # TODO(method)
    epoch = os.popen("rpm -qp --qf %{epoch} " + rpm_path).read().strip()
    if epoch == "(none)":
        epoch = "0"
    basic_data = os.popen("rpm -qp --qf '\"name\": \"%{NAME}\", \"version\": \"%{version}\", \"arch\": \"%{arch}\","
                          " \"release\": \"%{release}\"' " + rpm_path).read()
    lines = basic_data.splitlines()  # 按行分割字符串
    filtered_lines = [line for line in lines if not line.lower().startswith('warning')]  # 过滤掉以 warning 开头的行
    basic_data = os.linesep.join(filtered_lines)
    json_data = json.loads("{" + basic_data + "}")
    json_data["epoch"] = epoch
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
