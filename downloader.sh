#!/usr/bin/env bash

EPKG_USER=${1:-$USER}
EPKG_TARS_PATH=/tmp/$EPKG_USER
EPKG_USER_HOME=/root
URL=https://repo.oepkgs.net/openeuler/epkg/rootfs/
EPKG_MANAGER_TAR=epkg_manager.tar.gz
EPKG_INITIAL_SH=epkg_initial.sh
EPKG_HELPER=epkg_helper
EPKG_INSTALL_MODE=user

create_epkg_user() {
    echo "Attention: Select the installation mode (global: /opt/.epkg, user: $HOME/.epkg, other: $HOME/.epkg)"
    read choice
    if [ "$choice" == "global" ]; then
        EPKG_USER_HOME=/opt
        EPKG_INSTALL_MODE=global
    else
       if [ $EPKG_USER != root ]; then
            EPKG_USER_HOME=/home/$EPKG_USER
        fi
    fi
}

mk_home() {
    echo "Attention: Two tars will be saved in $EPKG_TARS_PATH"
    echo "sure to continue? (y: continue, others: exit)"
    read choice
    if [ "$choice" != "y" ]; then
        return
    fi
    mkdir -p $EPKG_TARS_PATH
    echo $EPKG_USER_HOME/.epkg/envs/common/profile-1/usr/
    mkdir -p $EPKG_USER_HOME/.epkg/envs/common/profile-1/usr/{bin,lib}
    mkdir -p $EPKG_USER_HOME/.epkg/envs/common/profile-1/etc/epkg
    ln -sT profile-1 $EPKG_USER_HOME/.epkg/envs/common/profile-current
}

download_and_unpack() {
    echo "Attention: Need 150M space to download and unpack tars to $EPKG_TARS_PATH"
    echo "sure to continue? (y: continue, others: exit)"
    read choice
    if [ "$choice" != "y" ]; then
        return 1
    fi

    if [ ! -f /tmp/$EPKG_INITIAL_SH ]; then
        curl -o /tmp/$EPKG_INITIAL_SH $URL/$EPKG_INITIAL_SH
    fi
    cp /tmp/$EPKG_INITIAL_SH $EPKG_USER_HOME

    if [ ! -f /tmp/$EPKG_MANAGER_TAR ]; then
        curl -o /tmp/$EPKG_MANAGER_TAR $URL/$EPKG_MANAGER_TAR
    fi
    tar -xvf /tmp/$EPKG_MANAGER_TAR -C $EPKG_TARS_PATH > /dev/null

    if [ $EPKG_USER != root ]; then
        chown -R $EPKG_USER:$EPKG_USER $EPKG_TARS_PATH
    fi
	cp -r $EPKG_TARS_PATH/epkg_manager/bin/epkg $EPKG_USER_HOME/.epkg/envs/common/profile-1/usr/bin/
	rm -rf /bin/epkg
	ln -sT  $EPKG_USER_HOME/.epkg/envs/common/profile-1/usr/bin/epkg /bin/epkg
	cp -r $EPKG_TARS_PATH/epkg_manager/lib/epkg $EPKG_USER_HOME/.epkg/envs/common/profile-1/usr/lib/
	cp -r $EPKG_TARS_PATH/epkg_manager/channel.json $EPKG_USER_HOME/.epkg/envs/common/profile-1/etc/epkg
    chown -R $EPKG_USER:$EPKG_USER $EPKG_USER_HOME/.epkg
    chown $EPKG_USER:$EPKG_USER $EPKG_USER_HOME/$EPKG_INITIAL_SH

    if [ "$EPKG_INSTALL_MODE" == global ]; then
        echo "downloading epkg_helper ..."
        if [ ! -f /tmp/$EPKG_HELPER ]; then
            curl -o /tmp/$EPKG_HELPER $URL/$EPKG_HELPER
        fi
        cp /tmp/$EPKG_HELPER /usr/bin

        chmod 4755 /usr/bin/epkg_helper
        chmod 755 /opt/.epkg
    fi

    return 0
}

install_needed_tools() {
    local package_name="jq tar file grep patchelf findutils coreutils util-linux"
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

echo "Execute user: $USER, initail epkg user: $EPKG_USER"
# step 0. create nonroot user
create_epkg_user

# step 1. mk path to save tar files
mk_home

# step 2. install needed tools
install_needed_tools

# step 3. download epkg_manager.tar.gz and epkg_rootfs.tar.gz
download_and_unpack
if [ $? -ne 0 ]; then
    exit 1
fi