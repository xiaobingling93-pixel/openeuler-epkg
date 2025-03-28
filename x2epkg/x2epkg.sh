#!/usr/bin/env bash

OUT_DIR=""
ORIGIN_URL=""

show_help() {
	cat <<-EOF
Usage:
x2epkg xxx.rpm --origin-url https://***                               # single rpm package
x2epkg xxx.deb --origin-url https://***                               # single deb package
x2epkg file_path/*.rpm --origin-url https://***                       # several rpms
x2epkg xxx.rpm --out-dir PATH --origin-url https://***                # convert package into output dir
EOF
}

# 解析命令行参数
OPT=$(getopt -o h --long origin-url:,out-dir:,help -- "$@")
if [ $? -ne 0 ]; then
    echo "Error: Failed to parse options" >&2
    exit 1
fi
eval set -- "$OPT"

while true; do
    case "$1" in
        --origin-url)
            ORIGIN_URL="$2"
            shift 2
            ;;
        --out-dir)
            OUT_DIR="$2"
            shift 2
            ;;
        -h|--help)
            show_help
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
    show_help
    exit 1
fi

# 检查必要参数
if [[ -z "$ORIGIN_URL" ]]; then
    echo "错误：必须提供 --origin-url 参数" >&2
    show_help
    exit 1
fi

# 输出目标路径
echo "Output path is: $OUT_DIR"
echo "Origin url is: $ORIGIN_URL"

# 批量解压逻辑
for pkg in "$@"; do
  if [ -f "$pkg" ]; then      # single convert
    case "$pkg" in
      *.rpm)
        source rpm/convert-rpm2epkg.sh "${pkg}"
        echo "rpm: $pkg"
        ;;
      *.deb)
        echo "deb: $pkg"
        source deb/convert-deb2epkg.sh "${pkg}"
        ;;
      *.pkg.tar.zst)
        echo "archlinux: $pkg"
        source archlinux/convert-archlinux2epkg.sh "${pkg}"
        ;;
      *)
        help
        exit 1
        ;;
    esac
  fi
done
echo "Operation completed."
