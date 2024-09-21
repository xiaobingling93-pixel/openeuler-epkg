Name:           epkg
Version:        0.1.0
Release:        1%{?dist}
Summary:        A new type of software package
License:        MulanPSL-2.0+
URL:            https://gitee.com/openeuler/epkg
Source0:        %{name}-%{version}.tar

Requires:       jq
Requires:       coreutils
Requires:       patchelf
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
CURRENT_USER=${SUDO_USER:-root}
USER_HOME=$(eval echo ~$CURRENT_USER)
mkdir -p "$USER_HOME/.epkg/envs/common/profile-1"
cp -R %{_datadir}/%{name}/temp_install/* "$USER_HOME/.epkg/envs/common/profile-1/"
ln -sf "$USER_HOME/.epkg/envs/common/profile-1/usr/bin/epkg" /bin/epkg
mkdir -p "$USER_HOME/.epkg/envs/common/profile-1/etc/epkg/"
cp /etc/epkg/channel.json "$USER_HOME/.epkg/envs/common/profile-1/etc/epkg/"
chown -R $CURRENT_USER:$CURRENT_USER "$USER_HOME/.epkg"

%postun
CURRENT_USER=${SUDO_USER:-root}
USER_HOME=$(eval echo ~$CURRENT_USER)
rm -rf "$USER_HOME/.epkg/"
rm -rf "$USER_HOME/.cache/epkg"
rm -rf /etc/epkg/channel.json
sed -i '/.epkg/d; /EPKG_INITIALIZED=yes/d' "$USER_HOME/.bashrc"

%files
%{_datadir}/%{name}
%config(noreplace) %{_sysconfdir}/%{name}/channel.json


%changelog
* Sun Sep 01 2024 duan_pj <pengjieduan@gmail.com> - 0.1.0-1
- Initial package release
