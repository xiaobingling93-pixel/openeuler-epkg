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
    # Clear the output file
    : > "${epkg_conversion_dir}/info/files"

    find "${epkg_conversion_dir}/fs/" -exec stat -c "%n %a %U %G %F" {} + 2>/dev/null |
        while read -r path mode uname gname type
    do
        # Get relative path safely
        local relative_path="${path#${epkg_conversion_dir}/fs/}"
        [ -z "$relative_path" ] && continue

        # Initialize attributes for each file
        local attributes=
        local file_type=
        local sha256=

        case "$type" in
            "regular file"|"regular empty file")
                file_type="file"
                [ "$mode" = "644" ] && mode=
                sha256=$(sha256sum "$path")
                sha256=${sha256%% *}
                ;;
            "directory")
                file_type="dir"
                [ "$mode" = "755" ] && mode=
                ;;
            "symbolic link")
                file_type="link"
                [ "$mode" = "777" ] && mode=
                ;;
            "character special file")
                file_type="char"
                ;;
            "block special file")
                file_type="block"
                ;;
            "socket")
                file_type="socket"
                ;;
            "fifo")
                file_type="fifo"
                ;;
            *)
                echo "unknown file type: $type $path"
                exit 1
                ;;
        esac

        # Build attributes string
        local attrs="type=$file_type"
        [ -n "$mode" ]          && attrs="$attrs mode=$mode"
        [ "$uname" != "root" ]  && attrs="$attrs uname=$uname"
        [ "$gname" != "root" ]  && attrs="$attrs gname=$gname"
        [ -n "$sha256" ]        && attrs="$attrs sha256=$sha256"

        # Write to file (using printf for reliability with special characters)
        printf "%s %s\n" "$relative_path" "$attrs" >> "${epkg_conversion_dir}/info/files"
    done
}
