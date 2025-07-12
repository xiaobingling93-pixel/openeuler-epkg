#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Global Epkg Path - Only Global Mode Use
OPT_EPKG=/opt/epkg

# Clean Record
EPKG_CLEAN_DIR=
EPKG_EDIT_FILE=

# Shell Type
shell=$(basename "$SHELL")
case "$shell" in
	"bash")
        RC_FILE=.bashrc
		;;
	"zsh")
        RC_FILE=.zshrc
		;;
	*)
		echo "Unsupported shell: $shell"
		exit 1
		;;
esac

check_exec_user() {
    if [[ -d "/opt/epkg/envs/root/base/" && "$(id -u)" != "0" ]]; then
        echo "Attention: Please use the root user to uninstall global mode epkg."
        return 1
    fi

    return 0
}

clean_user_file() {
    local home=$1

    if [ -d "$home/.epkg/" ]; then
        /bin/rm -rf $home/.epkg/
        EPKG_CLEAN_DIR="$home/.epkg/ $EPKG_CLEAN_DIR"
    fi
    if [ -d $home/.cache/epkg/ ]; then
        # Don't remove $home/.cache/epkg/downloads for quick development cycle
        /bin/rm -rf $home/.cache/epkg/channel
        EPKG_CLEAN_DIR="$home/.cache/epkg/channel $EPKG_CLEAN_DIR"
    fi

    bashrc_file="$home/$RC_FILE"
    if [ -f "$bashrc_file" ]; then
        if grep -q '# epkg begin' "$bashrc_file" && grep -q '# epkg end' "$bashrc_file"; then
            sed -i '/# epkg begin/,/# epkg end/d' "$bashrc_file"
            EPKG_EDIT_FILE="$bashrc_file $EPKG_EDIT_FILE"
        fi
    fi
}

clean_global_file() {
    /bin/rm -rf $OPT_EPKG
    EPKG_CLEAN_DIR="$OPT_EPKG/ $EPKG_CLEAN_DIR"

    ALL_USERS=$(getent passwd | awk -F: '$3 >= 1000 {print $1 ":" $6}')
    ALL_USERS=$(echo "$ALL_USERS" | grep -v '^nobody:')
    ALL_USERS="$ALL_USERS root:/root"

    for USER in $ALL_USERS; do
        IFS=':' read -r user home <<< "$USER"
        clean_user_file $home
    done
}

# step 0. check exec user
check_exec_user || exit 1

# step 1. clean files and context
if test -d /opt/epkg/envs/root/base; then
    clean_global_file
else
    clean_user_file $HOME
fi

echo "Attention: Uninstall success"
echo "Attention: Remove epkg files  : $EPKG_CLEAN_DIR"
echo "Attention: Remove epkg context: $EPKG_EDIT_FILE"
echo "Attention: For changes to take effect, close and re-open your current shell."

# vim: sw=4 ts=4 et
