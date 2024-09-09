Name:           epkg
Version:        0.1.0
Release:        1%{?dist}
Summary:        A new type of software package
License:        MulanPSL-2.0+
URL:            https://gitee.com/openeuler/epkg
Source0:        %{name}-%{version}.tar.gz

Requires:       jq
Requires:       coreutils
Requires:       patchelf
Requires:       grep
Requires:       findutils

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
mkdir -p $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/{bin,lib}

# Copy files to the temporary directory
cp -a %{_builddir}/%{name}-%{version}/bin/* $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/bin
cp -a %{_builddir}/%{name}-%{version}/lib/* $RPM_BUILD_ROOT%{_datadir}/%{name}/temp_install/lib
install -m 644 channel.json $RPM_BUILD_ROOT%{_sysconfdir}/%{name}/


%post
CURRENT_USER=${SUDO_USER:-root}
USER_HOME=$(eval echo ~$CURRENT_USER)
mkdir -p "$USER_HOME/.epkg/envs/common/profile-1"
cp -R %{_datadir}/%{name}/* "$USER_HOME/.epkg/envs/common/profile-1/"
chown -R $CURRENT_USER:$CURRENT_USER "$USER_HOME/.epkg"
rm -rf %{_datadir}/%{name}/temp_install

%files
%{_datadir}/%{name}
%config(noreplace) %{_sysconfdir}/%{name}/channel.json


%changelog
* Sun Sep 01 2023 2024 duan_pj <pengjieduan@gmail.com> - 0.1.0-1
- Initial package release
