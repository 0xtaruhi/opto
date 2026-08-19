# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {PULP_AXI_ROOT PULP_AXI_MANIFEST PULP_AXI_TOP PULP_AXI_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

set manifest [open $env(PULP_AXI_MANIFEST) r]
set sources {}
set common_cell_roots {}
while {[gets $manifest line] >= 0} {
    set fields [split $line "\t"]
    if {[llength $fields] == 2 && ![string match "#*" $line]} {
        set relative [lindex $fields 0]
        lappend sources [file join $env(PULP_AXI_ROOT) $relative]
        if {[string match \
                ".bender/git/checkouts/common_cells-*/include/common_cells/*.svh" \
                $relative]} {
            lappend common_cell_roots \
                [file dirname [file dirname [file dirname $relative]]]
        }
    }
}
close $manifest

set common_cell_roots [lsort -unique $common_cell_roots]
if {[llength $common_cell_roots] != 1} {
    error "manifest must pin exactly one common_cells include root"
}
set common_cells [file join $env(PULP_AXI_ROOT) [lindex $common_cell_roots 0]]

lappend sources [file join [file dirname [info script]] live_tops.sv]
read_hdl \
    -define {SYNTHESIS COMMON_CELLS_ASSERTS_OFF} \
    -incdir [list \
        [file join $env(PULP_AXI_ROOT) include] \
        [file join $common_cells include]] \
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
