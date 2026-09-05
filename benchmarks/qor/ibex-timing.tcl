# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {IBEX_PERIOD IBEX_LIBRARY IBEX_RESULT_ROOT IBEX_ROOT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

set repo_root [file normalize [file join [file dirname [info script]] ../..]]
set env(IBEX_MANIFEST) [file join $repo_root qualification upstream ibex-core manifest.tsv]
source [file join $repo_root qualification upstream ibex-core sources.tcl]

set include_dirs {}
foreach option $ibex_compile_options {
    if {[string match "+incdir+*" $option]} {
        lappend include_dirs [string range $option 8 end]
    }
}

read_libs $env(IBEX_LIBRARY)
read_hdl -define SYNTHESIS -incdir $include_dirs $ibex_sources
elaborate ibex_core
if {$env(IBEX_PERIOD) ne "none"} {
    create_clock -name core_clk -period $env(IBEX_PERIOD) [get_ports clk_i]
}
set_db synth_effort high
synth

file mkdir $env(IBEX_RESULT_ROOT)
redirect -file [file join $env(IBEX_RESULT_ROOT) qor.rpt] {
    report_qor
}
redirect -file [file join $env(IBEX_RESULT_ROOT) area.rpt] {
    report_area
}
redirect -file [file join $env(IBEX_RESULT_ROOT) timing.rpt] {
    report_timing -max_paths 10
}
write_hdl -hierarchy [file join $env(IBEX_RESULT_ROOT) ibex_core.v]
exit
