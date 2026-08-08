# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {IBEX_ROOT IBEX_MANIFEST IBEX_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

source [file join [file dirname [info script]] sources.tcl]
set include_dirs {}
foreach option $ibex_vcs_options {
    if {[string match "+incdir+*" $option]} {
        lappend include_dirs [string range $option 8 end]
    }
}
read_hdl -define SYNTHESIS -incdir $include_dirs $ibex_sources
elaborate ibex_core
redirect -file $env(IBEX_CHECK_REPORT) {
    check_design
    report_area
}
exit
