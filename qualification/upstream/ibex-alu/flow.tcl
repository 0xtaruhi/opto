# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {IBEX_ROOT IBEX_CHECK_REPORT} {
    if {![info exists env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

read_hdl [list \
    [file join $env(IBEX_ROOT) rtl ibex_pkg.sv] \
    [file join $env(IBEX_ROOT) rtl ibex_alu.sv]]
elaborate ibex_alu
redirect -file $env(IBEX_CHECK_REPORT) {
    check_design
    report_area
}
exit
