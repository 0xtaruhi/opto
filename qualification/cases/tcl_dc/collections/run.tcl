# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

set root $::env(OPTO_CASE_ROOT)
set output $::env(OPTO_CASE_OUTPUT)
read_hdl [list [file join $root top.sv]]
elaborate top
set ports [get_ports *]
if {[llength $ports] != 2} {
    error "expected two top-level bus ports"
}
set selected [get_ports -filter {.direction == in} *]
if {[llength $selected] != 1} {
    error "expected one input bus port"
}
redirect -file [file join $output check.rpt] {check_design}
redirect -file [file join $output area.rpt] {report_area}
exit
