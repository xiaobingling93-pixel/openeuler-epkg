#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

init_channel_repo()
{
	local env=$1
	local channel=$2
	local repo=$3

	# channel.yaml
	local env_channel_yaml=${HOME}/.epkg/envs/${env}/profile-current/etc/epkg/channel.yaml
	mkdir -p $(dirname ${env_channel_yaml})
	cp $EPKG_CACHE/epkg-manager/channel/${channel}-channel.yaml  $env_channel_yaml
	# installed-packages.json
	echo -e "{\n}" > $HOME/.epkg/envs/$env/profile-current/installed-packages.json

	return 0
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
