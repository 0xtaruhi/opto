# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {PULP_AXI_ROOT PULP_AXI_MANIFEST PULP_AXI_TOP PULP_AXI_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

set manifest [open $env(PULP_AXI_MANIFEST) r]
set sources {}
while {[gets $manifest line] >= 0} {
    set fields [split $line "\t"]
    if {[llength $fields] == 2 && ![string match "#*" $line]} {
        lappend sources [file join $env(PULP_AXI_ROOT) [lindex $fields 0]]
    }
}
close $manifest

set common_cells [glob -nocomplain -types d \
    [file join $env(PULP_AXI_ROOT) .bender git checkouts common_cells-*]]
if {[llength $common_cells] != 1} {
    error "expected exactly one pinned common_cells Bender checkout"
}

lappend sources [file join [file dirname [info script]] live_tops.sv]
read_hdl \
    -define {SYNTHESIS COMMON_CELLS_ASSERTS_OFF} \
    -incdir [list \
        [file join $env(PULP_AXI_ROOT) include] \
        [file join [lindex $common_cells 0] include]] \
    $sources
elaborate $env(PULP_AXI_TOP)
check_design
read_libs [file normalize \
    [file join [file dirname [info script]] .. .. libraries opto_test.lib]]
synth
redirect -file $env(PULP_AXI_CHECK_REPORT) {
    check_design
    report_area
}
if {[info exists env(PULP_AXI_NETLIST_DIRECTORY)]} {
    file mkdir $env(PULP_AXI_NETLIST_DIRECTORY)
    write_hdl -hierarchy \
        [file join $env(PULP_AXI_NETLIST_DIRECTORY) "$env(PULP_AXI_TOP).v"]
}
exit
