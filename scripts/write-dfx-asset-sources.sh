#!/usr/bin/env bash

# `niv update` puts base32 "nix hashes" (https://nixos.wiki/wiki/Nix_Hash) in nix/sources.json.
# These hashes can't be compared trivially with equivalent hashes output by shasum -a 256.
# This script converts the needed base32 hashes into base16 hashes that can be trivially compared.
#
# This script also makes it so there is no build-time dependency on jq (in addition to nix).
#
# Run this script when updating agent-rs, replica, ic-starter, or ic-ref in nix/sources.json.

set -e

which jq >/dev/null || ( echo "Please install jq in order to run this script." ; exit 1 )
which nix >/dev/null || ( echo "Please install nix in order to run this script." ; exit 1 )
which curl >/dev/null || ( echo "Please install curl in order to run this script." ; exit 1 )

SDK_ROOT_DIR="$( cd -- "$(dirname -- "$( dirname -- "${BASH_SOURCE[0]}" )" )" &> /dev/null && pwd )"

DFX_ASSET_SOURCES="$SDK_ROOT_DIR/scripts/dfx-asset-sources.sh"
NIX_SOURCES_JSON="$SDK_ROOT_DIR/nix/sources.json"

read_sha256_from_nix_sources() {
    KEY="$1"

    SHA256_BASE32=$(jq -r .'"'"$KEY"'".sha256' "$NIX_SOURCES_JSON")

    nix-hash --to-base16 --type sha256 "$SHA256_BASE32"
}

read_url_from_nix_sources() {
    KEY="$1"

    jq -r .'"'"$KEY"'".url' "$NIX_SOURCES_JSON"
}

normalize_varname() {
    echo "$1" | tr '[:lower:]-' '[:upper:]_'
}

write_sha256() {
    KEY="$1"
    SHA256=$(read_sha256_from_nix_sources "$KEY")

    NAME=$(normalize_varname "${KEY}_SHA256")

    echo "$NAME=\"$SHA256\"" >>"$DFX_ASSET_SOURCES"
}

calculate_sha256() {
    KEY="$1"
    URL=$(read_url_from_nix_sources "$KEY")
    TEMP_FILE="$(mktemp)"
    TEMP_DIR="$(mktemp -d)"
    curl --silent --location --fail --output "$TEMP_FILE" "$URL"

    tar -xf "$TEMP_FILE" -C "$TEMP_DIR"
    EXPECTED_BASE32_SHA256=$(jq -r .'"'"$KEY"'".sha256' "$NIX_SOURCES_JSON")
    ACTUAL_BASE32_SHA256="$(nix-hash --base32 --type sha256 "$TEMP_DIR")"

    SHA256="$(shasum -a 256 "$TEMP_FILE" |  awk '{print $1}' )"

    chmod -R 0744 "$TEMP_DIR"
    rm "$TEMP_FILE"
    rm -rf "$TEMP_DIR"

    if [ "$EXPECTED_BASE32_SHA256" != "$ACTUAL_BASE32_SHA256" ]; then
        echo "SHA256 mismatch for $URL: expected $EXPECTED_BASE32_SHA256, got $ACTUAL_BASE32_SHA256"
        exit 1
    fi

    NAME=$(normalize_varname "${KEY}_SHA256")

    echo "$NAME=\"$SHA256\"" >>"$DFX_ASSET_SOURCES"

}

write_url() {
    KEY="$1"
    URL=$(read_url_from_nix_sources "$KEY")

    NAME=$(normalize_varname "${KEY}_URL")

    echo "$NAME=\"$URL\"" >>"$DFX_ASSET_SOURCES"
}

write_var() {
    VALUE=$(jq -r .'"'"$1"'"."'"$2"'"' "$NIX_SOURCES_JSON")
    NAME=$(normalize_varname "${1}_${2}")
    echo "$NAME=\"$VALUE\"" >>"$DFX_ASSET_SOURCES"
}

echo "# generated by write-dfx-asset-sources.sh" >"$DFX_ASSET_SOURCES"

for name in "ic-ref" "icx-proxy" "ic-admin" "ic-btc-adapter" "ic-canister-http-adapter" "ic-nns-init" "ic-starter" "motoko" "replica" "canister-sandbox" "sandbox-launcher" "sns";
do
    if [[ "$name" == "replica" || "$name" == "canister-sandbox" ]]; then
        echo "# The replica and canister_sandbox binaries must have the same revision." >>"$DFX_ASSET_SOURCES"
    fi
    for platform in "darwin" "linux";
    do
        write_sha256 "${name}-x86_64-${platform}"
        write_url "${name}-x86_64-${platform}"
    done
done

write_url "motoko-base"
calculate_sha256 "motoko-base"

