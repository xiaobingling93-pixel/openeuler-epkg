#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

init_channel_repo()
{
	local env=$1
	local channel=$2
	local repo=$3

	# channel.yaml
	[[ -f $EPKG_CACHE/epkg-manager/channel/${channel}-channel.yaml ]] || echo "channel ${channel} not found" && return 1
	local env_channel_yaml=${HOME}/.epkg/envs/${env}/profile-current/etc/epkg/channel.yaml
	mkdir -p $(dirname ${env_channel_yaml})
	cp $EPKG_CACHE/epkg-manager/channel/${channel}-channel.yaml  $env_channel_yaml
	# installed-packages.json
	echo -e "{\n}" > $HOME/.epkg/envs/$env/profile-current/installed-packages.json

	# remove in furture: channel.json 
	local env_channel_json=${HOME}/.epkg/envs/${env}/profile-current/etc/epkg/channel.json
	local tmp_env_channel_json=/tmp/channel.json

	[[ -d $(dirname ${env_channel_json}) ]] || mkdir -p $(dirname ${env_channel_json})
	[[ -f ${env_channel_json} ]] || {
		touch ${env_channel_json}
		echo -e "{\n}" > ${env_channel_json}
	}

	local has_channel
	local channel_content
	local repo_content

	# /etc/epkg/channel.json:
	#	channel config file from yum install
	#
	# $COMMON_PROFILE_LINK/etc/epkg/channel.json:
	#	channel config file from script install
	for channel_json in /etc/epkg/channel.json $COMMON_PROFILE_LINK/etc/epkg/channel.json
	do
		[[ -f ${channel_json} ]] || continue

		has_channel=$(cat ${channel_json} | $COMMON_PROFILE_LINK/bin/jq 'has("'"${channel}"'")')

		[[ $has_channel == true ]] || continue

		channel_content=$(cat ${channel_json} | $COMMON_PROFILE_LINK/bin/jq '.["'"${channel}"'"]')

		[[ -z $repo ]] && {
			$COMMON_PROFILE_LINK/bin/jq --argjson channel_content "${channel_content}" '.["'"${channel}"'"] = '"${channel_content}"'' "${env_channel_json}" > ${tmp_env_channel_json} && \
				mv -f ${tmp_env_channel_json} ${env_channel_json}
		}

		[[ -n ${repo} ]] && {
			repo_content=$(echo "${channel_content}" | $COMMON_PROFILE_LINK/bin/jq '.["'"${repo}"'"]')

			$COMMON_PROFILE_LINK/bin/jq --argjson repo_content "${repo_content}" '.["'"${channel}"'"]["'"${repo}"'"] = '"${repo_content}"'' "${env_channel_json}" > ${tmp_env_channel_json} && \
				mv -f ${tmp_env_channel_json} ${env_channel_json}
		}
	done

	return 0
}

init_repo_conf()
{
	local env=$1
	local channel_repo=$2

	local channel
	local repo
	read channel repo <<< ${channel_repo//\// }

	init_channel_repo ${env} ${channel} ${repo}
}

list_repos()
{
	local channel_dir="$EPKG_CACHE/epkg-manager/channel"
	[[ -d ${channel_dir} ]] || return 1

    # get terminal width
    t_width=$(tput cols)
    # define a max value for print line size
    l_length=150
    # case the l_length exceeds the screen width, the length of screen is printed
    l_length=$(( l_length < t_width ? l_length : t_width ))
    printf '%.0s-' $(seq 1 ${l_length})
    printf '\n'
    printf "%-30s | %-15s | %-1s\n" "channel" "repo" "url"
    printf '%.0s-' $(seq 1 ${l_length})
    printf '\n'

    for yaml_file in "${channel_dir}"/*-channel.yaml; do
        [[ -f ${yaml_file} ]] || continue
        
        # 获取channel名称和baseurl
        channel_name=""
        channel_baseurl=""
        in_channel=0
        in_repos=0
        
        while IFS= read -r line; do
            line=$(echo "$line" | sed -e 's/^[[:space:]]*//' -e 's/"//g')
            
            # skip space & comment
            [[ -z $line || $line == \#* ]] && continue
            
            if [[ $line == "channel:" ]]; then
                in_channel=1
                continue
            elif [[ $line == "repos:" ]]; then
                in_channel=0
                in_repos=1
                continue
            fi
            
			# parse channel and repo
            if [[ $in_channel -eq 1 ]]; then
                if [[ $line == "name:"* ]]; then
                    channel_name=$(echo "$line" | sed 's/name:[[:space:]]*//')
                elif [[ $line == "baseurl:"* ]]; then
                    channel_baseurl=$(echo "$line" | sed 's/baseurl:[[:space:]]*//')
                fi
            elif [[ $in_repos -eq 1 ]]; then
                if [[ $line =~ ^[[:space:]]*([^:]+): ]]; then
                    repo_name="${BASH_REMATCH[1]}"
                    printf "%-30s | %-15s | %-1s\n" "$channel_name" "$repo_name" "${channel_baseurl}${repo_name}"
                fi
            fi
        done < "$yaml_file"
    done

    printf '%.0s-' $(seq 1 ${l_length})
    printf '\n'
}

# vim: sw=4 ts=4 et
