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

mkdir -p "$OUTPUT_ARGUMENT"
readonly OUTPUT_DIRECTORY="$(cd -- "$OUTPUT_ARGUMENT" && pwd)"

sources=()
arguments=(
    --binary
    --timing
    -j "$BUILD_JOBS"
    -Wno-fatal
    -Wno-TIMESCALEMOD
    -Wno-UNOPTFLAT
    -CFLAGS "-fcoroutines -O0"
    -MAKEFLAGS "CXX=$CXX_COMMAND LINK=$CXX_COMMAND"
    --top-module cva6_gate_smoke_tb
    --Mdir "$OUTPUT_DIRECTORY/obj_dir"
)

if [[ "$MODE" == "rtl" ]]; then
    : "${CVA6_ROOT:?CVA6_ROOT must name the pinned CVA6 checkout}"
    readonly CONFIGURATION="$CVA6_ROOT/core/include/cv32a6_imac_sv32_config_pkg.sv"

    while IFS=$'\t' read -r relative_path checksum; do
        if [[ -z "$checksum" || "$relative_path" == \#* ]]; then
            continue
        fi
        if [[ "$relative_path" == "@CONFIG@" ]]; then
            sources+=("$CONFIGURATION")
        else
            sources+=("$CVA6_ROOT/$relative_path")
        fi
    done < "$CASE_DIRECTORY/manifest.tsv"

    arguments+=(
        -DSYNTHESIS
        -DHPDCACHE_ASSERT_OFF
        "+incdir+$CVA6_ROOT/core/include"
        "+incdir+$CVA6_ROOT/core/cvfpu/src"
        "+incdir+$CVA6_ROOT/vendor/pulp-platform/common_cells/include"
        "+incdir+$CVA6_ROOT/vendor/pulp-platform/common_cells/src"
        "+incdir+$CVA6_ROOT/vendor/pulp-platform/axi/include"
        "+incdir+$CVA6_ROOT/common/local/util"
        "+incdir+$CVA6_ROOT/core/cache_subsystem/hpdcache/rtl/include"
        "+incdir+$CVA6_ROOT/core/cache_subsystem/hpdcache/rtl/src/utils/ecc"
    )
else
    if [[ ! -f "$NETLIST_ARGUMENT" ]]; then
        echo "synthesized netlist does not exist: $NETLIST_ARGUMENT" >&2
        exit 2
    fi
    readonly NETLIST="$(cd -- "$(dirname -- "$NETLIST_ARGUMENT")" && pwd)/$(basename -- "$NETLIST_ARGUMENT")"
    sources+=(
        "$REPOSITORY_ROOT/qualification/libraries/opto_test_cells.v"
        "$NETLIST"
        "$CASE_DIRECTORY/gate_sram_models.sv"
    )
    arguments+=(--output-split 20000 --output-split-cfuncs 20000)
fi

"$VERILATOR_COMMAND" "${arguments[@]}" \
    "${sources[@]}" "$CASE_DIRECTORY/gate_smoke_tb.sv"
(
    cd "$OUTPUT_DIRECTORY"
    exec "$OUTPUT_DIRECTORY/obj_dir/Vcva6_gate_smoke_tb"
)
