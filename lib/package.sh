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
	parse_dnf_output
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
	$FAKEROOT_EXEC dnf -y --installroot "$CURRENT_ENV" --downloadonly --downloaddir="$EPKG_PKG_CACHE_DIR" install "$@" > $CURRENT_ENV/tmp/dnf_output.txt
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

		# Extract package name, version, and architecture
		local package_name=${package_info%% *}
		package_info=${package_info#* }
		local package_version=${package_info%% *}
		package_info=${package_info#* }
		local package_arch=${package_info%% *}

		# Format output filename
		local filename="${package_name}-${package_version}.${package_arch}.rpm"

		package_names_to_install="$package_names_to_install"$'\n'"$package_name"
		package_files_to_install="$package_files_to_install"$'\n'"$filename"
	done < $CURRENT_ENV/tmp/dnf_output.txt
}

run_rpm_installation() {
	echo "Running RPM installation..."
	(
	cd "$EPKG_PKG_CACHE_DIR" || exit

	# Install the newly downloaded packages in db and filesystem
	rpm -i $install_complete_packages --dbpath "$RPMDB_DIR" --root "$EPKG_STORE_ROOT" --noscripts

	# For packages whose files are already in filesystem, call rpm with --justdb
	rpm -i $install_updatedb_packages --dbpath "$RPMDB_DIR" --justdb
)
}

create_symlinks() {
	# Run rpm -ql to list the files installed by the packages
	local files=$(rpm --dbpath "$RPMDB_DIR" -ql "$@")
	local file
	local path

	# Create directories and symlinks in the specified env_root directory
	#
	while IFS= read -r file; do
		path=$EPKG_STORE_ROOT/$file
		[ -d "$file" ] && continue

		# Create parent directory if it doesn't exist
		mkdir -p "$CURRENT_ENV/$(dirname "$file")"

		if [ "${file#*/bin/}" != "$file" ]; then
			handle_exec "$path" && continue
		fi

		# Create symlink
		ln -s "$path" "$CURRENT_ENV/$file"
	done <<< "$files"
}

handle_exec() {
	local file_type=$(file $path)

	case $file_type in
		*ELF 64-bit LSB shared object*)
			handle_elf;;
		*)
			return 1
			;;
	esac
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	cp $ELFLOADER_EXEC $CURRENT_ENV/$file
	replace_string $CURRENT_ENV/$file $id1 $path
	replace_string $CURRENT_ENV/$file $id2 $CURRENT_ENV
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
	dnf --installroot "$CURRENT_ENV" search "$@"
}

list_packages() {
	rpm -qa --dbpath "$RPMDB_DIR"
}
