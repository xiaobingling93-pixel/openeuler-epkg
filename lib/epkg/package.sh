#!/usr/bin/env bash

. ./query.sh

install_package() {
	local downloaded_packages
	local downloaded_packages2
	local newly_downloaded_packages
	record_cached_packages
	download_packages "$@"
	record_newly_downloaded_packages
	uncompress_packages
	create_profile_symlinks
}

record_cached_packages() {
	# echo "Recording currently cached packages..."
	downloaded_packages=$(ls "$EPKG_PKG_CACHE_DIR")
}

record_newly_downloaded_packages() {
	# echo "Recording newly downloaded packages..."
	downloaded_packages2=$(ls "$EPKG_PKG_CACHE_DIR")
	newly_downloaded_packages=$(comm -13 <(echo "$downloaded_packages") <(echo "$downloaded_packages2"))
}


download_packages() {
	local requires=$(accurate_query_requires $1)
	local packges_info=${requires#*PACKAGE  CHANNEL}
	local packages_url=""
	local count=0
	for ite in $packges_info;
	do
		count=$((count + 1))
		if ((count % 3 == 0)); then
			packages_url+="$ite "
		fi
	done

	for package_url in $packages_url;
	do
		$COMMON_PROFILE_LINK/bin/cp  "$package_url" "$EPKG_PKG_CACHE_DIR"
	done
}

uncompress_packages() {
	for package in $newly_downloaded_packages;
	do
		local package_full_name=${package%.*}
		local tar_dir="$EPKG_STORE_ROOT/$package_full_name"
		mkdir -p "$tar_dir"
		$COMMON_PROFILE_LINK/bin/tar --zstd -xvf $EPKG_PKG_CACHE_DIR/$package -C $tar_dir
	done
}

create_profile_symlinks() {
	for package in $newly_downloaded_packages;
	do
		local package_full_name=${package%.*}
		local fs_dir="$EPKG_STORE_ROOT/$package_full_name/fs"
		local fs_files=$(find $fs_dir -type f)
		create_symlink_by_fs
	done
}

create_symlink_by_fs() {
	local rfs
	local file

	# fs_file=/tmp/epkg-cache/xxx/fs/etc/ima/digest_lists/0-metadata_list-compact-info-7.0.3-3.oe2409.aarch64
	while IFS= read -r fs_file; do
		rfs_file=${fs_file#$fs_dir}

		$COMMON_PROFILE_LINK/bin/ls $fs_file &> /dev/null || continue

		# Create parent directory if it doesn't exist
		$COMMON_PROFILE_LINK/bin/mkdir -p "$CURRENT_PROFILE_DIR/$($COMMON_PROFILE_LINK/bin/dirname "$rfs_file")"

		#if [ "${fs_file}" == *"/bin/"* ]; then
		if [ "${fs_file#*/bin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [[ "${fs_file}" == *"/etc/"* ]]; then
			$COMMON_PROFILE_LINK/bin/cp $fs_file $CURRENT_PROFILE_DIR/$rfs_file
			continue
		fi

		[ -e "$CURRENT_PROFILE_DIR/$rfs_file" ] && continue
		#[ -e "$CURRENT_PROFILE_DIR/$rfs_file" ] && rm -rf $CURRENT_PROFILE_DIR/$rfs_file

		[[ "$rfs_file" =~  "/etc/yum.repos.d" ]] && continue

		$COMMON_PROFILE_LINK/bin/ln -s "$fs_file" "$CURRENT_PROFILE_DIR/$rfs_file"
	done <<< "$fs_files"
}

handle_exec() {
	local file_type=$($COMMON_PROFILE_LINK/bin/file $1)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf
	fi
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$COMMON_PROFILE_LINK/bin/cp $ELFLOADER_EXEC $CURRENT_PROFILE_DIR/$rfs_file
	replace_string "$CURRENT_PROFILE_DIR/$rfs_file" "$id1" "$CURRENT_PROFILE_DIR"
	replace_string "$CURRENT_PROFILE_DIR/$rfs_file" "$id2" "$fs_file"
}

replace_string() {
	local binary_file="$1"
	local long_id="$2"
	local str="$3"

	local position=$(/usr/bin/grep -m1 -oba "$long_id" $binary_file | $COMMON_PROFILE_LINK/bin/cut -d ":" -f 1)
	[ -n "$position" ] && {
		$COMMON_PROFILE_LINK/bin/echo -en "$str\0" | $COMMON_PROFILE_LINK/bin/dd of=$binary_file bs=1 seek="$position" conv=notrunc status=none
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
