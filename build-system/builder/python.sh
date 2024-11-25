#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


python_build() {
  pip install setuptools wheel -i https://mirrors.aliyun.com/pypi/simple/
  pip install -r requirements.txt -i https://mirrors.aliyun.com/pypi/simple/
  python3 setup.py bdist_wheel
}

python_install() {
  rm -rf /opt/buildroot
  mkdir /opt/buildroot
  cp dist/*.whl /opt/buildroot
}
