#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Build Dir
BUILD_WORKSPACE_DIR=$HOME/.cache/epkg/build-workspace
BUILD_SCRIPTS_DIR=$BUILD_WORKSPACE_DIR/scripts
BUILD_SOURCES_DIR=$BUILD_WORKSPACE_DIR/sources
BUILD_PATCHES_DIR=$BUILD_WORKSPACE_DIR/patches
BUILD_SRC_DIR=$BUILD_WORKSPACE_DIR/src
BUILD_RESULT_DIR=$BUILD_WORKSPACE_DIR/result
BUILD_FS_DIR=$BUILD_RESULT_DIR/fs
BUILD_INFO_DIR=$BUILD_RESULT_DIR/info
BUILD_PGP_DIR=$BUILD_RESULT_DIR/info/pgp
BUILD_EPKG_DIR=$BUILD_WORKSPACE_DIR/epkg

dependency_check() {
	# Check commands
	for cmd in python3 patch jq tar stat sed curl; do
		if ! command -v "$cmd" >/dev/null 2>&1; then
			echo "Command '$cmd' not found, please install first"
			return 1
		fi
	done

	# Check PyYAML 
	if ! pip show pyyaml &> /dev/null; then
		echo "pyyaml is not installed. Please pip install."
		return 1
	fi

	return 0
}

init_workspace() {
	rm -rf $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_SCRIPTS_DIR
	mkdir -p $BUILD_SOURCES_DIR
	mkdir -p $BUILD_PATCHES_DIR
	mkdir -p $BUILD_SRC_DIR
	mkdir -p $BUILD_RESULT_DIR
	mkdir -p $BUILD_FS_DIR
	mkdir -p $BUILD_INFO_DIR
	mkdir -p $BUILD_PGP_DIR
	mkdir -p $BUILD_EPKG_DIR
	return 0
}

parse_yaml() {
	yaml_path=$1
	python3 "$PROJECT_DIR/build/scripts/pkg-yaml2sh.py" $yaml_path $PROJECT_DIR $BUILD_SCRIPTS_DIR
	echo "Generate pkgvars.sh"
}

source_scripts() {
	source $BUILD_SCRIPTS_DIR/pkgvars.sh
	if [ -e "$BUILD_SCRIPTS_DIR/phase.sh" ]; then
		source $BUILD_SCRIPTS_DIR/phase.sh
	fi
	source $PROJECT_DIR/build/build-system/"${buildSystem}".sh
	source $PROJECT_DIR/build/scripts/generic-download.sh
	source $PROJECT_DIR/build/scripts/generic-phase.sh
}

create_build_env() {
	source $EPKG_COMMON_PROFILE/usr/src/epkg/lib/epkg-rc.sh
	echo "buildRequires:${buildRequires[@]}"
	epkg env create build
	epkg env activate build --pure
	epkg install ${buildRequires[@]}
}

run_phase() {
	phases="prepare build package"
	for curPhase in ${phases[*]}; do
		runPhase "$curPhase"
	done
}

output_mtree_data() {
	local full_path=$1
  	local relative_path=${full_path#${BUILD_FS_DIR%/}/}
	[ -z "$relative_path" ] && relative_path="./"

	stat -c "mode=%a size=%s mtime=%Y" "$full_path" | sed "s|^|$relative_path |"
}

generate_info_files() {
	local dir=$1

	for entry in "$dir"/*; do
		if [ -d "$entry" ]; then
			output_mtree_data "$entry"
			generate_info_files "$entry"
		elif [ -f "$entry" ]; then
			output_mtree_data "$entry"
		fi
	done
}

generate_info_package_json() {
	json_content=$(jq -n \
		--arg name "$name" \
		--arg hash "$hash" \
		--arg epoch "$epoch" \
		--arg version "$version" \
		--arg release "$release" \
		--arg dist "$dist" \
		--arg arch "$(uname -m)" \
		'{name: $name, hash: $hash, epoch: $epoch, version: $version, release: $release, dist: $dist, arch: $arch,
			requires: [],
			provides: {}
		}'
	)

	echo "$json_content"
}

build_pipeline() {
	# step 1. Parse yaml
	parse_yaml $@

	# step 2. Source file
	source_scripts

	# step 3. Download & Extract & Patch
	src_download

	# step 5. Build env create
	cd $BUILD_SRC_DIR/$name-$version
	create_build_env

	# step 6. Run phase
	run_phase
}

post_pipeline() {
	# Calculate Hash (demo)
	epkg_hash_exec=$EPKG_COMMON_PROFILE/usr/bin/epkg
	hash=$($epkg_hash_exec hash "$BUILD_FS_DIR" )
	echo "pkg_hash: $hash, dir: $BUILD_FS_DIR"

	# Generate epkg info (demo, empty file)
	local dist="oe2409"
	local epoch=0
	generate_info_files $BUILD_FS_DIR > $BUILD_INFO_DIR/files
	generate_info_package_json > $BUILD_INFO_DIR/package.json
	touch $BUILD_INFO_DIR/runtimePhase.sh
	touch $BUILD_INFO_DIR/buildinfo.json
	
	# zstd compress
	compress_file=${BUILD_EPKG_DIR}/${hash}__${name}__${version}__${release}.${dist}.epkg
	tar --zstd -cf $compress_file -C $BUILD_RESULT_DIR .
	echo "Compress success: $compress_file"
}

run_build() {
	# Prep Step
	dependency_check || return 1
	init_workspace

	# Main Step
	build_pipeline "$@"

	# Post Step
	post_pipeline
}
