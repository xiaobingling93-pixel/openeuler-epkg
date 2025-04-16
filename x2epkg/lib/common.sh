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
  find "${epkg_conversion_dir}/fs/" -exec stat -c "%n %a %u %g %F" {} + 2>/dev/null | while read -r path mode uid gid type; do
    relative_path="/${path#$target_dir/}"

    user=$(getent passwd "$uid" | cut -d: -f1)
    group=$(getent group "$gid" | cut -d: -f1)

    [ "$user" = "root" ] && user=""
    [ "$group" = "root" ] && group=""

    if [[ "$type" == "regular file" ]]; then
      file_type="file"
      sha256=$(sha256sum "$path" 2>/dev/null | awk '{print $1}')
    elif [[ "$type" == "directory" ]]; then
      file_type="dir"
      sha256=""
    else
      file_type="$type"
      sha256=""
    fi

    attributes="type=$file_type"
    if [[ "$mode" != "755" && "$mode" != "644" ]]; then
      attributes+=" mode=$mode"
    fi
    [ -n "$user" ] && attributes+=" user=$user"
    [ -n "$group" ] && attributes+=" group=$group"
    [ -n "$sha256" ] && attributes+=" sha256digest=$sha256"

    echo "$relative_path $attributes" >> "${epkg_conversion_dir}/info/files"
  done
}
