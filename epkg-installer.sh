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
    elif [[ "$choice" == "2" && "$(id -u)" = "0" ]]; then
        EPKG_INSTALL_MODE="global"
        EPKG_CACHE=$OPT_EPKG/cache
        EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
    elif [[ "$choice" == "2" && "$(id -u)" != "0" ]]; then
        echo "Please use the root user to execute the global installation mode"
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
        # TODO: temp cp ->  only touch bashrc epkg()
        cp $EPKG_CACHE/$EPKG_HELPER /usr/bin
        chmod 4755 /usr/bin/epkg_helper
    else
        chown -R $USER:$USER $HOME_EPKG
        chmod -R 755 $HOME_EPKG
    fi
    # TODO: temp ln  ->  only touch bashrc epkg()
    rm -rf /bin/epkg
    ln -sT $EPKG_COMMON_ROOT/profile-1/usr/bin/epkg /bin/epkg
    return 0
}

change_bashrc() {
    
    return 0
}

# TODO: assume has tar/coreutils; detect use curl/wget, use self contained tools
dependency_check() {
    local package_name="jq tar file grep findutils coreutils util-linux"
    local missing_packages=
    for pkg in $package_name; do
        if ! rpm -q $pkg >/dev/null 2>&1; then
            missing_packages="$missing_packages $pkg"
        fi
    done

    if [[ ! -z "$missing_packages" ]]; then
        echo "packages $missing_packages not found, please install "
        return 1
    fi

    return 0
}

# step 0. select installation mode
select_installation_mode
if [ $? -ne 0 ]; then
    exit 1
fi
echo "Directories $EPKG_CACHE and $EPKG_COMMON_ROOT will be created."

# step 1. mk path
mk_home

# step 2. dependency check
dependency_check
if [ $? -ne 0 ]; then
    exit 1
fi

# step 3. download - unpack - change bashrc
epkg_download
epkg_unpack
change_bashrc
if [ $? -ne 0 ]; then
    exit 1
fi
