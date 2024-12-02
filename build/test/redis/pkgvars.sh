#!/usr/bin/env bash

name="redis"
version="4.0.14"
release="7"
license="BSD-3-Clause and MIT"
homepage="https://redis.io"
sources=("https://gitee.com/src-openeuler/redis/raw/master/redis-4.0.14.tar.gz" "https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/redis.logrotate" "https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/redis-sentinel.service" "https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/redis.service")
patches=("https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/CVE-2020-14147.patch" "https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/improved-HyperLogLog-cardinality-estimation.patch" "https://gitee.com/rkingkoyo/epkg_test/releases/download/redis-4.0.14/Aesthetic-changes-to-PR.patch")
buildSystem="make"
buildRequires=('make' 'gcc' 'gawk' 'systemd' 'logrotate' 'shadow-utils')