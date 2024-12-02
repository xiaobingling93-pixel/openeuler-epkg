prepare () {
        sed -i -e 's|^logfile .*$|logfile /var/log/redis/redis.log|g' redis.conf
        sed -i -e '$ alogfile /var/log/redis/sentinel.log' sentinel.conf
        sed -i -e 's|^dir .*$|dir /var/lib/redis|g' redis.conf
}