# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2020 Huawei Technologies Co., Ltd. All rights reserved.

# why 24.03
# 	match debandance rpm rust version for install rpm
FROM	openeuler/openeuler:24.03

RUN	sed -i 's|http://repo.openeuler.org/|https://mirrors.huaweicloud.com/openeuler/|g' /etc/yum.repos.d/*.repo

RUN	yum clean all && \
	yum makecache && \
	yum -y update

RUN	yum -y install wget zstd rpm cpio
RUN	yum -y install findutils python3-rpm
COPY	batch-convert-rpm2epkg /usr/bin
COPY	convert-rpm2epkg /usr/bin

