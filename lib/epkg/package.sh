#!/usr/bin/env bash

install_package() {
	local downloaded_packages
	local downloaded_packages2
	local package_files_to_install
	local newly_downloaded_packages
	local install_complete_packages
	local install_updatedb_packages

	record_cached_packages
	invoke_dnf_installation "$@"
	parse_dnf5_output
	record_newly_downloaded_packages
	determine_installation_candidates
	run_rpm_installation
	create_symlinks $package_names_to_install
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

determine_installation_candidates() {
	# echo "Determining installation candidates..."
	install_complete_packages="$newly_downloaded_packages"
	install_updatedb_packages=$(comm -13 <(echo "$newly_downloaded_packages") <(echo "$package_files_to_install"))
}

invoke_dnf_installation() {
	echo "Invoking DNF installation..."
	# download rpms
	$FAKEROOT_EXEC $COMMON_PROFILE_LINK/lib/ld-linux-aarch64.so.1 $COMMON_PROFILE_LINK/bin/dnf5 download "$@" --destdir="$EPKG_PKG_CACHE_DIR" --resolve --alldeps --installroot "$CURRENT_PROFILE_DIR"
	# generate solver.result
	local _pwd=$(pwd)
	cd $COMMON_PROFILE_LINK
	$FAKEROOT_EXEC $COMMON_PROFILE_LINK/lib/ld-linux-aarch64.so.1 $COMMON_PROFILE_LINK/bin/dnf5 install -y "$@" --debugsolver --assumeno --installroot "$CURRENT_PROFILE_DIR"
	cd ${_pwd}
}

# intput line:
# ---> Package pcre2.aarch64 10.42-1.oe2309 will be installed
# output package_files_to_install:
# pcre2-10.42-1.oe2309.aarch64.rpm
parse_dnf_output() {
	package_files_to_install=
	package_names_to_install=

	while IFS= read -r line; do
		# Remove leading '---> Package ' and trailing ' will be installed'
		local package_info=${line#*Package }
		package_info=${package_info% will be installed}
		[ "$package_info" = "$line" ] && continue

		# basesystem.noarch 12-3.oe2203sp3
		# =>
		# basesystem-12-3.oe2203sp3.noarch.rpm
		# Extract package name, version, and architecture
		local package_name=${package_info%%\.*}
		package_info=${package_info#*\.}
		local package_arch=${package_info%% *}
		package_info=${package_info#* }
		local package_version=${package_info%% *}

		# Format output filename
		[ -n "$package_name" ] && [ -n "$package_version" ] && [ -n "$package_arch" ] && {

			local filename="${package_name}-${package_version}.${package_arch}.rpm"
		}

		package_names_to_install="$package_names_to_install"$'\n'"$package_name"
		package_files_to_install="$package_files_to_install"$'\n'"$filename"
	done < $CURRENT_PROFILE_DIR/tmp/dnf_output.txt
}

#install NetworkManager-libnm-1:1.26.2-4.oe1.aarch64@local
#install abattis-cantarell-fonts-0.201-1.oe1.noarch@local
#install acl-2.2.53-8.oe1.aarch64@local
parse_dnf5_output() {
	package_files_to_install=
	package_names_to_install=

	while IFS= read -r line; do
		local package_info=${line##* }
		[ "$package_info" = "$line" ] && continue
		package_info=${package_info%%@*}
		local package_arch=${package_info##*.}
		local package_name=${package_info%-*-*}
		local temp=${package_info#$package_name-}
		local package_version=${temp%.$package_arch}
		package_version=${package_version#*:}
		local filename="${package_name}-${package_version}.${package_arch}.rpm"

		[ -n "$filename" ] && {
			package_names_to_install="$package_names_to_install"$'\n'"$package_name"
			package_files_to_install="$package_files_to_install"$'\n'"$filename"
		}

	done < $COMMON_PROFILE_LINK/debugdata/packages/solver.result
}

run_rpm_installation() {
	echo "Running RPM installation..."
	(
	cd "$EPKG_PKG_CACHE_DIR" || exit

	# Install the newly downloaded packages in db and filesystem
	[ -n "$install_complete_packages" ] && {
		$COMMON_PROFILE_LINK/bin/rpm -i $install_complete_packages --root "$EPKG_STORE_ROOT" --noscripts
	}
	local rpmdb_dir=$EPKG_STORE_ROOT/var/lib/rpm

	# For packages whose files are already in filesystem, call rpm with --justdb
	[ -n "$install_updatedb_packages" ] && {
		$COMMON_PROFILE_LINK/bin/rpm -i $install_updatedb_packages --dbpath "$rpmdb_dir" --justdb
	}
)
}

create_symlinks() {
	# Run rpm -ql to list the files installed by the packages
	local rpmdb_dir=$EPKG_STORE_ROOT/var/lib/rpm
	local files=$($COMMON_PROFILE_LINK/bin/rpm --dbpath "$rpmdb_dir" -ql "$@")
	local file
	local path

	# Create directories and symlinks in the specified env_root directory
	#
	while IFS= read -r file; do
		path=$EPKG_STORE_ROOT/$file
		[ -d "$path" ] && {
			continue
		}
		$COMMON_PROFILE_LINK/bin/ls $path &> /dev/null || continue

		[[ "$file" =~ "is not installed" ]] && {
			continue
		}

		[[ "$file" =~ "contains no files" ]] && {
			continue
		}

		# Create parent directory if it doesn't exist
		tfile=${file#/*/*/*/*/}
		$COMMON_PROFILE_LINK/bin/mkdir -p "$CURRENT_PROFILE_DIR/$(dirname "$tfile")"

		if [ "${file#*/bin/}" != "$file" ]; then
			handle_exec "$path" && continue
		fi

		# Create symlink
		[ -e "$CURRENT_PROFILE_DIR/$tfile" ] && continue
		$COMMON_PROFILE_LINK/bin/ln -s "$path" "$CURRENT_PROFILE_DIR/$tfile"
	done <<< "$files"
}

handle_exec() {
	local file_type=$($COMMON_PROFILE_LINK/bin/file $path)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf
	fi
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$COMMON_PROFILE_LINK/bin/cp $ELFLOADER_EXEC $CURRENT_PROFILE_DIR/$tfile
	replace_string "$CURRENT_PROFILE_DIR/$tfile" "$id1" "$CURRENT_PROFILE_DIR"
	replace_string "$CURRENT_PROFILE_DIR/$tfile" "$id2" "$path"
}

replace_string() {
	local binary_file="$1"
	local long_id="$2"
	local str="$3"

	local position=$(grep -m1 -oba "$long_id" $binary_file | cut -d ":" -f 1)
	echo -en "$str\0" | dd of=$binary_file bs=1 seek="$position" conv=notrunc status=none
}

######### END install_package() #########

remove_package() {
	:
}

upgrade_package() {
	:
}

search_package() {
	$COMMON_PROFILE_LINK/lib/ld-linux-aarch64.so.1 $COMMON_PROFILE_LINK/bin/dnf5 --installroot "$CURRENT_PROFILE_DIR" search "$@"
}

list_packages() {
	rpm -qa --dbpath "$RPMDB_DIR"
}
