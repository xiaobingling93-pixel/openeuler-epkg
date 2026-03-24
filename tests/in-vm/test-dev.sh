#!/bin/sh
# Run build-from-source test across multiple Docker images

. "$(dirname "$0")/host-vars.sh"

DOCKER_IMAGE="${DOCKER_IMAGE:-debian}"

SCRIPT_DIR="$(dirname "$0")"
FAILED_TESTS=""
PASSED_TESTS=""

# docker pull hub.oepkgs.net/openeuler/openeuler:25.09
# docker pull quay.io/openeuler/openeuler:25.09
DOCKER_IMAGES="openeuler/openeuler:25.09"

# docker pull hub.oepkgs.net/openeuler/openeuler
# docker pull quay.io/openeuler/openeuler
DOCKER_IMAGES="openeuler/openeuler"

# List of Docker images to test build-from-source on
# These should be official Docker Hub images that support building epkg
DOCKER_IMAGES="openeuler/openeuler fedora ubuntu archlinux"

# Run build-from-source test on each Docker image
for docker_image in $DOCKER_IMAGES; do
    echo "========================================="
    echo "Testing build-from-source on: $docker_image"
    echo "========================================="

    # Export DOCKER_IMAGE for host-vars.sh to pick up
    export DOCKER_IMAGE="$docker_image"

    # Run the build-from-source test
    if "$SCRIPT_DIR/test-one.sh" -d "$SCRIPT_DIR/cases/build-from-source.sh"; then
        echo "PASSED: $docker_image"
        PASSED_TESTS="$PASSED_TESTS $docker_image"
    else
        echo "FAILED: $docker_image"
        FAILED_TESTS="$FAILED_TESTS $docker_image"
    fi
    echo ""
done

# Summary
echo "========================================="
echo "Build-from-source Test Summary"
echo "========================================="
echo "Passed: $(echo $PASSED_TESTS | wc -w)"
echo "Failed: $(echo $FAILED_TESTS | wc -w)"

if [ -n "$FAILED_TESTS" ]; then
    echo "Failed images:$FAILED_TESTS"
    exit 1
fi

exit 0
