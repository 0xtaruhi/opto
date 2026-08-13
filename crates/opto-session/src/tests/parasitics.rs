// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

struct ParasiticFixture {
    _dir: TestPath,
    base: PathBuf,
    partial: PathBuf,
    cycle: PathBuf,
    session: Session,
}

fn fixture(name: &str) -> ParasiticFixture {
    let dir = temp_dir(name);
    let library = dir.join("demo.lib");
    let verilog = dir.join("top.v");
    let base = dir.join("base.spef");
    let partial = dir.join("partial.spef");
    let cycle = dir.join("cycle.spef");
    std::fs::write(
        &library,
        r#"
library (demo) {
  time_unit : "1ps";
  capacitive_load_unit (1, ff);
  cell (BUF) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 0.1; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise (scalar) { values ( "0.1" ); }
        cell_fall (scalar) { values ( "0.1" ); }
        rise_transition (scalar) { values ( "0.1" ); }
        fall_transition (scalar) { values ( "0.1" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &verilog,
        "module top(input a, output z); wire n; BUF U1(.A(a), .Y(n)); BUF U2(.A(n), .Y(z)); endmodule\n",
    )
    .unwrap();
    std::fs::write(&base, spef_body("1.0", "1 U1:Y U2:A 1000.0")).unwrap();
    std::fs::write(&partial, spef_body("1.0", "")).unwrap();
    std::fs::write(
        &cycle,
        r#"*SPEF "IEEE 1481-1998"
*DESIGN "top"
*DIVIDER /
*DELIMITER :
*C_UNIT 1 FF
*R_UNIT 1 OHM
*D_NET n 2.0
*CONN
*I U1:Y O
*I U2:A I
*CAP
1 U2:A 1.0
2 n:1 1.0
*RES
1 U1:Y U2:A 1000.0
2 U1:Y n:1 1000.0
3 n:1 U2:A 1000.0
*END
"#,
    )
    .unwrap();

    let mut session = Session::new();
    session.set_synth_effort(SynthesisEffort::Low);
    session.read_libs(std::slice::from_ref(&library)).unwrap();
    session
        .import_verilog(std::slice::from_ref(&verilog), &FrontendOptions::default())
        .unwrap();
    session.synthesize().unwrap();
    ParasiticFixture {
        _dir: dir,
        base,
        partial,
        cycle,
        session,
    }
}

fn spef_body(capacitance: &str, resistor: &str) -> String {
    format!(
        r#"*SPEF "IEEE 1481-1998"
*DESIGN "top"
*DIVIDER /
*DELIMITER :
*C_UNIT 1 FF
*R_UNIT 1 OHM
*D_NET n {capacitance}
*CONN
*I U1:Y O
*I U2:A I
*CAP
1 U2:A {capacitance}
*RES
{resistor}
*END
"#
    )
}

fn golden_preamble(path: &std::path::Path) -> String {
    format!(
        "Information: Library unit = 1.000000 ps. (SPEF-10)\n\
Information: Derived delay scale factor = 1.000000. (SPEF-11)\n\
Information: Library unit = 0.001000 pF. (SPEF-10)\n\
Information: Derived capacitance scale factor = 1000.000000. (SPEF-12)\n\
Information: Library unit = 1.000000 kOhm. (SPEF-10)\n\
Information: Derived resistance scale factor = 0.001000. (SPEF-13)\n\n\
Reading {} ...\n\n\
Information: Path delimiter = /. (SPEF-2)\n\
Information: Pin delimiter = :. (SPEF-3)\n\n\
1 RNET/DNET has been read.\n",
        path.display()
    )
}

fn normalize_report_date(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            if line.starts_with("Date: ") {
                "Date: <timestamp>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn read_parasitics_default_output_matches_golden() {
    let mut fixture = fixture("read-parasitics-default-golden");
    let output = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.base),
            &ReadParasiticsOptions::default(),
        )
        .unwrap();
    assert_eq!(
        output,
        format!(
            "{}\n\n0 net completion steps have been performed.\n\
0 nets have been skipped due to partial parasitics\n",
            golden_preamble(&fixture.base)
        )
    );
}

#[test]
fn read_parasitics_completion_and_incremental_counts_match_golden() {
    let mut fixture = fixture("read-parasitics-completion-golden");
    let elmore = ReadParasiticsOptions {
        delay_model: ParasiticDelayModel::Elmore,
        ..ReadParasiticsOptions::default()
    };
    fixture
        .session
        .read_parasitics(std::slice::from_ref(&fixture.base), &elmore)
        .unwrap();
    let incremental = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.base),
            &ReadParasiticsOptions {
                increment: true,
                ..elmore.clone()
            },
        )
        .unwrap();
    assert!(incremental.ends_with(
        "0 net completion steps have been performed.\n\
0 pin-to-pin delays have been annotated on 1 net\n\
0 nets have been skipped due to partial parasitics\n"
    ));

    let skipped = fixture
        .session
        .read_parasitics(std::slice::from_ref(&fixture.partial), &elmore)
        .unwrap();
    assert!(skipped.ends_with(
        "0 net completion steps have been performed.\n\
0 pin-to-pin delays have been annotated on 0 nets\n\
1 net has been skipped due to partial parasitics\n"
    ));

    let completed = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.partial),
            &ReadParasiticsOptions {
                completion: Some(ReadParasiticsCompletion::Zero),
                ..elmore
            },
        )
        .unwrap();
    assert!(completed.ends_with(
        "1 net completion step has been performed.\n\
1 pin-to-pin delay has been annotated on 1 net\n\
0 nets have been skipped due to partial parasitics\n"
    ));
}

#[test]
fn read_parasitics_verbose_and_loop_output_match_golden() {
    let mut fixture = fixture("read-parasitics-verbose-golden");
    let verbose = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.base),
            &ReadParasiticsOptions {
                delay_model: ParasiticDelayModel::Elmore,
                verbose: true,
                ..ReadParasiticsOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        normalize_report_date(&verbose),
        format!(
            "{}\n\n0 net completion steps have been performed.\n\
1 pin-to-pin delay has been annotated on 1 net\n\
0 nets have been skipped due to partial parasitics\n\n\
# Parasitic annotations report\n\n\
Design: top\n\
Version: opto {}\n\
Date: <timestamp>\n\n\
Information: Updating design information...\n\n\
| Net | From | To   | Rise | Fall | Load |\n\
|-----|------|------|------|------|------|\n\
| n   | U1/Y | U2/A | 1.10 | 1.10 | 1.10 |",
            golden_preamble(&fixture.base),
            env!("CARGO_PKG_VERSION"),
        )
    );

    let cycle = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.cycle),
            &ReadParasiticsOptions {
                delay_model: ParasiticDelayModel::Arnoldi,
                ..ReadParasiticsOptions::default()
            },
        )
        .unwrap();
    assert!(cycle.contains(
        "Warning: Net 'n' contains an interconnection loop. The delays and transition times computed for this net may be inaccurate. (SPEF-19)"
    ));
    assert!(
        cycle.contains("Warning: '1' nets with interconnection loops have been read. (SPEF-21)")
    );
}

#[test]
fn read_parasitics_syntax_only_stops_before_annotation_summary() {
    let mut fixture = fixture("read-parasitics-syntax-golden");
    let output = fixture
        .session
        .read_parasitics(
            std::slice::from_ref(&fixture.base),
            &ReadParasiticsOptions {
                syntax_only: true,
                ..ReadParasiticsOptions::default()
            },
        )
        .unwrap();
    assert_eq!(output, golden_preamble(&fixture.base));
}
