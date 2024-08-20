#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <rpm-package>"
    exit 1
fi

rpm_package="$1"

# 临⚌~W⚌⚌~V~G件
requires_file=$(mktemp)
dependencies_file=$(mktemp)
json_file=$(mktemp)

dnf download $rpm_package
# ⚌~_⚌询⚌~L~G⚌~ZRPM⚌~Z~D⚌~]⚌~V
dnf repoquery --requires "$rpm_package" > "$requires_file"
cat $requires_file

# ⚌~@~Z⚌~G⚌~]⚌~V类⚌~^~K⚌~H~F类
declare -A file_deps
declare -A so_deps
declare -A bin_deps

while read -r dep; do
    echo "read dep: $dep"
    if [[ "$dep" =~ \.so ]]; then
        echo "catch soname: $dep"
        so_deps["$dep"]=1
    elif [[ "$dep" =~ /bin/ ]]; then
        echo "catch bin: $dep"
        bin_deps["$dep"]=1
    else
        echo "catch file: $dep"
        file_deps["$dep"]=1
    fi
done < "$requires_file"

echo "cat deps"
echo "sonames: $so_deps"
echo "bin_deps: $bin_deps"
echo "file_deps: $file_deps"
# ⚌~N⚌⚌~O~VRPM⚌~L~E⚌~Z~DSHA256⚌~S~H⚌~L
get_sha256() {
    local rpm_name="$1"
    echo "get sha256 for $rpm_name"
    rpm_file_name=$(ls | grep "^${rpm_name}.*\.rpm$")
    sha256=$(sha256sum $rpm_file_name | awk '{print $1}')
    echo $sha256
}

# ⚌~N⚌⚌~O~V⚌~L~E⚌~Z~D详⚌~F信⚌~A⚌
get_package_info() {
    local rpm_name="$1"
    local sha256=$(get_sha256 "$rpm_name")

    # ⚌~N⚌⚌~O~V⚌~L~E⚌~V~G件⚌~@~Asonames⚌~R~Lbinaries
    local files=$(rpm -ql "$rpm_name" | jq -R . | jq -s .)
    local sonames=$(rpm -q --requires "$rpm_name" | grep 'so' | jq -R . | jq -s .)
    local binaries=$(rpm -q --filesbypackage "$rpm_name" | grep 'bin' | jq -R . | jq -s .)

    echo "{\"hash\": \"$sha256\", \"pkgname\": \"$rpm_name\", \"files\": $files, \"sonames\": $sonames, \"binaries\": $binaries}"
}

# ⚌~_⚌询⚌~]⚌~V并⚌~R类
declare -A rpm_info
rpm_info["unknown"]=""

# ⚌~D⚌~P~F⚌~V~G件类⚌~^~K⚌~Z~D⚌~]⚌~V
for dep in "${!file_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    echo "query dep's rpm name: $rpm_name"

    if [ -n "$rpm_name" ];then
        echo "ready to get sha256sum for $rpm_name"
        download_result=$(dnf download "$rpm_name")
        echo "download_result: $download_result"
        sha256=$(get_sha256 "$rpm_name")
        rpm_info[$sha256]+="$rpm_name|files|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|files|$dep "
    fi
done

# ⚌~D⚌~P~Fsoname类⚌~^~K⚌~Z~D⚌~]⚌~V
for dep in "${!so_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    if [ -n "$rpm_name" ];then
        echo "ready to get sha256sum for $rpm_name"
        download_result=$(dnf download "$rpm_name")
        echo "download_result: $download_result"
        sha256=$(get_sha256 "$rpm_name")
        rpm_info[$sha256]+="$rpm_name|sonames|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|sonames|$dep "
    fi
done

# ⚌~D⚌~P~F⚌~L⚌~[⚌~H⚌类⚌~^~K⚌~Z~D⚌~]⚌~V
for dep in "${!bin_deps[@]}"; do
    rpm_name=$(dnf repoquery --whatprovides "$dep" | awk -F '-' '{print $1}')
    if [ -n "$rpm_name" ];then
        echo "ready to get sha256sum for $rpm_name"
        download_result=$(dnf download "$rpm_name")
        echo "download_result: $download_result"
        sha256=$(get_sha256 "$rpm_name")
        rpm_info[$sha256]+="$rpm_name|binaries|$dep "
    else
        rpm_info["unknown"]+="$rpm_name|binaries|$dep "
    fi
done


# ⚌~T~_⚌~H~PJSON⚌~S⚌~G⚌
echo "{" > "$json_file"

for sha256 in "${!rpm_info[@]}"; do
    IFS='|' read -r rpm_name type dep <<<"${rpm_info[$sha256]}"
    case $type in
        "files")
            files="[$dep]"
            ;;
        "sonames")
            files=""
            sonames="[$dep]"
            ;;
        "binaries")
            files=""
            binaries="[$dep]"
            ;;
    esac
    echo "\"$sha256\": {\"pkgname\": \"$rpm_name\", \"files\": $files, \"sonames\": $sonames, \"binaries\": $binaries}," >> "$json_file"
done

# ⚌~H| ⚌~Y⚌⚌~\~@⚌~P~N⚌~@个⚌~@~W⚌~O⚌并⚌~W⚌⚌~P~HJSON
sed -i '$ s/,$//' "$json_file"
echo "}" >> "$json_file"

# ⚌~S⚌~G⚌⚌~S⚌~^~\
cat "$json_file"

# ⚌~E⚌~P~F临⚌~W⚌⚌~V~G件
rm "$requires_file" "$dependencies_file" "$json_file"
