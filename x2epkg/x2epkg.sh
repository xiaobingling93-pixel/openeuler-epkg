#!/usr/bin/env bash

DEST="/srv/os-repo/epkg3/tmp"

__get_x2epkg_help_info() {
	cat <<-EOF
Usage:
x2epkg xxx.rpm                                # single rpm package
x2epkg xxx.deb                                # single deb package
x2epkg file_path/*.rpm                        # several rpms
x2epkg xxx.rpm --dest PATH                    # convert package into dest dir
EOF
}

# 解析命令行参数
OPT=$(getopt -o h --long out-dir:,help -- "$@")
if [ $? -ne 0 ]; then
    echo "Error: Failed to parse options" >&2
    exit 1
fi
eval set -- "$OPT"

while true; do
    case "$1" in
        --out-dir)
            DEST="$2"
            shift 2
            ;;
        -h|--help)
            __get_x2epkg_help_info
            exit 0
            ;;
        --)
            shift
            break
            ;;
        *)
            echo "Error: Invalid option $1"
            shift
            ;;
    esac
done

if [ $# -eq 0 ]; then
    echo "Error: file path must be provided."
    __get_x2epkg_help_info
    exit 1
fi

# 输出目标路径和repo源
echo "Destination path is: $DEST"
if [ -n "$SRC_REPO" ]; then
    echo "Source repository file is: $SRC_REPO"
fi

# 批量解压逻辑
for pkg in $@; do
  if [ -f "$pkg" ]; then      # single convert
    case "$pkg" in
      *.rpm)
        ./rpm/convert-rpm2epkg.sh "${pkg}" "${DEST}"
        echo "rpm: $pkg"
        ;;
      *.deb)
        echo "deb: $pkg"
        ./deb/convert-deb2epkg.sh "${pkg}" "${DEST}"
        ;;
      *.pkg.tar.zst)
        echo "archlinux: $pkg"
        ./archlinux/convert-archlinux2epkg.sh "${pkg}" "${DEST}"
        ;;
      *)
        help
        exit 1
        ;;
    esac
  fi
done
wait
echo "Operation completed."
