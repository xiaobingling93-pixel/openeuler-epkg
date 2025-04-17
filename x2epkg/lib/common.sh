#!/bin/bash

epkg_conversion_dir="${HOME}/epkg_conversion"

init_conversion_dirs()
{
rm -rf ${epkg_conversion_dir}/*

mkdir -p ${epkg_conversion_dir}/{fs,info}
mkdir -p ${epkg_conversion_dir}/info/pgp
mkdir -p ${epkg_conversion_dir}/info/install
touch ${epkg_conversion_dir}/info/{package.json,files}
}

generate_mtree_files()
{
  find "${epkg_conversion_dir}/fs/" -exec stat -c "%n %a %U %G %F" {} + 2>/dev/null | while read -r path mode uname gname type; do
    relative_path="/${path#${epkg_conversion_dir}/fs/}"

    case "$type" in
      "regular file")
        file_type="file"
        [[ "$mode" != "644" ]] || attributes+="mode=$mode"
        sha256=$(sha256sum "$path" 2>/dev/null | awk '{print $1}')
        ;;
      "directory")
        file_type="dir"
        [[ "$mode" != "755" ]] || attributes+="mode=$mode"
        sha256=""
        ;;
      "symbolic link")
        file_type="link"
        [[ "$mode" != "777" ]] || attributes+="mode=$mode"
        sha256=$(sha256sum "$path" 2>/dev/null | awk '{print $1}')
        ;;
      *)
        file_type="$type"
        sha256=""
        ;;
    esac

    [ "$uname" != "root" ] && attributes+=" uname=$uname"
    [ "$gname" != "root" ] && attributes+=" group=$gname"
    attributes="type=$file_type"
    [ -n "$sha256" ] && attributes+=" sha256=$sha256"

    echo "$relative_path $attributes" >> "${epkg_conversion_dir}/info/files"
  done
}
