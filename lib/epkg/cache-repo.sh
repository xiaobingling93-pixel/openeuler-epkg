#!/usr/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

cache_repo_index()
{
	local repo_name=$1
	local repo_url=$2
	local local_cache_path=$EPKG_CHANNEL_CACHE_DIR/${repo_url##*/channel/}

	# Check if store-paths.zst already exists
	if [[ -f "${local_cache_path}/repodata/store-paths.zst" ]]; then
		return
	fi
	
	echo "Caching repodata for: $repo_name"

	# clean old metadata files and re-init metadata dir
	$epkg_helper rm -rf ${local_cache_path} && \
	$epkg_helper mkdir -p ${local_cache_path}/repodata

	# sync repo metadata from local path TODO:epkg_helper
	[[ $repo_url =~ ^/ ]] && {
		cp -r $repo_url/repodata/store-paths.zst ${local_cache_path}/repodata/
		cp -r $repo_url/repodata/pkg-info.zst ${local_cache_path}/repodata/
		cp -r $repo_url/repodata/index.yaml ${local_cache_path}/repodata/
	}

	# sync repo metadata from http urls
	[[ $repo_url =~ ^http ]] && {
		$epkg_helper curl -# -o ${local_cache_path}/repodata/store-paths.zst.tmp $repo_url/repodata/store-paths.zst --retry 5 && \
		$epkg_helper mv ${local_cache_path}/repodata/store-paths.zst.tmp ${local_cache_path}/repodata/store-paths.zst

		$epkg_helper curl -# -o ${local_cache_path}/repodata/pkg-info.zst.tmp $repo_url/repodata/pkg-info.zst --retry 5 && \
		$epkg_helper mv ${local_cache_path}/repodata/pkg-info.zst.tmp ${local_cache_path}/repodata/pkg-info.zst

		$epkg_helper curl -# -o ${local_cache_path}/repodata/index.yaml.tmp $repo_url/repodata/index.yaml --retry 5 &&\
		$epkg_helper mv ${local_cache_path}/repodata/index.yaml.tmp ${local_cache_path}/repodata/index.yaml
	}

	[[ -f ${local_cache_path}/repodata/store-paths.zst ]] && \
	[[ -f ${local_cache_path}/repodata/pkg-info.zst ]] && \
	[[ -f ${local_cache_path}/repodata/index.yaml ]] || {
		echo "Failed to sync metadata for repo:"
		echo "	${repo_url}"

		rm -rf ${local_cache_path}

		return
	}

	# cached medatata file should be decompressed
	$epkg_helper zstd -d -q ${local_cache_path}/repodata/store-paths.zst
	$epkg_helper tar --use-compress-program=zstd -xf ${local_cache_path}/repodata/pkg-info.zst -C ${local_cache_path}/

	echo "Cache repodata succeed: $repo_name"
}

loop_cache_repos()
{
	local channel_conf_file=$1
	local channel_conf_content=$(cat $channel_conf_file)

	local channel_content
	local repo_name
	local repo_enable_code
	local repo_url

	for channel in $(echo ${channel_conf_content} | jq '. | keys[]')
	do
		# channel_content=$(echo "${channel_conf_content}" | jq '.channel['"$i"']')
		channel_content=$(echo "${channel_conf_content}" | jq '.['"${channel}"']')
		[[ ${channel_content} == null ]] && continue

		for repo in $(echo ${channel_content} | jq '. | keys[]')
		do
			repo_content=$(echo "${channel_content}" | jq '.["'${repo//\"/}'"]')
			[[ ${repo_content} == null ]] && continue

			repo_enable_code=$(echo ${repo_content} | jq '.enabled' | tr -d '"')

			# skip cache metadata for disabled repos
			[[ ${repo_enable_code} == 1 ]] || continue

			repo_url=$(echo ${repo_content} | jq '.url' | tr -d '"')

			[[ -z ${repo_url} ]] && continue

			cache_repo_index $repo $repo_url
		done
	done
}

cache_repo()
{
	local epkg_helper=
	__get_epkg_helper "install_mode"

	local old_path=$PATH
	export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin
	if [ -z "$CURRENT_PROFILE_DIR" ]; then
		local channel_conf_dir=/etc/epkg
	else
		local channel_conf_dir=$CURRENT_PROFILE_DIR/etc/epkg
	fi

	for repo_conf_file in "${channel_conf_dir}"/*.json; do
		jq empty ${repo_conf_file} || {
			echo "Epkg channel conf file not in format json: ${repo_conf_file}"
			echo "Fix up and try again."

			continue
		}

		loop_cache_repos ${repo_conf_file}
	done

	export PATH=$old_path
}
