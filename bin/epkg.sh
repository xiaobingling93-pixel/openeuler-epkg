#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	export EPKG_COMMON_PROFILE=/opt/epkg/users/public/envs/common/profile-current
	export PROJECT_DIR=/opt/epkg
else
	export EPKG_COMMON_PROFILE=$HOME/.epkg/envs/common/profile-current
	export PROJECT_DIR=$HOME/.epkg
fi

export PATH=$EPKG_COMMON_PROFILE/usr/bin:$PATH

source $EPKG_COMMON_PROFILE/usr/lib/epkg/paths.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/env.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/init.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/package.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/epkg-rc.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/query.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/cache-repo.sh
source $EPKG_COMMON_PROFILE/usr/lib/epkg/repo.sh
source $PROJECT_DIR/build/scripts/generic-build.sh

__get_epkg_help_info() {
	cat <<-EOF
Usage:
epkg install [--env ENV] PACKAGE
epkg remove [--env ENV] PACKAGE
epkg upgrade [PACKAGE]
epkg update

epkg search PACKAGE
epkg list

epkg env list
epkg [env] create|remove ENV
epkg [env] activate ENV
epkg [env] deactivate
epkg [env] register|unregister ENV
epkg [env] history ENV
epkg [env] rollback ENV
EOF
}

cmd=$1
shift

if [[ "$cmd" == "help" ]]; then
	__get_epkg_help_info
	exit 0
elif [[ "$cmd" == "init" ]]; then
	__epkg_init
	exit 0
else
	if ! __check_epkg_user_init; then	
		echo "Warning: epkg has not been initialized, Automatically execute epkg init."
		__epkg_init
		echo "Warning: epkg init finish, Please rerun cmd."
		exit 0
	fi

	echo "EPKG_ACTIVE_ENV: $EPKG_ACTIVE_ENV"
	get_active_env "$@"
	__set_epkg_curr_dir $env
fi

case "$cmd" in
	"install")
		installroot=""
		package_arr=()
		while [[ $# -gt 0 ]];do
			case "$1" in
				--installroot=*)
					installroot="${1#*=}"
					shift
					;;
				*)
					package_arr+=("$1")
					shift
					;;
			esac
		done
		if [ ${#package_arr[@]} -eq 0 ]; then
			echo "No Packages specified." >&2
			exit 1
		fi
		install_package
		;;
	"update")
		cache_repo
		;;
	"upgrade")
		upgrade_package "$@"
		;;
	"search")
		search_package "$@"
		;;
	"show")
		# show_package "$@"
		subcmd=$1
		shift
		case $subcmd in
			"--requires")
				query_requires "$@"
				;;
			"--files")
				show_package_file_list "$@"
				;;
			"-f")
				show_package_file_list "$@"
				;;
			*)
				echo "Usage: epkg show [-f|files|requires|provides(wait...)|whatrequires(wait...)|wahtprovides(wait...)|]"
				;;
		esac
		;;
	"env")
		subcmd=$1
		shift
		case $subcmd in
			"list")
				__epkg_list_environments
				;;
			"create")
				# Check Parameters $#==3 or ($#==5 and $5==--repo)
				if ! { [ $# -eq 1 ] || [ $# -eq 3 -a "$2" = "--repo" ]; }; then
					echo "Usage: epkg env create <env_name> [--repo <repo_name>]"
					exit 1
				fi

				__epkg_create_environment "$@"
				;;
			"remove")
				if [ $# -ne 1 ]; then
					echo "Usage: epkg env remove <env_name>"
					exit 1
				fi

				__epkg_remove_environment "$@"
				;;
			"register")
				if [ $# -ne 1 ]; then
					echo "Usage: epkg env register|unregister <env_name>"
					exit 1
				fi

				__epkg_register_environment "$@"
				;;
			"unregister")
				if [ $# -ne 1 ]; then
					echo "Usage: epkg env register|unregister <env_name>"
					exit 1
				fi

				__epkg_unregister_environment "$@"
				;;
			"activate")
				__epkg_activate_environment "$@"
				;;
			"deactivate")
				__epkg_deactivate_environment "$@"
				;;
			*)
				echo "Usage: epkg env [list|create|remove|register|unregister|activate|deactivate|history|rollback]"
				;;
		esac
		;;

	"repo")
		subcmd=$1
		shift
		case $subcmd in
			"list")
				list_repos
				;;
			*)
				echo "Usage: epkg repo [list]"
				;;
		esac
		;;
	"build")
		run_build "$@"
		;;
	# "localinstall")
	# 	local_install_package "$@"
	# 	;;
	*)
		echo "Usage: epkg [install|remove|upgrade|search|list|init|env|create|remove|register|unregister|activate|deactivate|history|rollback|help]"
		;;
esac

# vim: sw=4 ts=4 et
