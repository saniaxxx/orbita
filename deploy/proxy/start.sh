#!/bin/sh
set -eu

# Render only the resolver variable; preserve all Nginx request variables.
if [ -z "${NGINX_LOCAL_RESOLVERS:-}" ]; then
    NGINX_LOCAL_RESOLVERS=$(awk 'BEGIN { ORS=" " } $1 == "nameserver" { if ($2 ~ /:/) print "[" $2 "]"; else print $2 }' /etc/resolv.conf)
    NGINX_LOCAL_RESOLVERS=${NGINX_LOCAL_RESOLVERS% }
fi
if [ -z "$NGINX_LOCAL_RESOLVERS" ]; then
    printf '%s\n' 'No DNS resolvers found in /etc/resolv.conf.' >&2
    exit 1
fi
export NGINX_LOCAL_RESOLVERS

envsubst '${NGINX_LOCAL_RESOLVERS}' \
    < /etc/nginx/templates/nginx.conf.template \
    > /etc/nginx/nginx.conf

/app/generate.sh
nginx -t
exec nginx -g 'daemon off;'
