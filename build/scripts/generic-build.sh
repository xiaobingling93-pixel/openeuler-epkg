#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Project Dir
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	PROJECT_DIR=/opt/epkg
	EPKG_COMMON_PROFILE=$PROJECT_DIR/users/public/envs/common/profile-current
else
	PROJECT_DIR=$HOME/.epkg
	EPKG_COMMON_PROFILE=$PROJECT_DIR/envs/common/profile-current
fi
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

dependency_check() {
	# Check Python 
	if ! command -v python3 &> /dev/null; then
		echo "Python is not installed. Please install."
		return 1
	fi

	# Check PyYAML 
	if ! pip show pyyaml &> /dev/null; then
		echo "pyyaml is not installed. Please install."
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
	return 0
}

parse_yaml() {
	yaml_path=$1
	python "$PROJECT_DIR/build/scripts/pkg-yaml2sh.py" $yaml_path $PROJECT_DIR $BUILD_SCRIPTS_DIR
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
	source $EPKG_COMMON_PROFILE/usr/lib/epkg/epkg-rc.sh
	echo "buildRequires:${buildRequires[@]}"
	epkg env create build
	epkg install ${buildRequires[@]}
}

run_phase() {
	phases="prepare build package"
	for curPhase in ${phases[*]}; do
		runPhase "$curPhase"
	done
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
	# Generate epkg info (demo, empty file)
	touch $BUILD_INFO_DIR/runtimePhase.sh
	touch $BUILD_INFO_DIR/buildinfo.json
	touch $BUILD_INFO_DIR/package.json
	touch $BUILD_INFO_DIR/files

	echo "hash calculate dir: $BUILD_RESULT_DIR"
	epkg_hash_exec=$EPKG_COMMON_PROFILE/usr/bin/epkg-hash
	file_hash=$($epkg_hash_exec "$BUILD_RESULT_DIR" )
	echo "pkg_hash: $file_hash"
}

# Prep Step
dependency_check || exit 1
init_workspace

# Main Step
build_pipeline $@

# Post Step
post_pipeline