Name:           epkg
Version:        0.1.0
Release:        1%{?dist}
Summary:        A new type of software package
License:        MulanPSL-2.0+
URL:            https://gitee.com/openeuler/epkg
Source0:        %{name}-%{version}.tar

Requires:       jq
Requires:       coreutils
Requires:       grep
Requires:       findutils
Requires:       tar
Requires:       file

%description
epkg is a new type of software package format developed by the openEuler community. It supports multiple environments and versions, addressing software package compatibility issues.

%prep
%setup -q

%install
rm -rf $RPM_BUILD_ROOT
mkdir -p $RPM_BUILD_ROOT%{_bindir}
mkdir -p $RPM_BUILD_ROOT%{_libdir}/%{name}
mkdir -p $RPM_BUILD_ROOT%{_sysconfdir}/%{name}

# Create a temporary directory structure
mkdir -p $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/usr/{bin,lib}

# Copy files to the temporary directory
cp -a %{_builddir}/%{name}-%{version}/bin/* $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/usr/bin/
cp -a %{_builddir}/%{name}-%{version}/lib/* $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/usr/lib/
install -m 644 channel.json $RPM_BUILD_ROOT%{_sysconfdir}/%{name}/channel.json

%post
EPKG_HOME=/opt
CURRENT_USER=${SUDO_USER:-root}
mkdir -p "$EPKG_HOME/.epkg/envs/common/profile-1"
cp -R %{_datadir}/%{name}/temp_install/* "$EPKG_HOME/.epkg/envs/common/profile-1/"
ln -sf "$EPKG_HOME/.epkg/envs/common/profile-1/usr/bin/epkg" /bin/epkg
mkdir -p "$EPKG_HOME/.epkg/envs/common/profile-1/etc/epkg/"
cp /etc/epkg/channel.json "$EPKG_HOME/.epkg/envs/common/profile-1/etc/epkg/"
chown -R $CURRENT_USER:$CURRENT_USER "$EPKG_HOME/.epkg"
chmod -R 755 "$EPKG_HOME/.epkg"
curl -s -o $RPM_BUILD_ROOT%{_bindir}/epkg_helper https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg_helper
chmod 4755 $RPM_BUILD_ROOT%{_bindir}/epkg_helper

%postun
EPKG_HOME=/opt
CURRENT_USER=${SUDO_USER:-root}
rm -rf "$EPKG_HOME/.epkg/"
rm -rf "$EPKG_HOME/.cache/epkg/"
rm -rf /opt/.temp/elf-loader
rm -rf /opt/.temp/store.tar.gz
rm -rf /etc/epkg/channel.json
rm -rf /usr/bin/epkg_helper
ALL_USERS=$(getent passwd | awk -F: '$3 >= 1000 {print $1 ":" $6}')
ALL_USERS="$ALL_USERS root:/root"
for USER in $ALL_USERS; do
    IFS=':' read -r user home <<< "$USER"
    rm -rf $home/.epkg/
    bashrc_file="$home/.bashrc"
    if [ -f "$bashrc_file" ]; then
        sed -i '/.epkg/d;' "$bashrc_file"
    fi     
done

%files
%{_datadir}/%{name}
%config(noreplace) %{_sysconfdir}/%{name}/channel.json


%changelog
* Sun Sep 01 2024 duan_pj <pengjieduan@gmail.com> - 0.1.0-1
- Initial package release
