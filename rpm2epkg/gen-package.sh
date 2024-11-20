#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

SCRIPT_DIR=$(dirname "$(readlink -f "$0")")
source "$SCRIPT_DIR/../lib/epkg/hash.sh"

if [ "$#" -ne 3 ]; then
    echo "Usage: $0 <rpm-package> $1 <output dir>  $2 <store rpms>"
    exit 1
fi

echo "*************Start to generate metadata for $1*****************"
rpm_package="$1"
output_dir="$2"
output_parent_dir=$2
abnormal_output_dir="$2/wait_for_check"
store_rpms="$3"
json_data=""
full_rpm_names=()
has_unknown_requires=0


query_rpm_names() {
    local input_item=$rpm_package

    epoch=$(rpm -qp --qf %{epoch} "$input_item" 2>/dev/null)
    if [[ "$epoch" == "(none)" ]]; then
        epoch="0"
    fi
    rpm_names=$(rpm -qp --qf '%{NAME}-'$epoch':%{version}-%{release}.%{arch}' "$input_item" 2>/dev/null)
    # rpm_names=$(dnf repoquery --whatprovides "$input_item_noext" 2>/dev/null)

    IFS=$'\n' read -r -d '' -a full_rpm_names <<< "$rpm_names"
}

query_require_hash() {
    local pkg_name=$1
    local search_dir=$2
    find "$search_dir" -maxdepth 1 -mindepth 1 -type d -name "*$pkg_name*"| while read -r dir; do
        # ebe594c852e852f774472fa73aca86f4ac30c7ea43db9cf9055550d5357c92db-fftw-libs-3.3.8-11.oe2203sp3
        dir_name=$(basename "$dir")
        hash=${dir_name%%-*}
        dir_name=${dir_name%.*}
        dir_name=${dir_name%-*}
        dir_name=${dir_name%-*}
        epkg_name=${dir_name#*-}
        if [[ $epkg_name == "$pkg_name" ]]; then
            echo "$hash"
            return
        fi
    done
    echo ""
}

query_rpm_name() {
    local input_item=$1
    if [[ "/bin/sh" == $input_item ]];then
        input_item="bash"
    fi

    # 查询input_item对应的rpm包名信息，形如：audit-devel-1:3.0.1-1.1.oe2203sp3.aarch64，需要依次解析各个字段
    local full_rpm_name=$(dnf repoquery --whatprovides "$input_item" 2>/dev/null)
    IFS=':' read -r rpm_name_epoch version_release_dist_arch <<< $full_rpm_name
    rpm_name=${rpm_name_epoch%-*}
    epoch=${rpm_name_epoch##*-}
    version=${version_release_dist_arch%-*}
    release_dist_arch=${version_release_dist_arch##*-}
    arch=${release_dist_arch##*.}
    release_dist=${release_dist_arch%.*}
    dist=${release_dist##*.}
    release=${release_dist%.*}
    file_name="$rpm_name-$version_release_dist_arch.rpm"
    echo "$rpm_name $file_name $epoch $version $release $dist $arch"
}

query_requirements() {
    local package=$1
    dnf repoquery --requires "$package" > "$requires_file" 2>/dev/null
    echo "==============Requirements:"
    cat $requires_file
}

query_provides () {
    local package_file_name=$1
    local package=$2
    if [[ ! -f "$package_file_name" ]]; then
        dnf repoquery --provides "$package" > "$provides_file" 2>/dev/null
    else
        rpm -qp --provides $store_rpms/$package_file_name > $provides_file
    fi
    # rpm -ql $store_rpms/$package_file_name >> $provides_file
    echo "===============Provides:"
    cat $provides_file
}
 
cp_input_rpm () {
    # local package=$1
    # dnf download --destdir=$store_rpms $package 2>/dev/null
    cp $rpm_package "$store_rpms"
}

classify_requirements () {
    while read -r requirement; do
        if [[ "$requirement" =~ \.so ]]; then
            so_requirements["$requirement"]=1
	        echo "catch so requirement: $requirement"
        elif [[ "$requirement" =~ / ]]; then
            file_requirements["$requirement"]=1
	        echo "catch file requirement: $requirement"
        else
            bin_requirements["$requirement"]=1
	        echo "catch binary requirement: $requirement"
        fi
    done < "$requires_file"
}

update_requirement_checksum () {
    local requirement=$1
    local type=$2
    local requirement_array
    local sha256=""
    local valid_check_sum="no"
    # case1：(docker-runc or runc)
    if [[  "$requirement" == *" or "* ]];then
        echo "----------Requirement contains or: $requirement"
        cleaned="${requirement/(/}"
        cleaned="${cleaned%\)*}"
        IFS=' ' read -r -a requirement_array <<< "$cleaned"
    # case2：(npm(async) >= 1.5.0 with npm(async) < 2)
    elif [[  "$requirement" == *" with "* ]];then
        cleaned="${requirement/(/}"
        cleaned="${cleaned%\)*}"
        split_strings=$(echo "$cleaned" | awk -F ' with ' '{print $1 ";" $2}')
        IFS=';' read -r -a requirement_array <<< "$split_strings"
    # case3: (tpm2-abrmd-selinux >= 2.3.3-2 if selinux-policy)
    elif [[  "$requirement" == *" if "* ]];then
        echo "----------Requirement contains if: $requirement, return"
        return
    else
        requirement_array=("$requirement")
    fi
    
    for element in "${requirement_array[@]}"; do
        if [[ $element != "or" ]];then
            result=$(query_rpm_name "$element")
            echo "query rpm info for $element: $result"
            IFS=' ' read -r rpm_name file_name epoch version release dist arch<<< $result
            if [ -n "$file_name" ];then
                sha256=$(query_require_hash $rpm_name $output_parent_dir)
                if [[ -n "$sha256" ]];then
                    echo "----------hash from existed package.json"
                    valid_check_sum="yes"
                else
                    dnf download --dest=$store_rpms $rpm_name-$version-$release.$dist 2>/dev/null
                    if [[ ! -f "$store_rpms/$file_name" ]]; then
                        echo "-----------Warning: no rpm found for $rpm_name"
                        continue
                    fi
                    # sha256=$(sha256sum $store_rpms/$file_name | awk '{print $1}')
                    sha256=$(rpm_hash "${store_rpms}/${file_name}")
                    valid_check_sum="yes"
                fi
                echo "$rpm_name: $sha256"
                requirement_rpm_info[$sha256]+="$rpm_name|$type|$element;"
                return
            fi
        fi
    done
    if [[ $valid_check_sum == "no" ]];then
        requirement_rpm_info["unknown"]+="unknown|$type|$requirement;"
        has_unknown_requires=1
    fi    
}

init_rpm_provides_info_info () {
    while read -r provide; do
        if [[ "$provide" =~ \.so ]]; then
            rpm_provides_info["$provide"]="soname"
        elif [[ "$provide" =~ / ]]; then
            rpm_provides_info["$provide"]="file"
        else
            rpm_provides_info["$provide"]="binary"
        fi
    done < "$provides_file"
}

init_requirement_rpm_info () {
    requirement_rpm_info["unknown"]=""
    for requirement in "${!file_requirements[@]}"; do
        update_requirement_checksum "$requirement" file
    done
    for requirement in "${!so_requirements[@]}"; do
        update_requirement_checksum "$requirement" soname
    done
    for requirement in "${!bin_requirements[@]}"; do
        update_requirement_checksum "$requirement" binary
    done
}

convert_requiremennts_to_json () {
    local package_hash="$1"
    local data="$2"
    IFS=';' read -r -a entries <<< "$data"
    local pkgname=${entries[0]%%|*}
    local files=()
    local sonames=()
    local binaries=()
    for entry in "${entries[@]}"; do
        IFS='|' read -r pkgname category value <<< "$entry"
	    echo "convert_requiremennts_to_json: $pkgname  $category   $value"
        case "$category" in
            "file")
                files+=("${value}")
                ;;
            "soname")
                sonames+=("${value}")
                ;;
            "binary")
                binaries+=("${value}")
                ;;
        esac
    done

    # 创建 JSON 对象
    local json=$(jq -n \
        --arg package_hash "$package_hash" \
        --arg pkgname "$pkgname" \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            requires: [
                {
                    hash: $package_hash,
                    pkgname: $pkgname,
                    files: $files,
                    sonames: $sonames,
                    binaries: $binaries
                }
            ]
        }'
    )
    json_data=$json
}

convert_provides_to_json () {
    local files=()
    local sonames=()
    local binaries=()

    for provide in "${!rpm_provides_info[@]}"; do
        type=${rpm_provides_info[$provide]}
        echo "convert_provides_to_json provide: $provide; type: $type"
        case "$type" in
            "file")
                files+=("${provide}")
                ;;
            "soname")
                sonames+=("${provide}")
                ;;
            "binary")
                binaries+=("${provide}")
                ;;
        esac
    done

    # 创建 JSON 对象
    local json=$(jq -n \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            provides: {
                    files: $files,
                    sonames: $sonames,
                    binaries: $binaries
            }
        }'
    )
    json_data=$json
}

convert_package_info_to_json () {
    echo "$@"
    local package=$1
    local package_hash=$2
    local package_epoch=$3
    local package_version=$4
    local package_release=$5
    local package_dist=$6
    local package_arch=$7
    # IFS=' ' read -r package package_hash package_epoch package_version package_release package_dist package_arch<<< $result
    # 创建 JSON 对象
    local json=$(jq -n \
        --arg name "$package" \
        --arg hash "$package_hash" \
        --arg epoch "$package_epoch" \
        --arg version "$package_version" \
        --arg release "$package_release" \
        --arg dist "$package_dist" \
        --arg arch "$package_arch" \
        '{
            name: $name,
            hash: $hash,
            epoch: $epoch,
            version: $version,
            release: $release,
            dist: $dist,
            arch: $arch,
            requires: []
        }'
    )
    json_data=$json
}

generate_metadata_json () {
    local package_elements=$@
    output_json=$(jq -n '{}')

    # 获取并解析rpm包信息
    convert_package_info_to_json $package_elements
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    # 获取并解析requires信息
    if [[ "$has_unknown_requires" -eq 0 ]];then
        unset requirement_rpm_info["unknown"]
    fi
    for key in "${!requirement_rpm_info[@]}"; do
        data=${requirement_rpm_info[$key]}
        convert_requiremennts_to_json "$key" "${requirement_rpm_info[$key]}"
        output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '.requires += $new_obj.requires')
    done

    # 获取并解析provides信息
    convert_provides_to_json
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    output_file="package.json"
    echo "$output_json" | jq '.' > "$output_file"
    echo "$output_json" | jq '.'
}

restore_metadata_json() {
    local package=$1
    local restore_dir=$2
    if [[ "$has_unknown_requires" -eq 1 ]];then
        abnormal_output_dir="$abnormal_output_dir/$package"
        if [ ! -d "$abnormal_output_dir" ]; then
            mkdir -p "$abnormal_output_dir"
        fi
        mv "package.json" $abnormal_output_dir
        echo "---------Get abnormal requires for $package, move json to $abnormal_output_dir"
        echo "$package" >> ./need_check
        return
    fi
    if [ ! -d "$restore_dir" ]; then
        mkdir -p "$restore_dir"
    fi
    mv "package.json" $restore_dir
    echo "********JSON has been moved to $restore_dir**********"
}

clean_tmp_files() {
    rm "$requires_file" "$dependencies_file" "$provides_file"
    unset file_requirements
    unset so_requirements
    unset bin_requirements
    unset requirement_rpm_info
    unset rpm_provides_info    
    unset rpm_hashs
}

process_all_rpms() {
    for element in "${full_rpm_names[@]}"; do
        echo "=======================process $element"
        has_unknown_requires=0
        requires_file=$(mktemp)
        provides_file=$(mktemp)
        dependencies_file=$(mktemp)

        declare -A file_requirements
        declare -A so_requirements
        declare -A bin_requirements
        declare -A requirement_rpm_info
        declare -A rpm_provides_info
        declare -A rpm_hashs

        IFS=':' read -r rpm_name_epoch version_release_dist_arch <<< $element
        package=${rpm_name_epoch%-*}
        package_epoch=${rpm_name_epoch##*-}
        package_version=${version_release_dist_arch%-*}
        release_dist_arch=${version_release_dist_arch##*-}
        package_arch=${release_dist_arch##*.}
        release_dist=${release_dist_arch%.*}
        package_dist=${release_dist##*.}
        package_release=${release_dist%.*}
        package_file_name="$package-$version_release_dist_arch.rpm"
        echo "$package $package_file_name $package_epoch $package_version $package_release $package_dist $package_arch"
        # 暂停执行，等待用户输入
        
        # step 1 download rpm
        cp_input_rpm

        # step 2 check rpm's sha256 and package.json existed or not
        # package_hash=$(sha256sum $store_rpms/$package_file_name | awk '{print $1}')
        package_hash=$(rpm_hash $store_rpms/$package_file_name)
        echo $package_hash
    
        # store_dir="$output_dir/$package_hash"__"$package"__"$package_version"__"$package_release"."$package_dist"
        store_dir="$output_dir"
        if [[ -f "$store_dir/package.json" ]]; then
            echo "==========$store_dir/package.json already existed"
            rm "$store_dir/package.json"
        fi

        # step 3 query original requires and provides info of rpm
        query_requirements $package
        query_provides $package_file_name $package
        echo "========Query original requires and provides info Done========"

        # step 3 
        classify_requirements
        echo "========Classify original requires info Done========"

        # step 4
        init_requirement_rpm_info
        init_rpm_provides_info_info
        echo "========Turn original requires and provides info to array Done========"

        # step 5
        package_elements="$package $package_hash $package_epoch $package_version $package_release $package_dist $package_arch"
        generate_metadata_json $package_elements
        echo "========Json has been generated========="

        # step 6
        restore_metadata_json $package $store_dir

        #step 7 clean
        clean_tmp_files
    done
}

# step 1: check rpm exist or not
query_rpm_names
if [[ $status -eq 0 ]]; then
    echo "-----------Found rpms for $rpm_package:"
    for element in "${full_rpm_names[@]}"; do
        echo "$element"
    done
else
    echo "==========Warning: $rpm_package is abnormal"
    exit 0
fi


process_all_rpms
