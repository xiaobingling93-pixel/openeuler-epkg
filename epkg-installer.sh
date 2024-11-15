#!/usr/bin/env bash

# Download File
EPKG_URL=https://repo.oepkgs.net/openeuler/epkg/rootfs/
EPKG_MANAGER_TAR=epkg_manager.tar.gz
EPKG_HELPER=epkg_helper
# Global Epkg Path - Only Global Mode Use
OPT_EPKG=/opt/epkg
PUB_EPKG=$OPT_EPKG/users/public
# User Epkg Path
HOME_EPKG=$HOME/.epkg
# Epkg Mode-based Path
EPKG_INSTALL_MODE=
EPKG_CACHE=
EPKG_COMMON_ROOT=
EPKG_MANAGER_DIR=
BASHRC_FILE=
# Shell Type
shell=$(basename "$SHELL")
case "$shell" in
	"bash")
		RC_PATH=$HOME/.bashrc
		;;
	"zsh")
		RC_PATH=$HOME/.zshrc
		;;
	*)
		echo "Unsupported shell: $shell"
		exit 1
		;;
esac

select_installation_mode() {
    echo "Attention: Execute by $USER, Select the installation mode"
    echo "1: user   mode: epkg will be installed in the $HOME/.epkg/"
    echo "2: global mode: epkg common and store will be installed in the /opt/epkg/, requires root user"
    read choice
    if [[ "$choice" == "1" ]]; then
        EPKG_INSTALL_MODE="user"
        EPKG_CACHE=$HOME/.cache/epkg
        EPKG_COMMON_ROOT=$HOME_EPKG/envs/common
        BASHRC_FILE=$HOME/.bashrc
    elif [[ "$choice" == "2" && "$(id -u)" = "0" ]]; then
        EPKG_INSTALL_MODE="global"
        EPKG_CACHE=$OPT_EPKG/cache
        EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
        BASHRC_FILE=/etc/bashrc
    elif [[ "$choice" == "2" && "$(id -u)" != "0" ]]; then
        echo "Attention: Please use the root user to execute the global installation mode"
        return 1
    else
        echo "Error choice !"
        return 1
    fi
    EPKG_MANAGER_DIR=$EPKG_CACHE/epkg_manager
}

mk_home() {
    mkdir -p $EPKG_CACHE
    mkdir -p $EPKG_MANAGER_DIR
    mkdir -p $EPKG_COMMON_ROOT/profile-1/usr/{bin,lib}
    mkdir -p $EPKG_COMMON_ROOT/profile-1/etc/epkg
}

epkg_download() {
    # download epkg_manager    
    curl -o $EPKG_CACHE/$EPKG_MANAGER_TAR $EPKG_URL/$EPKG_MANAGER_TAR

    # download epkg_helper in global mode
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        curl -o $EPKG_CACHE/$EPKG_HELPER $EPKG_URL/$EPKG_HELPER
    fi
}

epkg_unpack() {
    # unpack epkg_manager
    tar -xvf $EPKG_CACHE/$EPKG_MANAGER_TAR -C $EPKG_CACHE > /dev/null
    cp -r $EPKG_MANAGER_DIR/bin/epkg $EPKG_COMMON_ROOT/profile-1/usr/bin/
	cp -r $EPKG_MANAGER_DIR/lib/epkg $EPKG_COMMON_ROOT/profile-1/usr/lib/
	cp -r $EPKG_MANAGER_DIR/channel.json $EPKG_COMMON_ROOT/profile-1/etc/epkg

    # unpack epkg_helper
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        /bin/cp -rf $EPKG_CACHE/$EPKG_HELPER $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
        chown -R $USER:$USER $OPT_EPKG
        chmod -R 755 $OPT_EPKG
        chmod 4755 $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
    else
        chown -R $USER:$USER $HOME_EPKG
        chmod -R 755 $HOME_EPKG
    fi
}

epkg_change_bashrc() {
    cat << EOF >> $BASHRC_FILE
# epkg begin
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	EPKG_COMMON_ROOT=/opt/epkg/users/public/envs/common
else
	EPKG_COMMON_ROOT=\$HOME/.epkg/envs/common
fi
source \$EPKG_COMMON_ROOT/profile-current/usr/lib/epkg/epkg-rc.sh
# epkg end
EOF
}

has_cmd()
{
	command -v "$1" >/dev/null
}

# TODO: assume has tar/coreutils; detect use curl/wget, use self contained tools
dependency_check() {
    local cmd_names="id tar cat cp chmod chown curl"
    local cmd
    local missing_cmds=

    for cmd in $cmd_names; do
        if ! has_cmd $cmd; then
            missing_cmds="$missing_cmds $pkg"
        fi
    done

    if [[ -n "$missing_cmds" ]]; then
        echo "Commands '$missing_cmds' not found, please install first"
        return 1
    fi

    return 0
}

# step 0. dependency check
dependency_check
if [ $? -ne 0 ]; then
    exit 1
fi

# step 1. select installation mode
select_installation_mode
if [ $? -ne 0 ]; then
    exit 1
fi
echo "Attention: Directories $EPKG_CACHE and $PUB_EPKG will be created."
echo "Attention: File $BASHRC_FILE will be modified."
mk_home

# step 2. download - unpack - change bashrc
epkg_download
epkg_unpack
epkg_change_bashrc

echo "Attention: For changes to take effect, close and re-open your current shell.."
