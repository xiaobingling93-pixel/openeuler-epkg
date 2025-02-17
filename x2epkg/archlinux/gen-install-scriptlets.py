import os
import sys

script_map = {
    "pre_install": "pre",
    "post_install": "post",
    "pre_upgrade": "preup",
    "post_upgrade": "postup",
    "pre_remove": "preun",
    "post_remove": "postun"
}

def extract_install_scripts():
    script_output = ""
    local_script_name = ""
    for line in line_list:
        if line == "}\n":
            script_output += line + local_script_name
            write_function2_file(script_output, script_map[local_script_name])
            script_output = ""
            local_script_name = ""
        elif local_script_name != "":
            script_output += line
        for script_name in script_map.keys():
            if line.startswith(f"{script_name}() "):
                script_output += line
                local_script_name = script_name
                break


def write_function2_file(function_body, file_name):
    with open(os.path.join(output_dir, file_name), "w") as file:
        file.write(function_body)


if __name__ == '__main__':
    install_file = sys.argv[1]
    output_dir = f"{sys.argv[2]}/install"
    os.makedirs(output_dir, exist_ok=True)
    with open(install_file, "r") as f:
        line_list = f.readlines()
    extract_install_scripts()
