redis_patch() {
        patch -p1 -N < /root/.epkg/build/workspace/patches/CVE-2020-14147.patch
        patch -p1 -N < /root/.epkg/build/workspace/patches/improved-HyperLogLog-cardinality-estimation.patch
        patch -p1 -N < /root/.epkg/build/workspace/patches/Aesthetic-changes-to-PR.patch
}

redis_prep () {
        sed -i -e 's|^logfile .*$|logfile /var/log/redis/redis.log|g' redis.conf
        sed -i -e '$ alogfile /var/log/redis/sentinel.log' sentinel.conf
        sed -i -e 's|^dir .*$|dir /var/lib/redis|g' redis.conf
}
