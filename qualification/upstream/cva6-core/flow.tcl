# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {CVA6_ROOT CVA6_MANIFEST CVA6_CONFIG_FILE CVA6_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

set manifest [open $env(CVA6_MANIFEST) r]
set files {}
while {[gets $manifest line] >= 0} {
    set fields [split $line "\t"]
    if {[llength $fields] == 2 && ![string match "#*" $line]} {
        set relative_path [lindex $fields 0]
        if {$relative_path eq "@CONFIG@"} {
            lappend files $env(CVA6_CONFIG_FILE)
        } else {
            lappend files [file join $env(CVA6_ROOT) $relative_path]
        }
    }
}
close $manifest

set include_dirs [list \
    [file join $env(CVA6_ROOT) core include] \
    [file join $env(CVA6_ROOT) core cvfpu src] \
    [file join $env(CVA6_ROOT) vendor pulp-platform common_cells include] \
    [file join $env(CVA6_ROOT) vendor pulp-platform common_cells src] \
    [file join $env(CVA6_ROOT) vendor pulp-platform axi include] \
    [file join $env(CVA6_ROOT) common local util] \
    [file join $env(CVA6_ROOT) core cache_subsystem hpdcache rtl include] \
    [file join $env(CVA6_ROOT) core cache_subsystem hpdcache rtl src utils ecc]]

read_hdl \
    -define {SYNTHESIS HPDCACHE_ASSERT_OFF} \
    -incdir $include_dirs \
    $files
elaborate cva6
set synthesis_requested [expr {
    [info exists env(CVA6_SYNTHESIS)] && $env(CVA6_SYNTHESIS) eq "1"
}]
if {$synthesis_requested} {
    set target [file normalize [file join [file dirname [info script]] .. .. libraries opto_test.lib]]
    read_libs [list $target]
    synth
}
redirect -file $env(CVA6_CHECK_REPORT) {
    check_design
    report_area
}
if {$synthesis_requested && [info exists env(CVA6_NETLIST_DIRECTORY)]} {
    file mkdir $env(CVA6_NETLIST_DIRECTORY)
    set configuration [file rootname [file tail $env(CVA6_CONFIG_FILE)]]
    set netlist [file join $env(CVA6_NETLIST_DIRECTORY) "$configuration.v"]
    write_hdl -hierarchy $netlist
}
exit
