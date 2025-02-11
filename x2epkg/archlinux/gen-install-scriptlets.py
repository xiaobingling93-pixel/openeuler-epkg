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
    max_functions = content.split("\n}")
    for max_function in max_functions:
        for script_name in script_map.keys():
            if f"{script_name}() " in max_function:
                middle_function = max_function.split(f"{script_name}() ")[1]
                function_body = middle_function.split(os.linesep, 1)[1].strip()
                write_function2_file(function_body, script_map[script_name])
                break


def write_function2_file(function_body, file_name):
    with open(os.path.join(output_dir, file_name), "w") as file:
        file.write(function_body)


if __name__ == '__main__':
    install_file = sys.argv[1]
    output_dir = f"{sys.argv[2]}/install"
    os.makedirs(output_dir, exist_ok=True)
    with open(install_file, "r") as f:
        content = f.read()
    extract_install_scripts()
