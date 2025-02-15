#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

install_package() {
	cache_repo
	# /root/.cache/epkg/packages/YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409.epkg
	# /root/.epkg/store/Z7YEZKCXLA5AAMBOV6ZXCG77MZSLMKIM__libev__4.33__4.oe2409/
	ROOTFS_LINK=$COMMON_PROFILE_LINK
	declare -A appbin_sources
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
		query_package_sources "$dpk"
		query_package_requires "$dpk"
	done
	[ -z "$require_packages" ] && echo "Attention: No such epkg package" && return 1

	local epkg_helper=
	__get_epkg_helper "install_mode"
	
	download_packages || return
	uncompress_packages || return
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

query_package_sources() {
	local pkg_source=$(get_sources $1)
	[[ -n "$pkg_source" ]] && appbin_sources["$pkg_source"]=1
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
	url_prefix=${url_prefix%/*/*}
	echo "Packages location: $url_prefix"

	for package_url in $packages_url;
	do
		local file="$EPKG_PKG_CACHE_DIR/$($ROOTFS_LINK/bin/basename $package_url)"
		local curl_opts=""
		if [ "${curl_help#*--etag-save}" != "$curl_help" ]; then
			curl_opts="--etag-save $file.etag.tmp --etag-compare $file.etag.txt"
		fi
		# curl		
		local http_status=$($epkg_helper $ROOTFS_LINK/bin/curl $curl_opts --silent --insecure --retry 5 -w "%{http_code}" -o "$file" "$package_url")
		if [[ "$http_status" != "200" && "$http_status" != "304" ]]; then
			echo "Error: Failed to download package from $package_url, http_status: $http_status"
			return 1
		fi
		# etag compare
		if [ -s "$file.etag.tmp" ] && ! cmp -s "$file.etag.txt" "$file.etag.tmp"; then
			echo "Downloading ${package_url##*/}"
			$epkg_helper mv "$file.etag.tmp" "$file.etag.txt"
		else
			$epkg_helper rm -f "$file.etag.tmp"
		fi
	done

	return 0
}

uncompress_packages() {
	for package in $require_packages;
	do
		[ -d "$uncompress_dir/$package/fs" ] && continue
		$epkg_helper $ROOTFS_LINK/bin/mkdir -p "$uncompress_dir/$package"
		$epkg_helper $ROOTFS_LINK/bin/tar --zstd --no-same-owner -xf $EPKG_PKG_CACHE_DIR/$package.epkg -C "$uncompress_dir/$package" || {
			echo "Error: Failed to extract package $EPKG_PKG_CACHE_DIR/$package.epkg"
			return 1
		}
		$epkg_helper $ROOTFS_LINK/bin/chmod -R 755 "$uncompress_dir/$package"
	done

	return 0
}

create_profile_symlinks() {
	for package in $require_packages;
	do
		echo "Installing $package"
		pushd $uncompress_dir/$package > /dev/null
		local fs_dir="$uncompress_dir/$package/fs"
		local fs_files=$($ROOTFS_LINK/bin/find $fs_dir \( -type f -o -type l \))
		local appbin_flag="false"
		IFS='__' read -ra pkg_split <<< "$package"
		local pkg_source
	 	pkg_source=$(get_sources "${pkg_split[2]}")
		if [[ -n "$pkg_source" ]]; then
			[[ -n "${appbin_sources[$pkg_source]}" ]] && appbin_flag="true"
		else
			echo "Attention: $package no source field."
		fi
		create_symlink_by_fs
		postinstall_scriptlet
		popd &> /dev/null
	done
}

postinstall_scriptlet() {
	# remove in future: exec pkg.epkg/info/install/
	IFS='__' read -ra pkg_split <<< "$package"
	if [[ "${pkg_split[2]}" == "golang" ]]; then
		# usr/bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/go"    "$symlink_dir/usr/bin/go"
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/lib/golang/bin/gofmt" "$symlink_dir/usr/bin/gofmt"
		# usr/app-bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "../bin/go"    "$symlink_dir/usr/app-bin/go"
		$epkg_helper $ROOTFS_LINK/bin/ln -s "../bin/gofmt" "$symlink_dir/usr/app-bin/gofmt"
	fi

	if [[ "${pkg_split[2]}" == "ca-certificates" ]]; then
		$epkg_helper $ROOTFS_LINK/bin/cp $COMMON_PROFILE_LINK/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem $symlink_dir/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem
	fi

	if [[ "${pkg_split[2]}" == "maven" ]]; then
		# usr/bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "$symlink_dir/usr/share/maven/bin/mvn"    "$symlink_dir/usr/bin/mvn"
		# usr/app-bin
		$epkg_helper $ROOTFS_LINK/bin/ln -s "../bin/mvn"    "$symlink_dir/usr/app-bin/mvn"
	fi

	if [[ "${pkg_split[2]}" == "python3-pip" ]]; then
		$epkg_helper $ROOTFS_LINK/bin/sed -i '1s|^.*$|#!/usr/bin/env python3|' $symlink_dir/usr/bin/pip
		$epkg_helper $ROOTFS_LINK/bin/sed -i '1s|^.*$|#!/usr/bin/env python3|' $symlink_dir/usr/bin/pip3
		$epkg_helper $ROOTFS_LINK/bin/sed -i '1s|^.*$|#!/usr/bin/env python3|' $symlink_dir/usr/bin/pip3.11
	fi

	if [[ "${pkg_split[2]}" == "ruby" ]]; then
		$epkg_helper $ROOTFS_LINK/bin/sed -i '1s|^.*$|#!/usr/bin/env ruby|' $symlink_dir/usr/bin/erb
	fi

	if [[ "${pkg_split[2]}" == "rubygems" ]]; then
		$epkg_helper $ROOTFS_LINK/bin/sed -i '1s|^.*$|#!/usr/bin/env ruby|' $symlink_dir/usr/bin/gem	
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
		local parent_dir_appbin=${rfs_file_appbin%/*}
		[ -e $symlink_dir/$parent_dir_appbin ] || $epkg_helper $ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$parent_dir_appbin"

    	local rfs_rel_path=$(realpath --relative-to="$symlink_dir/$parent_dir_appbin" "$symlink_dir/$rfs_file")
   		$epkg_helper $ROOTFS_LINK/bin/ln -sf "$rfs_rel_path" "$symlink_dir/$rfs_file_appbin"
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
	# example: cd $cur_envs/usr/bin/
	# relative ln: lrwxrwxrwx. 1 root root     4 Jan  8 15:45  awk -> gawk
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
upgrade_package() {
	:
}

search_package() {
	:
}

# vim: sw=4 ts=4 et
