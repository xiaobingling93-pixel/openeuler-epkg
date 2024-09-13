#!/bin/bash

init_channel_repo()
{
	local env=$1
	local channel=$2
	local repo=$3

	local env_channel_json=${HOME}/.epkg/envs/${env}/profile1/etc/epkg/channel.json
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
	# ${HOME}/.epkg/envs/common/profile-current/etc/epkg/channel.json:
	#	channel config file from script install
	for channel_json in /etc/epkg/channel.json ${HOME}/.epkg/envs/common/profile-current/etc/epkg/channel.json
	do
		[[ -f ${channel_json} ]] || continue

		has_channel=$(cat ${channel_json} | jq 'has("'"${channel}"'")')

		[[ $has_channel == true ]] || continue

		channel_content=$(cat ${channel_json} | jq '.["'"${channel}"'"]')

		[[ -z $repo ]] && {
			jq --argjson channel_content "${channel_content}" '.["'"${channel}"'"] = '"${channel_content}"'' "${env_channel_json}" > ${tmp_env_channel_json} && \
				mv -f ${tmp_env_channel_json} ${env_channel_json}
		}

		[[ -n ${repo} ]] && {
			repo_content=$(echo "${channel_content}" | jq '.["'"${repo}"'"]')

			jq --argjson repo_content "${repo_content}" '.["'"${channel}"'"]["'"${repo}"'"] = '"${repo_content}"'' "${env_channel_json}" > ${tmp_env_channel_json} && \
				mv -f ${tmp_env_channel_json} ${env_channel_json}
		}
	done
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
	for channel_json in /etc/epkg/channel.json ${HOME}/.epkg/envs/common/profile-current/etc/epkg/channel.json
	do
		[[ -f ${channel_json} ]] || continue

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

		jq -r 'to_entries[] | "\(.key) \(.value | to_entries[] | "\(.key) \(.value.url)")"' "${channel_json}" | sort | while read -r channel repo url; do
    			printf "%-30s | %-15s | %-1s\n" "$channel" "$repo" "$url"
		done
		printf '%.0s-' $(seq 1 ${l_length})
		printf '\n'

		break
	done
}
