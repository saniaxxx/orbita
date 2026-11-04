#!/bin/sh
set -eu
umask 077

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DOMAINS_DIR=$SCRIPT_DIR/domains
CA_CERT=${CA_CERT:-$SCRIPT_DIR/certs/ca.crt}
CA_KEY=${CA_KEY:-$SCRIPT_DIR/certs/ca.key}
OUTPUT_DIR=$SCRIPT_DIR/generated
NGINX_ROOT=${NGINX_ROOT:-/app}

# Rebuild only generated outputs, never the persistent CA.
rm -rf "$OUTPUT_DIR/certs" "$OUTPUT_DIR/configs" "$OUTPUT_DIR/stream"
mkdir -p "$OUTPUT_DIR/certs" "$OUTPUT_DIR/configs" "$OUTPUT_DIR/stream"
tmp_dir=$(mktemp -d "$OUTPUT_DIR/.generate.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT
trap 'exit 1' HUP INT TERM
: >"$tmp_dir/seen"

for domains_file in "$DOMAINS_DIR"/*.txt; do
    [ -f "$domains_file" ] || continue
    list_name=${domains_file##*/}
    list_name=${list_name%.txt}
    case "$list_name" in
        ''|.|..|*[!A-Za-z0-9._-]*)
            printf 'Invalid domain list filename: %s\n' "$domains_file" >&2
            exit 1 ;;
    esac

    list_cert_dir=$OUTPUT_DIR/certs/$list_name
    list_config_file=$OUTPUT_DIR/configs/$list_name.conf
    list_stream_file=$OUTPUT_DIR/stream/$list_name.conf
    mkdir -p "$list_cert_dir"
    : >"$list_config_file"
    : >"$list_stream_file"

    env_name=$(printf '%s' "$list_name" | tr '[:lower:].-' '[:upper:]__')_UPSTREAM_URL
    override_url=$(printenv "$env_name" || :)
    exec 3< "$domains_file"
    upstream=''
    IFS= read -r upstream <&3 || :
    upstream=${upstream%$(printf '\r')}
    upstream_url=${override_url:-$upstream}
    case "$upstream_url" in
        '') exec 3<&-; continue ;;
        http://*|https://*) ;;
        *) upstream_url=http://$upstream_url ;;
    esac
    upstream_url=${upstream_url%/}
    case "$upstream_url" in
        *[!A-Za-z0-9._~:/%+-]*)
            printf 'Invalid upstream URL in %s\n' "$domains_file" >&2
            exit 1 ;;
    esac
    authority=${upstream_url#*://}
    authority=${authority%%/*}
    upstream_tls_name=${authority%%:*}
    case "$upstream_tls_name" in
        ''|*[!A-Za-z0-9._-]*)
            printf 'Upstream must have a DNS hostname or IPv4 address: %s\n' "$upstream_url" >&2
            exit 1 ;;
    esac

    while IFS= read -r domain <&3 || [ -n "$domain" ]; do
        domain=${domain%$(printf '\r')}
        case "$domain" in ''|'#'*) continue ;; esac
        direct=false
        case "$domain" in !*) direct=true; domain=${domain#?} ;; esac
        domain=$(printf '%s' "$domain" | tr '[:upper:]' '[:lower:]')
        dns_name=$domain
        case "$domain" in \*.*) dns_name=${domain#*.} ;; esac
        case "$dns_name" in
            ''|.*|*.|*..*|*[!A-Za-z0-9.-]*)
                printf 'Invalid domain: %s\n' "$domain" >&2
                exit 1 ;;
        esac
        if [ "${#dns_name}" -gt 253 ] || ! printf '%s\n' "$dns_name" | grep -Eq -- '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$'; then
            printf 'Invalid DNS labels: %s\n' "$domain" >&2
            exit 1
        fi
        if grep -Fqx -- "$domain" "$tmp_dir/seen"; then
            printf 'Duplicate domain across lists: %s\n' "$domain" >&2
            exit 1
        fi
        printf '%s\n' "$domain" >>"$tmp_dir/seen"

        if [ "$direct" = true ]; then
            # Bypass the list upstream, but still terminate listed TLS locally.
            proxy_target='$scheme://$host$request_uri'
            proxy_tls_name='$host'
            proxy_host='$host'
        else
            proxy_target="$upstream_url/\$host\$request_uri"
            proxy_tls_name=$upstream_tls_name
            proxy_host='$proxy_host'
        fi

        printf '%s 127.0.0.1:8443;\n' "$domain" >>"$list_stream_file"
        key_domain=$(printf '%s' "$domain" | tr '*' '_')
        key_file=$list_cert_dir/$key_domain.key
        chain_file=$list_cert_dir/$key_domain.chain.crt
        cat >"$tmp_dir/extensions" <<EOF
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:$domain
EOF
        openssl ecparam -name secp384r1 -genkey -noout -out "$key_file"
        openssl req -new -sha384 -key "$key_file" -out "$tmp_dir/leaf.csr" \
            -subj "/C=RU/ST=Saint-Petersburg/L=Saint-Petersburg/O=v3rt3p/OU=https-proxy/CN=$domain"
        openssl x509 -req -sha384 -days 365 \
            -in "$tmp_dir/leaf.csr" -CA "$CA_CERT" -CAkey "$CA_KEY" \
            -set_serial "0x$(openssl rand -hex 16)" \
            -extfile "$tmp_dir/extensions" -out "$tmp_dir/leaf.crt"
        cat "$tmp_dir/leaf.crt" "$CA_CERT" >"$chain_file"

        # Wildcard routing uses the concrete requested host, not a literal '*'.
        cat >>"$list_config_file" <<EOF
server {
    listen 80;
    listen 127.0.0.1:8443 ssl;
    server_name $domain;
    ssl_certificate $NGINX_ROOT/generated/certs/$list_name/$key_domain.chain.crt;
    ssl_certificate_key $NGINX_ROOT/generated/certs/$list_name/$key_domain.key;

    location / {
        proxy_pass $proxy_target;
        proxy_http_version 1.1;
        proxy_ssl_server_name on;
        proxy_ssl_name $proxy_tls_name;
        proxy_ssl_verify on;
        proxy_ssl_trusted_certificate /etc/ssl/certs/ca-certificates.crt;
        proxy_set_header Host $proxy_host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection \$connection_upgrade;
    }
}

EOF
    done
    exec 3<&-
done
printf 'Generated private-CA certificates and HTTP/stream configs in %s\n' "$OUTPUT_DIR"
