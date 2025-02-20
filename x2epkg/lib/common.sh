#!/bin/bash

epkg_conversion_dir="${HOME}/epkg_conversion"

init_conversion_dirs()
{
rm -rf ${epkg_conversion_dir}/*

mkdir -p ${epkg_conversion_dir}/{fs,info}
mkdir -p ${epkg_conversion_dir}/info/pgp
mkdir -p ${epkg_conversion_dir}/info/install
touch ${epkg_conversion_dir}/info/{package.json,files}
}
