# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

foreach variable {FRONTEND_DIFF_RTL FRONTEND_DIFF_NETLIST FRONTEND_DIFF_LIBRARY FRONTEND_DIFF_SEQUENTIAL_LIBRARY} {
    if {![info exists ::env($variable)]} {
        error "required environment variable $variable is not set"
    }
}

read_libs [list $::env(FRONTEND_DIFF_LIBRARY) $::env(FRONTEND_DIFF_SEQUENTIAL_LIBRARY)]
read_hdl [list $::env(FRONTEND_DIFF_RTL)]
elaborate top
check_design
synth
write_hdl -hierarchy $::env(FRONTEND_DIFF_NETLIST)
exit
