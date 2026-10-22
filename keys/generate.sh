#!/bin/sh
# Generate the shared MCUboot development signing key pair.
set -eu
umask 077

usage() {
    printf 'Usage: %s [OUTPUT_DIR]\n' "$0"
    printf 'Generate dev-p256.pem and dev-p256.pub.pem (default: this script\047s directory).\n'
    printf 'Requires OpenSSL; refuses to overwrite either existing file.\n'
}

case "${1-}" in
    -h|--help) usage; exit 0 ;;
    -*) usage >&2; exit 1 ;;
esac
if [ "$#" -gt 1 ]; then
    usage >&2
    exit 1
fi
if ! command -v openssl >/dev/null 2>&1; then
    printf 'Error: OpenSSL is required.\n' >&2
    exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output_dir=${1:-$script_dir}
mkdir -p -- "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
private_key=$output_dir/dev-p256.pem
public_key=$output_dir/dev-p256.pub.pem

for key in "$private_key" "$public_key"; do
    if [ -e "$key" ] || [ -L "$key" ]; then
        printf 'Error: refusing to overwrite %s\n' "$key" >&2
        exit 1
    fi
done

# Finish and validate the pair before publishing it. Hard links avoid clobbering
# a file created by another invocation after the checks above.
stage=$(mktemp -d "$output_dir/.generate-keys.XXXXXX")
trap 'rm -rf -- "$stage"' 0
trap 'exit 1' HUP INT TERM

openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
    -out "$stage/dev-p256.pem"
openssl pkey -in "$stage/dev-p256.pem" -pubout \
    -out "$stage/dev-p256.pub.pem"
openssl pkey -in "$stage/dev-p256.pem" -check -noout >/dev/null

ln "$stage/dev-p256.pem" "$private_key"
if ! ln "$stage/dev-p256.pub.pem" "$public_key"; then
    rm -f -- "$private_key"
    exit 1
fi
printf 'Generated %s\nGenerated %s\n' "$private_key" "$public_key"
