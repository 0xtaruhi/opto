#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUTPUT_DIR" >&2
  exit 2
fi

output_dir=$1
if [[ -e "$output_dir" ]] && [[ -n "$(find "$output_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  echo "output directory is not empty: $output_dir" >&2
  exit 1
fi

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
mkdir -p "$output_dir"

fetch_and_verify() {
  local url=$1
  local expected=$2
  local archive=$3
  curl --fail --location --retry 3 --output "$archive" "$url"
  printf '%s  %s\n' "$expected" "$archive" | sha256sum --check --status
}

epfl_archive="$work_dir/epfl.tar.gz"
fetch_and_verify \
  "https://github.com/lsils/benchmarks/archive/0060e156826e733d69bf5b3322d1bdd0d03a1f9a.tar.gz" \
  "55d92e23bd423999f68c1af8b9679d26c6fa6709a30211aa6750badc87f4003e" \
  "$epfl_archive"
mkdir "$output_dir/epfl"
tar -xzf "$epfl_archive" --strip-components=1 -C "$output_dir/epfl"

iwls_archive="$work_dir/iwls2005.tgz"
fetch_and_verify \
  "https://iwls.org/iwls2005/IWLS_2005_benchmarks_V_1.0.tgz" \
  "f45955b87a255abd009f5bab081c658e90e7ee0b71c270d8a4948390a3033b51" \
  "$iwls_archive"
mkdir "$output_dir/iwls2005"
tar -xzf "$iwls_archive" --strip-components=1 -C "$output_dir/iwls2005"

python_bin=${PYTHON:-python3}
"$python_bin" "$(dirname "$0")/../../tools/check_real_benchmarks.py" \
  --sources "$output_dir" \
  "$(dirname "$0")/medium.toml"
"$python_bin" "$(dirname "$0")/../../tools/check_real_benchmarks.py" \
  --sources "$output_dir" \
  "$(dirname "$0")/gate.toml"
echo "fetched and verified real-medium-30 in $output_dir"
