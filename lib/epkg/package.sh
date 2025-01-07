#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

install_package() {
	cache_repo
	# /root/.cache/epkg/packages/YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409.epkg
	# /root/.epkg/store/Z7YEZKCXLA5AAMBOV6ZXCG77MZSLMKIM__libev__4.33__4.oe2409/
	ROOTFS_LINK=$COMMON_PROFILE_LINK
	local require_packages
	local packages_url=""
	local uncompress_dir
	local symlink_dir
	local opt_store="opt/store"
	if [ -z "$installroot" ]; then
		uncompress_dir="$EPKG_STORE_ROOT"
		symlink_dir="$CURRENT_PROFILE_DIR"
	else
		uncompress_dir="$installroot/$opt_store"
		symlink_dir="$installroot"
		$ROOTFS_LINK/bin/mkdir -p $symlink_dir/usr/{bin,sbin,lib,lib64}
		$ROOTFS_LINK/bin/ln -sT "usr/lib" "$symlink_dir/lib"
		$ROOTFS_LINK/bin/ln -sT "usr/lib64" "$symlink_dir/lib64"
		$ROOTFS_LINK/bin/ln -sT "usr/bin" "$symlink_dir/bin"
		$ROOTFS_LINK/bin/ln -sT "usr/sbin" "$symlink_dir/sbin"

	fi
	for dpk in ${package_arr[@]}
	do
		query_package_requires "$dpk"
	done

	local epkg_helper=
	__get_epkg_helper "install_mode"
	
	download_packages
	uncompress_packages
	create_profile_symlinks
	echo "Attention: Install success"
}

# local install demo (support 2024-1230-RC4)
local_install_package() {
	ROOTFS_LINK=$COMMON_PROFILE_LINK
	local local_package=$1
	local package_name=$($ROOTFS_LINK/bin/basename $local_package .epkg)
	local package_arr=($package_name)
	local require_packages=($package_name)
	local uncompress_dir="$EPKG_STORE_ROOT"
	local symlink_dir="$CURRENT_PROFILE_DIR"

	local epkg_helper=
	__get_epkg_helper "install_mode"

	# Todo: epkg mv Permission denied, need fix
	$epkg_helper /bin/mv $local_package $EPKG_PKG_CACHE_DIR

	uncompress_packages
	create_profile_symlinks
	echo "Attention: Install success"
}

query_package_requires() {
	local requires=$(accurate_query_requires $1)
	local packages_info=${requires#*PACKAGE  CHANNEL}
	local count=0
	for ite in $packages_info;
	do
		count=$((count + 1))
		if ((count % 3 == 0)); then
			local pkg_name=$($ROOTFS_LINK/bin/basename $ite .epkg)
			if [[ "$require_packages" ==  *"$pkg_name"* ]];then
				continue
			else
				require_packages+="$pkg_name "
				packages_url+="$ite "
			fi
		fi
	done
}

download_packages() {
	local curl_help=$($ROOTFS_LINK/bin/curl --help all)

	local url_prefix=${packages_url%% *}
	local url_prefix=${url_prefix%/*}
	local url_prefix=${url_prefix%/*}
	echo "Packages location: $url_prefix"

	for package_url in $packages_url;
	do
		echo "Downloading ${package_url##*/}"
		local file="$EPKG_PKG_CACHE_DIR/$($ROOTFS_LINK/bin/basename $package_url)"
		if [ "${curl_help#*--etag-save}" != "$curl_help" ]; then
			local curl_opts="--etag-save $file.etag.tmp --etag-compare $file.etag.txt"
		else
			local curl_opts=
		fi
		$epkg_helper $ROOTFS_LINK/bin/curl --silent --insecure $curl_opts -o "$file" "$package_url"  --retry 5
		if test -s "$file.etag.tmp"; then
			$epkg_helper mv "$file.etag.tmp" "$file.etag.txt"
		else
			$epkg_helper rm -f "$file.etag.tmp"
		fi
	done
}

uncompress_packages() {
	for package in $require_packages;
	do
		local tar_dir="$uncompress_dir/$package"

		test -d $tar_dir/fs && continue

		$epkg_helper $ROOTFS_LINK/bin/mkdir -p "$tar_dir"
		$epkg_helper $ROOTFS_LINK/bin/tar --zstd -xvf $EPKG_PKG_CACHE_DIR/$package.epkg -C $tar_dir &> /dev/null
		$epkg_helper $ROOTFS_LINK/bin/chmod -R 755 $tar_dir
	done
}

create_profile_symlinks() {
	for package in $require_packages;
	do
		echo "Installing $package"
		pushd $uncompress_dir/$package > /dev/null
		local fs_dir="$uncompress_dir/$package/fs"
		local fs_files=$($epkg_helper $ROOTFS_LINK/bin/find $fs_dir \( -type f -o -type l \))
		local appbin_flag="false"
		IFS='__' read -ra pkg_split <<< "$package"
		if [[ "${package_arr[@]}" =~ "${pkg_split[2]}" ]]; then
			appbin_flag="true"
		fi
		create_symlink_by_fs
		postinstall_scriptlet
		popd &> /dev/null
	done
}

postinstall_scriptlet() {
	# remove in future: exec runtimePhase.sh
	IFS='__' read -ra pkg_split <<< "$package"
	if [[ "${pkg_split[2]}" == "golang" ]]; then
		# usr/app-bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/go"    "$symlink_dir/usr/app-bin/go"
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/gofmt" "$symlink_dir/usr/app-bin/gofmt"
		# usr/bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/go"    "$symlink_dir/usr/bin/go"
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/gofmt" "$symlink_dir/usr/bin/gofmt"
	fi
}

create_symlink_by_fs() {
	if [ -z "$symlink_dir" ] || [ "$symlink_dir" = "/" ]; then
		echo "symlink_dir can't be empty or /."
		exit 1
	fi

	local rfs
	local file
	local epkg_helper=
	__get_epkg_helper "env_mode" "$symlink_dir"

	# fs_file=/tmp/epkg-cache/xxx/fs/etc/ima/digest_lists/0-metadata_list-compact-info-7.0.3-3.oe2409.aarch64
	while IFS= read -r fs_file; do
		rfs_file=${fs_file#$fs_dir}

		# app-bin 不应该被跳过
		[ -e "$symlink_dir/$rfs_file" ] && [[ "$appbin_flag" == "false" ]] && continue

		[ -e "$fs_file" ] || [ -L "$fs_file" ] || continue

		local parent_dir=${rfs_file%/*}

		# Create parent directory if it doesn't exist
		[ -e $symlink_dir/$parent_dir ] ||
		$epkg_helper $ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$parent_dir"

		if [ "${fs_file#*/bin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [ "${fs_file#*/sbin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [[ "${fs_file}" == *"/etc/"* ]]; then
			$epkg_helper $ROOTFS_LINK/bin/cp -r $fs_file $symlink_dir/$rfs_file &> /dev/null
			continue
		fi

		[[ "$rfs_file" =~  "/etc/yum.repos.d" ]] && continue

		if [ -z "$installroot" ]; then
			if [ -L "$fs_file" ]; then
				$epkg_helper $ROOTFS_LINK/bin/cp -P "$fs_file" "$symlink_dir/$rfs_file"
			else
				$epkg_helper $ROOTFS_LINK/bin/ln -sf "$fs_file" "$symlink_dir/$rfs_file"
			fi
		else
			if [ -L "$fs_file" ]; then
				$epkg_helper $ROOTFS_LINK/bin/cp -P "${fs_file#$installroot}" "$symlink_dir/$rfs_file"
			else
				$epkg_helper $ROOTFS_LINK/bin/ln -sf "${fs_file#$installroot}" "$symlink_dir/$rfs_file"
			fi
		fi

	done <<< "$fs_files"
}

handle_exec() {
	local file_type=$($epkg_helper $ROOTFS_LINK/bin/file $1)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf $rfs_file
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf $rfs_file
	elif [[ "$file_type" =~ 'ELF 64-bit LSB executable' ]]; then
		handle_elf $rfs_file
	elif [[ "$file_type" =~ 'ASCII text executable' ]]; then
		$epkg_helper $ROOTFS_LINK/bin/cp $fs_file $symlink_dir/$rfs_file
	# test: install autoconf
	elif [[ "$file_type" =~ 'Perl script text executable' ]]; then
		$epkg_helper $ROOTFS_LINK/bin/ln -s $fs_file $symlink_dir/$rfs_file
	elif [[ "$file_type" =~ 'symbolic link' ]]; then
		handle_symlink
	fi

	# Add app-bin path
	if [[ "$appbin_flag" == "true" && "$rfs_file" == "/usr/bin/"* ]]; then
		local rfs_file_appbin="${rfs_file/\/bin/\/app-bin}"
		local file_basename=$($epkg_helper $ROOTFS_LINK/bin/basename "$rfs_file_appbin")
		local parent_dir_appbin=${rfs_file_appbin%/*}
		[ -e $symlink_dir/$parent_dir_appbin ] || $epkg_helper $ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$parent_dir_appbin"
		pushd "$symlink_dir/$parent_dir_appbin" > /dev/null
		$epkg_helper $ROOTFS_LINK/bin/ln -sf "../../bin/$file_basename" "$file_basename"
		popd > /dev/null 
	fi
}

handle_symlink() {
	local ln_fs_file=$($epkg_helper $ROOTFS_LINK/bin/readlink -f  $fs_file)
    if [ ! -e "$ln_fs_file" ]; then
        return 1
    fi

	local ln_rfs=${ln_fs_file#$fs_dir}
	local rfs_file_dirname=$($epkg_helper $ROOTFS_LINK/bin/dirname "$symlink_dir/$rfs_file")
    local rfs_rel_path=$($epkg_helper $ROOTFS_LINK/bin/realpath --relative-to="$rfs_file_dirname" "$symlink_dir/$ln_rfs")
    ln -sf "$rfs_rel_path" "$symlink_dir/$rfs_file"
}

handle_elf() {
	local target_file=$1
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$epkg_helper $ROOTFS_LINK/bin/cp $ELFLOADER_EXEC $symlink_dir/$target_file
	if [ -z "$installroot" ]; then
		replace_string "$symlink_dir/$target_file" "$id1" "$symlink_dir"
		replace_string "$symlink_dir/$target_file" "$id2" "$fs_file"
	else
		replace_string "$symlink_dir/$target_file" "$id1" "/"
		replace_string "$symlink_dir/$target_file" "$id2" "${fs_file#$installroot}"
	fi
}

replace_string() {
	local binary_file="$1"
	local long_id="$2"
	local str="$3"

	local position=$($epkg_helper $ROOTFS_LINK/bin/grep -m1 -oba "$long_id" $binary_file | $ROOTFS_LINK/bin/cut -d ":" -f 1)
	[ -n "$position" ] && {
		$epkg_helper $ROOTFS_LINK/bin/echo -en "$str\0" | $epkg_helper $ROOTFS_LINK/bin/dd of=$binary_file bs=1 seek="$position" conv=notrunc status=none
	}
}


######### END install_package() #########

remove_package() {
	:
}

upgrade_package() {
	:
}

search_package() {
	:
}

list_packages() {
	:
}
