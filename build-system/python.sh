#!/usr/bin/env bash


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
