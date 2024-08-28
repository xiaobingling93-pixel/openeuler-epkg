#!/bin/bash

if [ "$#" -ne 3 ]; then
    echo "Usage: $0 <rpm-package> $1 <output dir>  $2 <store rpms>"
    exit 1
fi

echo "*************Start to generate metadata for $1*****************"
rpm_package="$1"
output_dir="$2"
abnormal_output_dir="$output_dir/wait_for_check"
store_rpms="$3"

json_data=""
rpm_hash=""
rpm_file_name=""
rpm_epoch=""
rpm_version=""
rpm_release=""
rpm_dist=""
rpm_arch=""

has_unknown_requires=0
requires_file=$(mktemp)
provides_file=$(mktemp)
dependencies_file=$(mktemp)

declare -A file_requirements
declare -A so_requirements
declare -A bin_requirements
declare -A requirement_rpm_info
declare -A rpm_provides_info


check_metadata_json_exist() {
    result=$(query_rpm_name $rpm_package)
    IFS=' ' read -r rpm_name file_name epoch version release dist arch<<< $result
    rpm_file_name=$file_name
    rpm_hash=$(sha256sum $store_rpms/$rpm_file_name | awk '{print $1}')
    output_dir="$output_dir/$rpm_hash-$rpm_name-$version-$release.$dist"
    if [[ -f "$output_dir/package.json" ]]; then
        return 1
    fi
    rpm_epoch=$epoch
    rpm_version=$version
    rpm_release=$release
    rpm_dist=$dist
    rpm_arch=$arch
    return 0
}

query_rpm_name() {
    local input_item=$1
    if [[ "/bin/sh" == $input_item ]];then
        input_item="bash"
    fi

    # 查询input_item对应的rpm包名信息，形如：audit-devel-1:3.0.1-11.oe2203sp3.aarch64，需要依次解析各个字段
    full_rpm_name=$(dnf repoquery --whatprovides "$input_item")
    IFS=':' read -r rpm_name_epoch version_release_dist_arch <<< $full_rpm_name
    rpm_name=${rpm_name_epoch%-*}
    epoch=${rpm_name_epoch##*-}
    version=${version_release_dist_arch%-*}
    release_dist_arch=${version_release_dist_arch##*-}
    IFS='.' read -r release dist arch <<< $release_dist_arch
    file_name="$rpm_name-$version_release_dist_arch.rpm"
    echo "$rpm_name $file_name $epoch $version $release $dist $arch"
}

query_requirements() {
    dnf repoquery --requires "$rpm_package" > "$requires_file"
    echo "==============Requirements:"
    cat $requires_file
}

query_provides () {
    if [[ ! -f "$rpm_file_name" ]]; then
        dnf repoquery --provides "$rpm_package" > "$provides_file"
    else
        rpm -qp --provides $store_rpms/$rpm_file_name > $provides_file
    fi
    echo "===============Provides:"
    cat $provides_file
}

download_input_rpm () {
    dnf download --destdir=$store_rpms $rpm_package 2>/dev/null
}

classify_requirements () {
    while read -r requirement; do
        requirement="${requirement%% [=<>]*}"
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
    # 考虑有这种require场景：(docker-runc or runc)
    if [[  "$requirement" == *" or "* ]];then
        echo "----------Requirement contains or: $requirement"
        cleaned="${requirement//(/}"
        cleaned="${cleaned//)/}"
        IFS=' ' read -r -a requirement_array <<< "$cleaned"
    else
        requirement_array=("$requirement")
    fi
    
    for element in "${requirement_array[@]}"; do
        if [[ $element != "or" ]];then
            result=$(query_rpm_name "$element")
            IFS=' ' read -r rpm_name file_name epoch version release dist arch<<< $result
            if [ -n "$file_name" ];then
                dnf download --dest=$store_rpms $rpm_name 2>/dev/null
                if [[ ! -f "$store_rpms/$file_name" ]]; then
                    echo "-----------Warning: no rpm found for $rpm_name"
                    continue
                fi
                sha256=$(sha256sum $store_rpms/$file_name | awk '{print $1}')
                echo "get sha256 for $rpm_name: $sha256"
                requirement_rpm_info[$sha256]+="$rpm_name|$type|$element "
                return
            fi
        fi
    done
    if [[ $requirement_rpm_info[$sha256] == "" ]];then
        requirement_rpm_info["unknown"]+="unknown|$type|$requirement "
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
    local rpm_hash="$1"
    local data="$2"
    IFS=' ' read -r -a entries <<< "$data"
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
        --arg rpm_hash "$rpm_hash" \
        --arg pkgname "$pkgname" \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            requires: {
                ($rpm_hash): {
                    pkgname: $pkgname,
                    files: $files,
                    sonames: $sonames,
                    binaries: $binaries
                }
            }
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
    # 创建 JSON 对象
    local json=$(jq -n \
        --arg name "$rpm_package" \
        --arg hash "$rpm_hash" \
        --arg epoch "$rpm_epoch" \
        --arg version "$rpm_version" \
        --arg release "$rpm_release" \
        --arg dist "$rpm_dist" \
        --arg arch "$rpm_arch" \
        '{
            package: {
                    name: $name,
                    hash: $hash,
                    epoch: $epoch,
                    version: $version,
                    release: $release,
                    dist: $dist,
                    arch: $arch
            }
        }'
    )
    json_data=$json
}

generate_metadata_json () {
    output_json=$(jq -n '{}')

    # 获取并解析rpm包信息
    convert_package_info_to_json
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    # 获取并解析requires信息
    for key in "${!requirement_rpm_info[@]}"; do
        data=${requirement_rpm_info[$key]}
        convert_requiremennts_to_json "$key" "${requirement_rpm_info[$key]}"
        output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')
    done

    # 获取并解析provides信息
    convert_provides_to_json
    output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')

    output_file="package.json"
    echo "$output_json" | jq '.' > "$output_file"
    echo "$output_json" | jq '.'
}

restore_metadata_json() {
    if [[ "$has_unknown_requires" -eq 1 ]];then
        abnormal_output_dir="$abnormal_output_dir/$rpm_package"
        if [ ! -d "$abnormal_output_dir" ]; then
            mkdir -p "$abnormal_output_dir"
        fi
        mv "package.json" $abnormal_output_dir
        echo "---------Get abnormal requires for $rpm_package, move json to $abnormal_output_dir"
        echo "$rpm_package" >> ./need_check
        return
    fi
    if [ ! -d "$output_dir" ]; then
        mkdir -p "$output_dir"
    fi
    mv "package.json" $output_dir
    echo "********JSON has been moved to $output_dir**********"
}

clean_tmp_files() {
    rm "$requires_file" "$dependencies_file" "$provides_file"
    unset file_requirements
    unset so_requirements
    unset bin_requirements
    unset requirement_rpm_info
    unset rpm_provides_info    
}

# step 1 download rpm
download_input_rpm

# step 2 check rpm's sha256 and package.json existed or not
check_metadata_json_exist
status=$?

if [[ $status -eq 0 ]]; then
    echo "-----------get sha256 for $rpm_package: $rpm_hash"
else
    echo "==========$output_dir/package.json already existed"
    exit 0
fi


# step 2 query original requires and provides info of rpm
query_requirements
query_provides
echo "========Query original requires and provides info Done========"

# step 3 
classify_requirements
echo "========Classify original requires info Done========"

# step 4
init_requirement_rpm_info
init_rpm_provides_info_info
echo "========Turn original requires and provides info to array Done========"

# step 5
generate_metadata_json
echo "========Json has been generated========="

# step 6
restore_metadata_json

#step 7 clean
clean_tmp_files
