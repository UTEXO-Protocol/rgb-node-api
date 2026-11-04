#!/bin/sh
set -eu

# Git may ask to store/erase credentials; never persist the build secret.
[ "${1:-}" = get ] || exit 0
protocol= host= path=
while IFS='=' read -r key value; do
    case "$key" in
        protocol) protocol=$value ;;
        host) host=$value ;;
        path) path=$value ;;
    esac
done
[ "$protocol" = https ] && [ "$host" = github.com ] || exit 0
case "$path" in
    UTEXO-Protocol/*) ;;
    rgb-tools-devs/rgb-consensus|rgb-tools-devs/rgb-consensus.git|\
    rgb-tools-devs/rgb-ops|rgb-tools-devs/rgb-ops.git|\
    rgb-tools-devs/rgb-api|rgb-tools-devs/rgb-api.git|\
    rgb-tools-devs/rgb-schemas|rgb-tools-devs/rgb-schemas.git) ;;
    *) exit 0 ;;
esac

token_file=${GITHUB_TOKEN_FILE:-/run/secrets/github_token}
[ -s "$token_file" ] || exit 1
printf 'username=x-access-token\npassword=%s\n' "$(cat "$token_file")"
