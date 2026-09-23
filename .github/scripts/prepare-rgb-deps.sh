#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${BFA_MIRRORS_TOKEN:-}" ]]; then
    echo "::error::Configure BFA_MIRRORS_TOKEN with read access to the private RGB BFA mirrors."
    exit 1
fi
if [[ ! "${RGB_LIB_REV:-}" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::RGB_LIB_REV must pin a complete library commit SHA."
    exit 1
fi

library_dir="${GITHUB_WORKSPACE:?}/../rgb-lib"
if [[ -e "$library_dir" ]]; then
    echo "::error::Expected a fresh sibling rgb-lib checkout; refusing to overwrite existing state."
    exit 1
fi

git config --global url."https://github.com/".insteadOf "ssh://git@github.com/"
# Git invokes this helper on demand. Store only the variable reference, never
# the token itself, in runner configuration or a credential-bearing remote URL.
git config --global credential.https://github.com.helper \
    '!f() { if [ "$1" = get ]; then printf "%s\n" "username=x-access-token" "password=$BFA_MIRRORS_TOKEN"; fi; }; f'

git init "$library_dir"
git -C "$library_dir" remote add origin https://github.com/UTEXO-Protocol/rgb-lib.git
git -C "$library_dir" fetch --depth=1 origin "$RGB_LIB_REV"
git -C "$library_dir" checkout --detach FETCH_HEAD
