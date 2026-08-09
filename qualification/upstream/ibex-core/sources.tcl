# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {IBEX_ROOT IBEX_MANIFEST} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

set ibex_sources {}
set manifest [open $env(IBEX_MANIFEST) r]
set line_number 0
while {[gets $manifest line] >= 0} {
    incr line_number
    if {$line_number < 4 || [string match "#*" $line]} {
        continue
    }
    set fields [split $line "\t"]
    if {[llength $fields] != 2} {
        error "invalid Ibex manifest record at line $line_number"
    }
    lappend ibex_sources [file join $env(IBEX_ROOT) [lindex $fields 0]]
}
close $manifest

set ibex_compile_options [list \
    +incdir+[file join $env(IBEX_ROOT) vendor lowrisc_ip ip prim rtl] \
    +incdir+[file join $env(IBEX_ROOT) vendor lowrisc_ip dv sv dv_utils]]
