#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Check if the architecture is either x86_64 or aarch64
ARCH=$(uname -m)
if [ "$ARCH" != "x86_64" ] && [ "$ARCH" != "aarch64" ]; then
    echo "Error: This script only supports x86_64 and aarch64 architectures."
    echo "Your system architecture is: $ARCH"
    exit 1
fi
# Download File
EPKG_URL=https://repo.oepkgs.net/openeuler/epkg/rootfs/
# for quick develop-test cycle
EPKG_VERSION=master
EPKG_MANAGER_URL=https://gitee.com/openeuler/epkg/repository/archive/$EPKG_VERSION.tar.gz
EPKG_MANAGER_TAR=$EPKG_VERSION.tar.gz
EPKG_ROOTFS=epkg-rootfs
EPKG_HELPER=epkg-helper
EPKG_HASH=epkg-hash
ELF_LOADER=elf-loader
# Global Epkg Path - Only Global Mode Use
OPT_EPKG=/opt/epkg
PUB_EPKG=$OPT_EPKG/users/public
# User Epkg Path
HOME_EPKG=$HOME/.epkg
# Epkg Mode-based Path
EPKG_INSTALL_MODE=
EPKG_COMMON_ROOT=
EPKG_STORE_ROOT=
ELFLOADER_EXEC=
EPKG_CACHE=
EPKG_PKG_CACHE_DIR=
EPKG_CHANNEL_CACHE_DIR=

# Shell Type
shell=$(basename "$SHELL")
case "$shell" in
	"bash")
		RC_PATH=$HOME/.bashrc
		;;
	"zsh")
		RC_PATH=$HOME/.zshrc
		;;
	*)
		echo "Unsupported shell: $shell"
		exit 1
		;;
esac

dependency_check() {
    local cmd_names="id tar cat cp chmod chown curl"
    local cmd
    local missing_cmds=

    for cmd in $cmd_names; do
        if ! command -v "$1" >/dev/null $cmd; then
            missing_cmds="$missing_cmds $cmd"
        fi
    done

    if [[ -n "$missing_cmds" ]]; then
        echo "Commands '$missing_cmds' not found, please install first"
        return 1
    fi

    return 0
}

select_installation_mode() {
    # User-based choice
    if [[ "$(id -u)" = "0" ]]; then
        echo "Attention: Execute by $USER, Select the installation mode"
        echo "1: user   mode: epkg will be installed in the $HOME/.epkg/"
        echo "2: global mode: epkg will be installed in the /opt/epkg/"
        read mode_choice
    else
        echo "Attention: Execute by $USER, epkg will be installed in the $HOME/.epkg/, sure to continue? (y: continue, others: exit)"
        read mode_choice
        if [[ "$mode_choice" == "y" ]]; then
            mode_choice=1
        fi
    fi
    # Set epkg var
    if [[ "$mode_choice" == "1" ]]; then
        EPKG_INSTALL_MODE="user"
        EPKG_COMMON_ROOT=$HOME_EPKG/envs/common
        EPKG_STORE_ROOT=$HOME_EPKG/store
        EPKG_CACHE=$HOME/.cache/epkg
    elif [[ "$mode_choice" == "2" && "$(id -u)" = "0" ]]; then
        EPKG_INSTALL_MODE="global"
        EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
        EPKG_STORE_ROOT=$OPT_EPKG/store
        EPKG_CACHE=$OPT_EPKG/cache
        RC_PATH=/etc/profile.d/epkg.sh
    elif [[ "$mode_choice" == "2" && "$(id -u)" != "0" ]]; then
        echo "Attention: Please use the root user to execute the global installation mode"
        return 1
    else
        echo "epkg installer exit!"
        return 1
    fi
    ELFLOADER_EXEC=$EPKG_COMMON_ROOT/profile-1/usr/bin/elf-loader
    EPKG_PKG_CACHE_DIR=$EPKG_CACHE/packages
    EPKG_CHANNEL_CACHE_DIR=$EPKG_CACHE/channel
    # Make init home
    create_init_home
}

create_init_home() {
    mkdir -p $EPKG_STORE_ROOT
    mkdir -p $EPKG_PKG_CACHE_DIR
    mkdir -p $EPKG_CHANNEL_CACHE_DIR

    mkdir -p $EPKG_COMMON_ROOT/profile-1/app-bin
    mkdir -p $EPKG_COMMON_ROOT/profile-1/usr/{bin,sbin,lib,lib64}
    mkdir -p $EPKG_COMMON_ROOT/profile-1/etc/epkg

    cd $EPKG_COMMON_ROOT/profile-1
	ln -sT "usr/bin"     "bin"
	ln -sT "usr/sbin"    "sbin"
	ln -sT "usr/lib"     "lib"
	ln -sT "usr/lib64"   "lib64"
    ln -sT "$EPKG_COMMON_ROOT/profile-1" "$EPKG_COMMON_ROOT/profile-current"
}

epkg_verify_checksum() {
    local checksum_file=$1
    pushd "$EPKG_CACHE" > /dev/null 
    if ! sha256sum -c "$checksum_file" > /dev/null 2>&1; then
        echo "checksum error: $checksum_file"
        popd > /dev/null 
        exit 1
    fi
    popd > /dev/null  # 返回原始目录
}

epkg_download() {
    # download epkg_manager    
    echo "download epkg manager"
    curl -# -o $EPKG_CACHE/$EPKG_MANAGER_TAR --max-redirs 3 --location $EPKG_MANAGER_URL

    # download epkg-hash
    echo "download epkg hash"
    curl -# -o $EPKG_CACHE/$EPKG_HASH-$ARCH $EPKG_URL/$EPKG_HASH-$ARCH
    curl -# -o $EPKG_CACHE/$EPKG_HASH-$ARCH.sha256 $EPKG_URL/$EPKG_HASH-$ARCH.sha256
    epkg_verify_checksum "$EPKG_HASH-$ARCH.sha256"

    # download epkg_helper in global mode
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        echo "download epkg helper"
        curl -# -o $EPKG_CACHE/$EPKG_HELPER-$ARCH $EPKG_URL/$EPKG_HELPER-$ARCH
        curl -# -o $EPKG_CACHE/$EPKG_HELPER-$ARCH.sha256 $EPKG_URL/$EPKG_HELPER-$ARCH.sha256
        epkg_verify_checksum "$EPKG_HELPER-$ARCH.sha256"
    fi

    # download epkg elf loader
    echo "download epkg elf loader"
	curl -# -o $EPKG_CACHE/$ELF_LOADER-$ARCH $EPKG_URL/$ELF_LOADER-$ARCH --retry 5
    curl -# -o $EPKG_CACHE/$ELF_LOADER-$ARCH.sha256 $EPKG_URL/$ELF_LOADER-$ARCH.sha256
    epkg_verify_checksum "$ELF_LOADER-$ARCH.sha256"
}

epkg_unpack() {
    # unpack epkg_manager
    tar -xvf $EPKG_CACHE/$EPKG_MANAGER_TAR -C $EPKG_CACHE > /dev/null
    local EPKG_MANAGER_DIR=$EPKG_CACHE/epkg-$EPKG_VERSION

    cp    $EPKG_MANAGER_DIR/bin/epkg.sh  $EPKG_COMMON_ROOT/profile-1/usr/bin/
    cp -a $EPKG_MANAGER_DIR/lib/epkg     $EPKG_COMMON_ROOT/profile-1/usr/lib/
    cp    $EPKG_MANAGER_DIR/channel.json $EPKG_COMMON_ROOT/profile-1/etc/epkg/

    # unpack epkg build
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        cp -a $EPKG_MANAGER_DIR/build  $OPT_EPKG
    else
        cp -a $EPKG_MANAGER_DIR/build  $HOME_EPKG
    fi

    # unpack epkg hash
    cp $EPKG_CACHE/$EPKG_HASH-$ARCH $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HASH

    # unpack epkg_helper
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        /bin/cp -rf $EPKG_CACHE/$EPKG_HELPER-$ARCH $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
        chown -R $USER:$USER $OPT_EPKG
        chmod -R 755 $OPT_EPKG
        chmod 4755 $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
    else
        chown -R $USER:$USER $HOME_EPKG
        chmod -R 755 $HOME_EPKG
    fi

    # unpack elf loader
	/bin/cp -f $EPKG_CACHE/$ELF_LOADER-$ARCH $ELFLOADER_EXEC
    chmod a+x $ELFLOADER_EXEC
}

epkg_change_bashrc() {
    # User-based cat
    if [[ "$(id -u)" = "0" && "$EPKG_INSTALL_MODE" == "global" ]]; then
        cat << EOF >> $RC_PATH

# epkg begin
source /opt/epkg/users/public/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
# epkg end
EOF
    else
        cat << EOF >> $RC_PATH

# epkg begin
source $HOME/.epkg/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
# epkg end
EOF
    fi
}

prepare_epkg_rootfs() {
	local curl_help=$(curl --help all)
	if [ "${curl_help#*--etag-save}" != "$curl_help" ]; then
		local curl_opts="--etag-save $EPKG_CACHE/rootfs-etag.tmp --etag-compare $EPKG_CACHE/rootfs-etag.txt"
	else
		local curl_opts=
	fi

	# download epkg_rootfs
	echo "download epkg rootfs"
	curl $curl_opts -# -o $EPKG_CACHE/$EPKG_ROOTFS-$ARCH.tar.gz $EPKG_URL/$EPKG_ROOTFS-$ARCH.tar.gz --retry 5
    curl $curl_opts -# -o $EPKG_CACHE/$EPKG_ROOTFS-$ARCH.tar.gz.sha256 $EPKG_URL/$EPKG_ROOTFS-$ARCH.tar.gz.sha256
    epkg_verify_checksum "$EPKG_ROOTFS-$ARCH.tar.gz.sha256"
	if [ -s $EPKG_CACHE/rootfs-etag.tmp ]; then
		mv $EPKG_CACHE/rootfs-etag.tmp $EPKG_CACHE/rootfs-etag.txt
	else
		rm -f $EPKG_CACHE/rootfs-etag.tmp
	fi

	# uncompress epkg_rootfs
	echo "install epkg rootfs, it will take 3min, please wait patiently.."
	/bin/tar -zxf $EPKG_CACHE/$EPKG_ROOTFS-$ARCH.tar.gz --strip-components=1 -C $EPKG_STORE_ROOT &> /dev/null
    /bin/chmod -R 755 $EPKG_STORE_ROOT
	# create comm profile-1 symlink to store
	create_rootfs_symlinks
    echo "Environment common created."
}

create_rootfs_symlinks() {
	uncompress_dir="$EPKG_STORE_ROOT"
	symlink_dir="$EPKG_COMMON_ROOT/profile-1"
	for pkg in $(ls $EPKG_STORE_ROOT);
	do
		local fs_dir="$EPKG_STORE_ROOT/$pkg/fs"
		local fs_files=$(/bin/find $fs_dir \( -type f -o -type l \))
		create_symlink_by_fs
	done
}

create_symlink_by_fs() {

	while IFS= read -r fs_file; do
		rfs_file=${fs_file#$fs_dir}

		$ROOTFS_LINK/bin/ls $fs_file &> /dev/null || continue

		$ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$($ROOTFS_LINK/bin/dirname "$rfs_file")"

		if [ "${fs_file#*/bin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [ "${fs_file#*/sbin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [[ "${fs_file}" == *"/etc/"* ]]; then
		    $ROOTFS_LINK/bin/cp -r $fs_file $symlink_dir/$rfs_file &> /dev/null
			continue
		fi

		[ -e "$symlink_dir/$rfs_file" ] && continue

		[[ "$rfs_file" =~  "/etc/yum.repos.d" ]] && continue

        $ROOTFS_LINK/bin/ln -s "$fs_file" "$symlink_dir/$rfs_file"
	done <<< "$fs_files"
}

handle_exec() {
	local file_type=$($ROOTFS_LINK/bin/file $1)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ASCII text executable' ]]; then
		$ROOTFS_LINK/bin/cp $fs_file $symlink_dir/$rfs_file
    elif [[ "$file_type" =~ 'symbolic link' ]]; then
		handle_symlink
	fi
}

handle_symlink() {
	local ln_fs_file=$($epkg_helper $ROOTFS_LINK/bin/readlink -f  $fs_file)
    if [ ! -e "$ln_fs_file" ]; then
        return 1
    fi

	local ln_rfs=${ln_fs_file#$fs_dir}
	ln -sf $symlink_dir/$ln_rfs $symlink_dir/$rfs_file
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$ROOTFS_LINK/bin/cp $ELFLOADER_EXEC $symlink_dir/$rfs_file
    replace_string "$symlink_dir/$rfs_file" "$id1" "$symlink_dir"
    replace_string "$symlink_dir/$rfs_file" "$id2" "$fs_file"
}

replace_string() {
	local binary_file="$1"
	local long_id="$2"
	local str="$3"
    
	local position=$($ROOTFS_LINK/bin/grep -m1 -oba "$long_id" $binary_file | $ROOTFS_LINK/bin/cut -d ":" -f 1)
	[ -n "$position" ] && {
		$ROOTFS_LINK/bin/echo -en "$str\0" | $ROOTFS_LINK/bin/dd of=$binary_file bs=1 seek="$position" conv=notrunc status=none
	}
}

prepare_conf() {
    # curl resolv.conf
    cp /etc/resolv.conf $EPKG_COMMON_ROOT/profile-current/etc/resolv.conf
}

# step 0. dependency check
dependency_check || exit 1

# step 1. select installation mode
select_installation_mode || exit 1
echo "Attention: Directories $EPKG_CACHE and $EPKG_COMMON_ROOT will be created."
echo "Attention: File $RC_PATH will be modified."

# step 2. download - unpack - change bashrc
epkg_download
epkg_unpack
epkg_change_bashrc

# step 3. common env init
prepare_epkg_rootfs
prepare_conf

# step 4. automic init
$EPKG_COMMON_ROOT/profile-1/usr/bin/epkg.sh init
