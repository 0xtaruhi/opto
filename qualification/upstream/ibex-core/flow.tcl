# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {IBEX_ROOT IBEX_MANIFEST IBEX_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

source [file join [file dirname [info script]] sources.tcl]
set include_dirs {}
foreach option $ibex_compile_options {
    if {[string match "+incdir+*" $option]} {
        lappend include_dirs [string range $option 8 end]
    }
}
read_hdl -define SYNTHESIS -incdir $include_dirs $ibex_sources
elaborate ibex_core
set synthesis_requested [expr {
    [info exists env(IBEX_SYNTHESIS)] && $env(IBEX_SYNTHESIS) eq "1"
}]
if {$synthesis_requested} {
    set target [file normalize [file join [file dirname [info script]] .. .. libraries opto_test.lib]]
    read_libs [list $target]
    synth
}
redirect -file $env(IBEX_CHECK_REPORT) {
    check_design
    report_area
}
if {$synthesis_requested && [info exists env(IBEX_NETLIST_DIRECTORY)]} {
    file mkdir $env(IBEX_NETLIST_DIRECTORY)
    write_hdl -hierarchy [file join $env(IBEX_NETLIST_DIRECTORY) ibex_core.v]
}
exit
