#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

set -euo pipefail

readonly MODE="${1:-}"
readonly OUTPUT_ARGUMENT="${2:-}"
readonly NETLIST_ARGUMENT="${3:-}"

if [[ "$MODE" != "rtl" && "$MODE" != "gate" ]] || [[ -z "$OUTPUT_ARGUMENT" ]]; then
    echo "usage: $0 {rtl|gate} OUTPUT_DIRECTORY [GATE_NETLIST]" >&2
    exit 2
fi
if [[ "$MODE" == "gate" && -z "$NETLIST_ARGUMENT" ]]; then
    echo "gate mode requires a synthesized netlist" >&2
    exit 2
fi

readonly CASE_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly REPOSITORY_ROOT="$(git -C "$CASE_DIRECTORY" rev-parse --show-toplevel)"
readonly VERILATOR_COMMAND="${VERILATOR:-verilator}"
readonly BUILD_JOBS="${BUILD_JOBS:-4}"
readonly CXX_COMMAND="${CXX:-c++}"
readonly PYTHON3_COMMAND="${PYTHON3:-python3}"

mkdir -p "$OUTPUT_ARGUMENT"
readonly OUTPUT_DIRECTORY="$(cd -- "$OUTPUT_ARGUMENT" && pwd)"

sources=()
arguments=(
    --binary
    --timing
    -j "$BUILD_JOBS"
    -Wno-fatal
    -Wno-PINMISSING
    -Wno-MULTITOP
    -Wno-TIMESCALEMOD
    -Wno-UNOPTFLAT
    -CFLAGS "-std=c++20 -fcoroutines"
    -MAKEFLAGS "CXX=$CXX_COMMAND LINK=$CXX_COMMAND PYTHON3=$PYTHON3_COMMAND"
    --top-module ibex_smoke_tb
    --Mdir "$OUTPUT_DIRECTORY/obj_dir"
)

if [[ "$MODE" == "rtl" ]]; then
    : "${IBEX_ROOT:?IBEX_ROOT must name the pinned Ibex checkout}"
    while IFS=$'\t' read -r relative_path checksum; do
        if [[ -z "$checksum" || "$relative_path" == \#* ]]; then
            continue
        fi
        sources+=("$IBEX_ROOT/$relative_path")
    done < "$CASE_DIRECTORY/manifest.tsv"
    arguments+=(
        -DSYNTHESIS
        "+incdir+$IBEX_ROOT/vendor/lowrisc_ip/ip/prim/rtl"
        "+incdir+$IBEX_ROOT/vendor/lowrisc_ip/dv/sv/dv_utils"
    )
else
    if [[ ! -f "$NETLIST_ARGUMENT" ]]; then
        echo "synthesized netlist does not exist: $NETLIST_ARGUMENT" >&2
        exit 2
    fi
    readonly NETLIST="$(cd -- "$(dirname -- "$NETLIST_ARGUMENT")" && pwd)/$(basename -- "$NETLIST_ARGUMENT")"
    sources+=("$REPOSITORY_ROOT/qualification/libraries/opto_test_cells.v" "$NETLIST")
fi

"$VERILATOR_COMMAND" "${arguments[@]}" \
    "${sources[@]}" "$CASE_DIRECTORY/gate_smoke_tb.sv"
exec "$OUTPUT_DIRECTORY/obj_dir/Vibex_smoke_tb"
