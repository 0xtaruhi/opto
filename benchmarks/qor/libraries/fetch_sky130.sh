#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <output-liberty>" >&2
    exit 2
fi

readonly orfs_revision=a5ff7ef7dac4338e6e5fad7710b85fc6c8f3503c
readonly expected_sha256=ec0e1067a35c8bf20b11e58d1e8ac53326067e4dac84a125cc1b917a3518d0d9
readonly source_url="https://raw.githubusercontent.com/The-OpenROAD-Project/OpenROAD-flow-scripts/$orfs_revision/flow/platforms/sky130hd/lib/sky130_fd_sc_hd__tt_025C_1v80.lib"
readonly maximum_attempts=3

output=$1
mkdir -p "$(dirname -- "$output")"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

if [[ -f $output ]] && [[ $(sha256_file "$output") == "$expected_sha256" ]]; then
    echo "using verified Liberty: $output"
    exit 0
fi

temporary=$(mktemp "${output}.tmp.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
for ((attempt = 1; attempt <= maximum_attempts; attempt++)); do
    curl --fail --location --silent --show-error --output "$temporary" "$source_url"
    actual_sha256=$(sha256_file "$temporary")
    if [[ $actual_sha256 == "$expected_sha256" ]]; then
        break
    fi
    if ((attempt == maximum_attempts)); then
        echo "Sky130 Liberty checksum mismatch after $maximum_attempts attempts: expected $expected_sha256, got $actual_sha256" >&2
        exit 1
    fi
    echo "Sky130 Liberty checksum mismatch on attempt $attempt; retrying pinned object" >&2
done

mv -- "$temporary" "$output"
trap - EXIT
echo "downloaded and verified Liberty: $output"
