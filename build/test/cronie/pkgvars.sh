#!/usr/bin/env bash

name="cronie"
version="1.7.2"
release="1"
license="GPL-2.0-or-later AND BSD-3-Clause AND BSD-2-Clause AND ISC AND LGPL-2.1-or-later"
homepage="https://github.com/cronie-crond/cronie"
sources=("https://gitee.com/src-openeuler/cronie/raw/master/cronie-1.7.2.tar.gz")
buildSystem="autotools"
buildRequires=('automake' 'autoconf' 'gcc' 'g++' 'make' 'automake' 'autoconf' 'gcc' 'g++' 'make')