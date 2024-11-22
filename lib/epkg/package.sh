#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

install_package() {
	cache_repo
	# /root/.cache/epkg/packages/YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409.epkg
	# /root/.epkg/store/Z7YEZKCXLA5AAMBOV6ZXCG77MZSLMKIM__libev__4.33__4.oe2409/
	#ROOTFS_LINK=$COMMON_PROFILE_LINK
	ROOTFS_LINK=""
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

query_package_requires() {
	local requires=$(accurate_query_requires $1)
	local packges_info=${requires#*PACKAGE  CHANNEL}
	local count=0
	for ite in $packges_info;
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
	for package_url in $packages_url;
	do
		echo "start download $package_url"
		local file="$EPKG_PKG_CACHE_DIR/$($ROOTFS_LINK/bin/basename $package_url)"
		if [ "${curl_help#*--etag-save}" != "$curl_help" ]; then
			local curl_opts="--etag-save $file.etag.tmp --etag-compare $file.etag.txt"
		else
			local curl_opts=
		fi
		$epkg_helper $ROOTFS_LINK/bin/curl -# --insecure $curl_opts -o "$file" "$package_url"  --retry 5
		if test -s "$file.etag.tmp"; then
			mv "$file.etag.tmp" "$file.etag.txt"
		else
			rm -f "$file.etag.tmp"
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
		echo "start install $package"
		local fs_dir="$uncompress_dir/$package/fs"
		local fs_files=$($epkg_helper $ROOTFS_LINK/bin/find $fs_dir \( -type f -o -type l \))
		local appbin_flag="false"
		IFS='__' read -ra pkg_split <<< "$package"
		if [[ "${package_arr[@]}" =~ "${pkg_split[2]}" ]]; then
			appbin_flag="true"
		fi
		create_symlink_by_fs
	done
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
		if [[ "$appbin_flag" == "true" ]]; then
			rfs_file="${rfs_file/\/bin/\/app-bin}"
		fi

		$epkg_helper $ROOTFS_LINK/bin/ls $fs_file &> /dev/null || continue

		# Create parent directory if it doesn't exist
		$epkg_helper $ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$($ROOTFS_LINK/bin/dirname "$rfs_file")"

		#if [ "${fs_file}" == *"/bin/"* ]; then
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

		[ -e "$symlink_dir/$rfs_file" ] && continue

		[[ "$rfs_file" =~  "/etc/yum.repos.d" ]] && continue

		if [ -z "$installroot" ]; then
			$epkg_helper $ROOTFS_LINK/bin/ln -s "$fs_file" "$symlink_dir/$rfs_file"
		else
			$epkg_helper $ROOTFS_LINK/bin/ln -s "${fs_file#$installroot}" "$symlink_dir/$rfs_file"
		fi

	done <<< "$fs_files"
}

handle_exec() {
	local file_type=$($epkg_helper $ROOTFS_LINK/bin/file $1)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ASCII text executable' ]]; then
		$epkg_helper $ROOTFS_LINK/bin/cp $fs_file $symlink_dir/$rfs_file
	fi
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$epkg_helper $ROOTFS_LINK/bin/cp $ELFLOADER_EXEC $symlink_dir/$rfs_file
	if [ -z "$installroot" ]; then
		replace_string "$symlink_dir/$rfs_file" "$id1" "$symlink_dir"
		replace_string "$symlink_dir/$rfs_file" "$id2" "$fs_file"
	else
		replace_string "$symlink_dir/$rfs_file" "$id1" "/"
		replace_string "$symlink_dir/$rfs_file" "$id2" "${fs_file#$installroot}"
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
