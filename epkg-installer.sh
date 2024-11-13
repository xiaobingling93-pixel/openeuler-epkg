#!/usr/bin/env bash

# XXX: only touch bashrc epkg()

# download file
EPKG_URL=https://repo.oepkgs.net/openeuler/epkg/rootfs/
EPKG_MANAGER_TAR=epkg_manager.tar.gz
EPKG_HELPER=epkg_helper
# epkg base path
OPT_EPKG=/opt/epkg
HOME_EPKG=$HOME/.epkg
PUB_EPKG=$OPT_EPKG/users/public
# epkg mode-based path
EPKG_INSTALL_MODE=
EPKG_CACHE=
EPKG_COMMON_ROOT=
EPKG_MANAGER_DIR=
# shell type
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
	# XXX: only prompt for root user
    echo "Attention: Select the installation mode"
    echo "1: global mode: epkg common and store will be installed in the /opt/epkg/, requires root user"
    echo "2: user   mode: epkg will be installed in the $HOME/.epkg/"
    read choice
    if [[ "$choice" == "1" ]]; then
        EPKG_INSTALL_MODE="global"
        EPKG_CACHE=$OPT_EPKG/cache
        EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
    elif [[ "$choice" == "2" ]]; then
        EPKG_INSTALL_MODE="user"
        EPKG_CACHE=$HOME/.cache/epkg
        EPKG_COMMON_ROOT=$HOME_EPKG/envs/common
    else
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
    echo "Attention: Need 150M space to download and unpack tars to $EPKG_CACHE"
    echo "sure to continue? (y: continue, others: exit)"
    read choice
    if [ "$choice" != "y" ]; then
        return 1
    fi

    # download epkg_manager    
    curl -o $EPKG_CACHE/$EPKG_MANAGER_TAR $EPKG_URL/$EPKG_MANAGER_TAR

    # download epkg_helper in global mode
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        curl -o $EPKG_CACHE/$EPKG_HELPER $EPKG_URL/$EPKG_HELPER
    fi
    
    return 0
}

epkg_unpack() {
    # unpack epkg_manager
    tar -xvf $EPKG_CACHE/$EPKG_MANAGER_TAR -C $EPKG_CACHE > /dev/null
    cp -r $EPKG_MANAGER_DIR/bin/epkg $EPKG_COMMON_ROOT/profile-1/usr/bin/
	cp -r $EPKG_MANAGER_DIR/lib/epkg $EPKG_COMMON_ROOT/profile-1/usr/lib/
	cp -r $EPKG_MANAGER_DIR/channel.json $EPKG_COMMON_ROOT/profile-1/etc/epkg

    # unpack epkg_helper
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        cp -r $EPKG_CACHE/$EPKG_HELPER $EPKG_COMMON_ROOT/profile-1/usr/bin/
        chmod -R 755 $OPT_EPKG
        chmod 4755 $EPKG_COMMON_ROOT/profile-1/usr/bin/epkg_helper
    else
        chown -R $USER:$USER $HOME_EPKG
        chown -R 755 $HOME_EPKG
    fi
    return 0
}

change_bashrc() {
    
    return 0
}

# XXX: assume has tar/coreutils; detect use curl/wget
# XXX: use self contained tools
# XXX: no install
install_needed_tools() {
    local package_name="jq tar file grep findutils coreutils util-linux"
    if rpm -q $package_name >/dev/null 2>&1; then
        return 0
    fi

    local max_retries=3
    local retry_interval=5
    local download_timeout=60
    local retry_count=0

    echo "Attention: $package_name were needed for initialization"
    echo "sure to continue? (y: continue, others: exit)"
    read choice
    if [ "$choice" != "y" ]; then
        return 1
    fi

    while ((retry_count < max_retries)); do
        if timeout ${download_timeout}s yum install -y $package_name; then
            echo "Package $package_name installed successfully on attempt $((retry_count+1))"
            return 0
        else
            echo "Installation failed on attempt $((retry_count+1)). Retrying after $retry_interval seconds..."
            ((retry_count++))
            sleep $retry_interval
        fi
    done

    echo "Failed to install package $package_name after $max_retries attempts."
    return 1
}

echo "Execute user: $USER"
# step 0. select installation mode
select_installation_mode
echo "Directories $EPKG_CACHE and $EPKG_COMMON_ROOT will be created."

# step 1. mk path
mk_home

# step 2. install needed tools
install_needed_tools

# step 3. download - unpack - change bashrc
epkg_download
epkg_unpack
change_bashrc
if [ $? -ne 0 ]; then
    exit 1
fi
