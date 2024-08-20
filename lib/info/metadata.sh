#!/bin/bash
json_data=""

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-package>"
    exit 1
fi

echo "Start to generate metadata for $1"
rpm_package="$1"
requires_file=$(mktemp)
dependencies_file=$(mktemp)
json_file=$(mktemp)
declare -A file_deps
declare -A so_deps
declare -A bin_deps
declare -A rpm_info

download_input_rpm() {
    dnf download $rpm_package
}

query_requirements() {
    dnf repoquery --requires "$rpm_package" > "$requires_file"
    cat $requires_file
}

classify_deps() {
    while read -r dep; do
    echo "read dep: $dep"
    if [[ "$dep" =~ \.so ]]; then
        echo "catch soname: $dep"
        so_deps["$dep"]=1
    elif [[ "$dep" =~ / ]]; then
        echo "catch file: $dep"
        file_deps["$dep"]=1
    else
        echo "catch binary: $dep"
        bin_deps["$dep"]=1
    fi
    done < "$requires_file"
}

get_sha256() {
    local rpm_name="$1"
    rpm_file_name=$(ls | grep "^${rpm_name}.*\.rpm$")
    sha256=$(sha256sum $rpm_file_name | awk '{print $1}')
    echo $sha256
}

get_package_info() {
    local rpm_name="$1"
    local sha256=$(get_sha256 "$rpm_name")
    local files=$(rpm -ql "$rpm_name" | jq -R . | jq -s .)
    local sonames=$(rpm -q --requires "$rpm_name" | grep 'so' | jq -R . | jq -s .)
    local binaries=$(rpm -q --filesbypackage "$rpm_name" | grep 'bin' | jq -R . | jq -s .)
    echo "{\"hash\": \"$sha256\", \"pkgname\": \"$rpm_name\", \"files\": $files, \"sonames\": $sonames, \"binaries\": $binaries}"
}

convert_to_json(){
    local key="$1"
    local data="$2"
    local json=""
    IFS=' ' read -r -a entries <<< "$data"
    local pkgname=${entries[0]%%|*}
    local files=()
    local sonames=()
    local binaries=()

    for entry in "${entries[@]}"; do
        IFS='|' read -r pkgname category value <<< "$entry"
        echo "$pkgname=======$category======$value"
        case "$category" in
            "files")
                files+=("${value}")
                ;;
            "sonames")
                sonames+=("${value}")
                ;;
            "binaries")
                binaries+=("${value}")
                ;;
        esac
    done

    local files_json=$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)
    local sonames_json=$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)
    local binaries_json=$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)

    json+="\"$key\": {
        \"pkgname\": \"$pkgname\",
        \"files\": $files_json,
        \"sonames\": $sonames_json,
        \"binaries\": $binaries_json
    }"

    json+=""
    echo "$json"
    json_data=$json
}

convert_to_json_by_jq () {
    local key="$1"
    local data="$2"
    IFS=' ' read -r -a entries <<< "$data"
    local pkgname=${entries[0]%%|*}
    local files=()
    local sonames=()
    local binaries=()
    for entry in "${entries[@]}"; do
        IFS='|' read -r pkgname category value <<< "$entry"
        echo "$pkgname=======$category======$value"
        case "$category" in
            "files")
                files+=("${value}")
                ;;
            "sonames")
                sonames+=("${value}")
                ;;
            "binaries")
                binaries+=("${value}")
                ;;
        esac
    done

    # 创建 JSON 对象
    local json=$(jq -n \
        --arg key "$key" \
        --arg pkgname "$pkgname" \
        --argjson files "$(printf '%s\n' "${files[@]}" | jq -R . | jq -s .)" \
        --argjson sonames "$(printf '%s\n' "${sonames[@]}" | jq -R . | jq -s .)" \
        --argjson binaries "$(printf '%s\n' "${binaries[@]}" | jq -R . | jq -s .)" \
        '{
            requires: {
                ($key): {
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

init_rpm_info_with_check_sum() {
    rpm_info["unknown"]=""

    for dep in "${!file_deps[@]}"; do
        if [[ "/bin/sh" == $dep ]];then
            rpm_name="bash"
        else
            rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
        fi
        if [ -n "$rpm_name" ];then
            echo "download rpm dep: $rpm_name"
            dnf download $rpm_name
            sha256=$(get_sha256 "$rpm_name")
            echo "get sha256 for $rpm_name: $sha256"
            rpm_info[$sha256]+="$rpm_name|files|$dep "
        else
            rpm_info["unknown"]+="$rpm_name|files|$dep "
        fi
    done

    for dep in "${!so_deps[@]}"; do
        rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
        if [ -n "$rpm_name" ];then
            echo "download rpm dep: $rpm_name"
            dnf download $rpm_name
            sha256=$(get_sha256 "$rpm_name")
            echo "get sha256 for $rpm_name: $sha256"
            rpm_info[$sha256]+="$rpm_name|sonames|$dep "
        else
            rpm_info["unknown"]+="$rpm_name|sonames|$dep "
        fi
    done

    for dep in "${!bin_deps[@]}"; do
        rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
        if [ -n "$rpm_name" ];then
            echo "download rpm dep: $rpm_name"
            dnf download $rpm_name
            sha256=$(get_sha256 "$rpm_name")
            echo "get sha256 for $rpm_name: $sha256"
            rpm_info[$sha256]+="$rpm_name|binaries|$dep "
        else
            rpm_info["unknown"]+="$rpm_name|binaries|$dep "
        fi
    done
}

generate_metadata_json_by_jq() {
    output_json=$(jq -n '{}')
    for key in "${!rpm_info[@]}"; do
        data=${rpm_info[$key]}
        echo "data: $data"
        convert_to_json_by_jq "$key" "${rpm_info[$key]}"
        output_json=$(echo "$output_json" | jq --argjson new_obj "$json_data" '. * $new_obj')
    done
    output_file="output.json"
    echo "$output_json" | jq '.' > "$output_file"
    echo "JSON has been written to $output_file"

}

# step 1
download_input_rpm
# step 2
query_requirements
# step 3
classify_deps
# step 4
init_rpm_info_with_check_sum
# step 5
generate_metadata_json_by_jq