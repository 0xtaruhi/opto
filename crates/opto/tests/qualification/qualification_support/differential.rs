// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{output_directory, required_executable, workspace_root};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Config {
    opto: PathBuf,
    yosys: PathBuf,
    output_dir: PathBuf,
    seed_start: u64,
    seeds: u64,
}

struct GeneratedCase {
    family: &'static str,
    width: u32,
    reference: String,
    candidate: String,
}

#[derive(Clone, Copy)]
struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn choose<'a>(&mut self, values: &'a [&'a str]) -> &'a str {
        values[self.index(values.len())]
    }

    fn index(&mut self, length: usize) -> usize {
        assert!(length > 0, "cannot choose from an empty collection");
        let length = u64::try_from(length).expect("collection length fits u64");
        usize::try_from(self.next() % length).expect("bounded random index fits usize")
    }
}

pub(super) fn run() {
    run_inner().unwrap_or_else(|error| panic!("generated differential qualification: {error}"));
}

fn run_inner() -> Result<(), Box<dyn Error>> {
    let config = Config {
        opto: PathBuf::from(env!("CARGO_BIN_EXE_opto")),
        yosys: required_executable("OPTO_YOSYS"),
        output_dir: output_directory("generated-differential"),
        seed_start: environment_u64("OPTO_DIFFERENTIAL_SEED_START", 0)?,
        seeds: environment_u64("OPTO_DIFFERENTIAL_SEEDS", 512)?,
    };
    if config.seeds == 0 {
        return Err("OPTO_DIFFERENTIAL_SEEDS must be greater than zero".into());
    }
    let opto = fs::canonicalize(&config.opto)?;
    if !opto.is_file() {
        return Err(format!("Opto executable is not a file: {}", opto.display()).into());
    }
    if config.output_dir.exists() {
        fs::remove_dir_all(&config.output_dir)?;
    }
    fs::create_dir_all(&config.output_dir)?;
    let output_dir = fs::canonicalize(&config.output_dir)?;
    let root = workspace_root();
    let flow = root.join("qualification/scripts/frontend-differential.tcl");
    let library = root.join("qualification/libraries/opto_test.lib");
    let sequential_library = root.join("qualification/libraries/frontend_sequential.lib");
    verify_reset_latch_negative_control(&config.yosys, &library, &sequential_library, &output_dir)?;
    let mut summary = String::from("seed\tfamily\twidth\tstatus\tstage\tartifact_dir\n");
    let mut failure_count = 0u64;

    for offset in 0..config.seeds {
        let seed = config
            .seed_start
            .checked_add(offset)
            .ok_or("seed range overflows u64")?;
        let generated = generate(seed);
        let case_name = format!("seed-{seed:016x}-{}", generated.family);
        let case_dir = output_dir.join(case_name);
        if case_dir.exists() {
            fs::remove_dir_all(&case_dir)?;
        }
        fs::create_dir(&case_dir)?;
        let reference = case_dir.join("reference.sv");
        let candidate = case_dir.join("candidate.sv");
        let netlist = case_dir.join("opto.v");
        fs::write(&reference, &generated.reference)?;
        fs::write(&candidate, &generated.candidate)?;

        let opto_output = Command::new(&opto)
            .arg("-f")
            .arg(&flow)
            .env("FRONTEND_DIFF_RTL", &candidate)
            .env("FRONTEND_DIFF_NETLIST", &netlist)
            .env("FRONTEND_DIFF_LIBRARY", &library)
            .env("FRONTEND_DIFF_SEQUENTIAL_LIBRARY", &sequential_library)
            .output()?;
        write_command_log(&case_dir.join("opto.log"), &opto_output)?;
        let (status, stage) = if !opto_output.status.success() || !netlist.is_file() {
            failure_count += 1;
            ("fail", "opto")
        } else {
            let formal_output = run_formal(
                &config.yosys,
                &reference,
                &netlist,
                &library,
                &sequential_library,
            )?;
            write_command_log(&case_dir.join("formal.log"), &formal_output)?;
            if formal_output.status.success() {
                ("pass", "formal")
            } else {
                failure_count += 1;
                ("fail", "formal")
            }
        };
        let artifact = if status == "pass" {
            fs::remove_dir_all(&case_dir)?;
            "-".to_string()
        } else {
            case_dir.display().to_string()
        };
        writeln!(
            summary,
            "{seed}\t{}\t{}\t{status}\t{stage}\t{artifact}",
            generated.family, generated.width
        )?;
        if status == "fail" {
            eprintln!(
                "FAIL seed={seed} family={} width={} stage={stage}",
                generated.family, generated.width
            );
        } else if (offset + 1).is_multiple_of(64) || offset + 1 == config.seeds {
            eprintln!(
                "generated differential progress: {}/{}",
                offset + 1,
                config.seeds
            );
        }
    }

    fs::write(output_dir.join("summary.tsv"), summary)?;
    fs::write(
        output_dir.join("metadata.txt"),
        format!(
            "opto={}\nyosys={}\nseed_start={}\nseeds={}\nfailures={}\n",
            command_version(&opto, ["--version"]),
            command_version(&config.yosys, ["-V"]),
            config.seed_start,
            config.seeds,
            failure_count
        ),
    )?;
    if failure_count != 0 {
        return Err(format!(
            "{failure_count} of {} generated cases failed; artifacts are in {}",
            config.seeds,
            output_dir.display()
        )
        .into());
    }
    println!(
        "all {} generated cases passed; summary: {}",
        config.seeds,
        output_dir.join("summary.tsv").display()
    );
    Ok(())
}

fn environment_u64(name: &str, default: u64) -> Result<u64, Box<dyn Error>> {
    std::env::var(name).map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|error| format!("{name} is not an unsigned integer: {error}").into())
    })
}

fn run_formal(
    yosys: &Path,
    reference: &Path,
    netlist: &Path,
    library: &Path,
    sequential_library: &Path,
) -> std::io::Result<Output> {
    // Keep latch state visible to equivalence: async2sync can hide the state
    // relation behind an asserted asynchronous reset.
    let commands = format!(
        r#"
read_verilog -sv "{}";
hierarchy -check -top top;
proc; flatten; memory; opt;
clk2fflogic; opt;
rename top gold;
design -stash gold;
read_liberty -ignore_miss_func "{}";
read_liberty -ignore_miss_func "{}";
read_verilog "{}";
hierarchy -check -top top;
flatten; proc; memory; opt;
clk2fflogic; opt;
rename top gate;
design -stash gate;
design -reset;
read_liberty -ignore_miss_func "{}";
read_liberty -ignore_miss_func "{}";
design -copy-from gold -as gold gold;
design -copy-from gate -as gate gate;
equiv_make gold gate equiv;
hierarchy -check -top equiv;
equiv_simple;
equiv_induct -undef -seq 4;
equiv_status -assert;
"#,
        yosys_quote(reference),
        yosys_quote(library),
        yosys_quote(sequential_library),
        yosys_quote(netlist),
        yosys_quote(library),
        yosys_quote(sequential_library)
    );
    Command::new(yosys)
        .arg("-Q")
        .arg("-p")
        .arg(commands)
        .output()
}

fn verify_reset_latch_negative_control(
    yosys: &Path,
    library: &Path,
    sequential_library: &Path,
    output_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    let control = output_dir.join("negative-reset-latch-control");
    fs::create_dir(&control)?;
    let reference = control.join("reference.sv");
    let candidate = control.join("candidate.sv");
    fs::write(
        &reference,
        "module top(input enable, reset, d, output reg q);\n\
         always @* begin\n\
           if (reset) q <= 1'b0;\n\
           else if (enable) q <= d;\n\
         end\n\
         endmodule\n",
    )?;
    fs::write(
        &candidate,
        "module top(input enable, reset, d, output reg q);\n\
         always @* begin\n\
           if (reset) q <= 1'b1;\n\
           else if (enable) q <= d;\n\
         end\n\
         endmodule\n",
    )?;
    let proof = run_formal(yosys, &reference, &candidate, library, sequential_library)?;
    write_command_log(&control.join("formal.log"), &proof)?;
    if proof.status.success() {
        return Err(format!(
            "formal negative control accepted an incorrect reset-latch implementation; see {}",
            control.join("formal.log").display()
        )
        .into());
    }
    let mut combined = proof.stdout.clone();
    combined.extend_from_slice(&proof.stderr);
    let transcript = String::from_utf8_lossy(&combined);
    if !transcript.contains("unproven $equiv") {
        return Err(format!(
            "formal negative control failed before proving the intended reset-latch mismatch; see {}",
            control.join("formal.log").display()
        )
        .into());
    }
    Ok(())
}

fn yosys_quote(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn write_command_log(path: &Path, output: &Output) -> std::io::Result<()> {
    let mut bytes = output.stdout.clone();
    bytes.extend_from_slice(&output.stderr);
    fs::write(path, bytes)
}

fn command_version<I, S>(program: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            let mut text = String::from_utf8(output.stdout).ok()?;
            if text.trim().is_empty() {
                text = String::from_utf8(output.stderr).ok()?;
            }
            Some(text.lines().next().unwrap_or("unknown").trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn generate(seed: u64) -> GeneratedCase {
    let mut rng = DeterministicRng::new(seed);
    match seed % 41 {
        0 => net_initializer_case(&mut rng),
        1 => procedural_case(&mut rng),
        2 => function_case(&mut rng),
        3 => concat_lvalue_case(&mut rng),
        4 => signed_case(&mut rng),
        5 => rotate_case(&mut rng),
        6 => unpacked_array_case(&mut rng),
        7 => generate_loop_case(&mut rng),
        8 => dynamic_indexed_lvalue_case(),
        9 => nonzero_range_case(&mut rng),
        10 => packed_struct_case(),
        11 => streaming_case(&mut rng),
        12 => inside_case(&mut rng),
        13 => compound_assignment_case(&mut rng),
        14 => procedural_for_case(&mut rng),
        15 => function_return_case(&mut rng),
        16 => sequential_enable_case(&mut rng),
        17 => sequential_sync_reset_case(&mut rng),
        18 => sequential_async_reset_case(&mut rng),
        19 => hierarchy_parameter_case(),
        20 => casez_case(&mut rng),
        21 => case_inside_case(&mut rng),
        22 => replication_cast_case(),
        23 => constant_operator_case(),
        24 => sequential_negative_edge_case(&mut rng),
        25 => sequential_active_low_enable_case(&mut rng),
        26 => sequential_sync_active_low_controls_case(&mut rng),
        27 => sequential_async_active_high_reset_case(&mut rng),
        28 => sequential_async_negative_edge_case(&mut rng),
        29 => dynamic_part_select_rvalue_case(),
        30 => multidimensional_unpacked_case(&mut rng),
        31 => packed_union_case(),
        32 => unary_operator_case(&mut rng),
        33 => mixed_signed_width_case(),
        34 => generate_conditional_case(),
        35 => function_loop_break_case(),
        36 => nested_member_select_case(),
        37 => latch_case(&mut rng),
        38 => active_low_latch_case(&mut rng),
        39 => reset_latch_case(&mut rng),
        _ => deep_expression_case(&mut rng),
    }
}

fn width(rng: &mut DeterministicRng) -> u32 {
    2 + (rng.next() % 7) as u32
}

fn continuous_header(width: u32) -> String {
    format!(
        "module top(input wire [{}:0] a, b, c, input wire [3:0] shamt, input wire [2:0] sel, output wire [{}:0] y, output wire flag);\n",
        width - 1,
        width - 1
    )
}

fn procedural_header(width: u32) -> String {
    format!(
        "module top(input logic [{}:0] a, b, c, input logic [3:0] shamt, input logic [2:0] sel, output logic [{}:0] y, output wire flag);\n",
        width - 1,
        width - 1
    )
}

fn sequential_header(width: u32) -> String {
    format!(
        "module top(input logic clk, reset, en, input logic [{}:0] a, b, c, input logic [3:0] shamt, input logic [2:0] sel, output logic [{}:0] y, output wire flag);\n",
        width - 1,
        width - 1
    )
}

fn reference(width: u32, value: &str, flag: &str) -> String {
    format!(
        "{}assign y = {value};\nassign flag = {flag};\nendmodule\n",
        continuous_header(width)
    )
}

fn net_initializer_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let op = rng.choose(&["+", "-", "^", "&", "|"]);
    let value = format!("((a {op} b) ^ c)");
    let candidate = format!(
        "{}wire [{}:0] first = a {op} b;\nwire [{}:0] second = first ^ c;\nassign y = second;\nassign flag = ^second;\nendmodule\n",
        continuous_header(width),
        width - 1,
        width - 1
    );
    GeneratedCase {
        family: "net_initializer",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn procedural_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "sel[1:0] == 2'd0 ? a + b : sel[1:0] == 2'd1 ? a - b : sel[1:0] == 2'd2 ? (a & b) ^ c : (sel[2] ? b : c)";
    let candidate = format!(
        "{}always_comb begin\n    y = c;\n    case (sel[1:0])\n        2'd0: y = a + b;\n        2'd1: y = a - b;\n        2'd2: y = (a & b) ^ c;\n        default: y = sel[2] ? b : c;\n    endcase\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "procedural_case",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn function_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let op = rng.choose(&["+", "-", "^"]);
    let value = format!("((a ^ b) {op} c)");
    let candidate = format!(
        "{}function automatic logic [{}:0] transform(input logic [{}:0] x, z, q);\n    logic [{}:0] temporary;\n    begin\n        temporary = x ^ z;\n        transform = temporary {op} q;\n    end\nendfunction\nassign y = transform(a, b, c);\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        width - 1,
        width - 1,
        width - 1
    );
    GeneratedCase {
        family: "function",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn concat_lvalue_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let widths = [2u32, 4, 6, 8];
    let width = widths[rng.index(widths.len())];
    let half = width / 2;
    let value = format!("({{a[{}:0], a[{}:{}]}} ^ b)", half - 1, width - 1, half);
    let candidate = format!(
        "{}wire [{}:0] low, high;\nassign {{high, low}} = a;\nwire [{}:0] swapped = {{low, high}};\nassign y = swapped ^ b;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        half - 1,
        width - 1
    );
    GeneratedCase {
        family: "concat_lvalue",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn signed_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let comparison = "$signed(a) < $signed(b)";
    let reference = format!(
        "{}function automatic [{}:0] reference_ashr(input [{}:0] value, input [3:0] amount);\n    integer bit_index;\n    begin\n        for (bit_index = 0; bit_index < {width}; bit_index = bit_index + 1) begin\n            if (bit_index + amount < {width})\n                reference_ashr[bit_index] = value[bit_index + amount];\n            else\n                reference_ashr[bit_index] = value[{}];\n        end\n    end\nendfunction\nassign y = {comparison} ? (a - b) : reference_ashr(a, shamt);\nassign flag = {comparison};\nendmodule\n",
        continuous_header(width),
        width - 1,
        width - 1,
        width - 1
    );
    let candidate = format!(
        "{}wire signed [{}:0] signed_a = a;\nwire signed [{}:0] signed_b = b;\nwire signed [{}:0] arithmetic = signed_a >>> shamt;\nassign y = signed_a < signed_b ? (a - b) : arithmetic;\nassign flag = signed_a < signed_b;\nendmodule\n",
        continuous_header(width),
        width - 1,
        width - 1,
        width - 1
    );
    GeneratedCase {
        family: "signed",
        width,
        reference,
        candidate,
    }
}

fn rotate_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = format!("((a << shamt) | (a >> (4'd{width} - shamt)))");
    let candidate = format!(
        "{}wire [3:0] inverse = 4'd{width} - shamt;\nwire [{}:0] rotated = (a << shamt) | (a >> inverse);\nassign y = rotated;\nassign flag = ^rotated;\nendmodule\n",
        continuous_header(width),
        width - 1
    );
    GeneratedCase {
        family: "rotate",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn unpacked_array_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "sel[1:0] == 2'd0 ? a : sel[1:0] == 2'd1 ? b : sel[1:0] == 2'd2 ? c : (a ^ b ^ c)";
    let candidate = format!(
        "{}wire [{}:0] values [0:3];\nassign values[0] = a;\nassign values[1] = b;\nassign values[2] = c;\nassign values[3] = a ^ b ^ c;\nassign y = values[sel[1:0]];\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        width - 1
    );
    GeneratedCase {
        family: "unpacked_array",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn generate_loop_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "((a & b) ^ ~c)";
    let candidate = format!(
        "{}for (genvar i = 0; i < {width}; i++) begin : generated_bits\n    wire term = a[i] & b[i];\n    assign y[i] = term ^ ~c[i];\nend\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "generate_loop",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn dynamic_indexed_lvalue_case() -> GeneratedCase {
    let width = 8;
    let value = "(a & ~(8'h03 << sel[1:0])) | (({6'b0, b[1:0]}) << sel[1:0])";
    let candidate = format!(
        "{}always_comb begin\n    y = a;\n    y[sel[1:0] +: 2] = b[1:0];\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "dynamic_indexed_lvalue",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn nonzero_range_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = 8;
    let (range, index) = if rng.next() & 1 == 0 {
        ("[10:3]", "4'd3 + {1'b0, sel}")
    } else {
        ("[3:10]", "4'd10 - {1'b0, sel}")
    };
    let value = "a ^ {8{a[sel]}}";
    let candidate = format!(
        "{}wire {range} indexed = a;\nwire selected = indexed[{index}];\nassign y = a ^ {{8{{selected}}}};\nassign flag = selected;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "nonzero_range",
        width,
        reference: reference(width, value, "a[sel]"),
        candidate,
    }
}

fn packed_struct_case() -> GeneratedCase {
    let width = 8;
    let value = "({a[7:4], b[3:0]} ^ c)";
    let candidate = format!(
        "{}typedef struct packed {{ logic [3:0] high; logic [3:0] low; }} pair_t;\npair_t from_fields, from_pattern;\nassign from_fields.high = a[7:4];\nassign from_fields.low = b[3:0];\nassign from_pattern = '{{high: a[7:4], low: b[3:0]}};\nassign y = (sel[0] ? {{from_fields.high, from_fields.low}} : from_pattern) ^ c;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "packed_struct",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn streaming_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = 8;
    let slices = [1u32, 2, 4];
    let slice = slices[rng.index(slices.len())];
    let parts = (0..width)
        .step_by(slice as usize)
        .map(|offset| format!("a[{}:{}]", offset + slice - 1, offset))
        .collect::<Vec<_>>()
        .join(", ");
    let value = format!("({{{parts}}} ^ b)");
    let candidate = format!(
        "{}wire [7:0] streamed = {{<<{slice}{{a}}}};\nassign y = streamed ^ b;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "streaming",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn inside_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let condition = "((sel >= 3'd1 && sel <= 3'd3) || sel == 3'd5)";
    let value = format!("(a ^ {{{width}{{{condition}}}}})");
    let candidate = format!(
        "{}wire member = sel inside {{[3'd1:3'd3], 3'd5}};\nassign y = a ^ {{{width}{{member}}}};\nassign flag = member;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "inside",
        width,
        reference: reference(width, &value, condition),
        candidate,
    }
}

fn compound_assignment_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "((a ^ b) + c)";
    let candidate = format!(
        "{}always_comb begin\n    y = a;\n    y ^= b;\n    y += c;\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "compound_assignment",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn procedural_for_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let reversed = (0..width)
        .map(|index| format!("b[{index}]"))
        .collect::<Vec<_>>()
        .join(", ");
    let value = format!("(a ^ {{{reversed}}})");
    let candidate = format!(
        "{}always_comb begin\n    y = '0;\n    for (int bit_index = 0; bit_index < {width}; bit_index++) begin\n        y[bit_index] = a[bit_index] ^ b[{width} - 1 - bit_index];\n    end\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "procedural_for",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn function_return_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(sel[0] ? a : b)";
    let candidate = format!(
        "{}function automatic logic [{}:0] choose(input logic [{}:0] x, z, fallback, input logic take_x);\n    begin\n        choose = fallback;\n        if (take_x)\n            return x;\n        choose = z;\n    end\nendfunction\nassign y = choose(a, b, c, sel[0]);\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        width - 1,
        width - 1
    );
    GeneratedCase {
        family: "function_return",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn sequential_enable_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a ^ b) + c";
    let candidate = format!(
        "{}always_ff @(posedge clk) begin\n    if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(posedge clk) begin\n    if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_enable",
        width,
        reference,
        candidate,
    }
}

fn sequential_sync_reset_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "a + (b ^ c)";
    let candidate = format!(
        "{}always_ff @(posedge clk) begin\n    if (reset)\n        y <= '0;\n    else if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(posedge clk) begin\n    if (reset)\n        y <= {{{width}{{1'b0}}}};\n    else if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_sync_reset",
        width,
        reference,
        candidate,
    }
}

fn sequential_async_reset_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a & b) | c";
    let candidate = format!(
        "{}always_ff @(posedge clk or negedge reset) begin\n    if (!reset)\n        y <= '0;\n    else if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(posedge clk or negedge reset) begin\n    if (reset == 1'b0)\n        y <= {{{width}{{1'b0}}}};\n    else if (en == 1'b1)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_async_reset",
        width,
        reference,
        candidate,
    }
}

fn sequential_negative_edge_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a + b) ^ c";
    let candidate = format!(
        "{}always_ff @(negedge clk) begin\n    y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(negedge clk) y <= {value};\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_negative_edge",
        width,
        reference,
        candidate,
    }
}

fn sequential_active_low_enable_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a | b) - c";
    let candidate = format!(
        "{}always_ff @(posedge clk) begin\n    if (!en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(posedge clk) begin\n    if (en == 1'b0)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_active_low_enable",
        width,
        reference,
        candidate,
    }
}

fn sequential_sync_active_low_controls_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a & ~b) ^ c";
    let candidate = format!(
        "{}always_ff @(negedge clk) begin\n    if (!reset)\n        y <= '1;\n    else if (!en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(negedge clk) begin\n    if (reset == 1'b0)\n        y <= {{{width}{{1'b1}}}};\n    else if (en == 1'b0)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_sync_active_low_controls",
        width,
        reference,
        candidate,
    }
}

fn sequential_async_active_high_reset_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a ^ ~b) + c";
    let candidate = format!(
        "{}always_ff @(negedge clk or posedge reset) begin\n    if (reset)\n        y <= '1;\n    else if (!en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(negedge clk or posedge reset) begin\n    if (reset == 1'b1)\n        y <= {{{width}{{1'b1}}}};\n    else if (en == 1'b0)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_async_active_high_reset",
        width,
        reference,
        candidate,
    }
}

fn sequential_async_negative_edge_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a - b) | c";
    let candidate = format!(
        "{}always_ff @(negedge clk or negedge reset) begin\n    if (!reset)\n        y <= '0;\n    else\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @(negedge clk or negedge reset) begin\n    if (reset == 1'b0)\n        y <= {{{width}{{1'b0}}}};\n    else\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "sequential_async_negative_edge",
        width,
        reference,
        candidate,
    }
}

fn dynamic_part_select_rvalue_case() -> GeneratedCase {
    let width = 4;
    let header = "module top(input logic [7:0] a, input logic [2:0] sel, output wire [3:0] y, output wire flag);\n";
    let value = "(sel[1:0] == 2'd0 ? a[3:0] : sel[1:0] == 2'd1 ? a[4:1] : sel[1:0] == 2'd2 ? a[5:2] : a[6:3])";
    let reference = format!("{header}assign y = {value};\nassign flag = ^({value});\nendmodule\n");
    let candidate = format!(
        "{header}wire [3:0] selected = a[{{1'b0, sel[1:0]}} +: 4];\nassign y = selected;\nassign flag = ^selected;\nendmodule\n"
    );
    GeneratedCase {
        family: "dynamic_part_select_rvalue",
        width,
        reference,
        candidate,
    }
}

fn multidimensional_unpacked_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value =
        "(sel[1:0] == 2'd0 ? a : sel[1:0] == 2'd1 ? b : sel[1:0] == 2'd2 ? c : (a ^ b ^ c))";
    let candidate = format!(
        "{}wire [{}:0] matrix [0:1][0:1];\nassign matrix[0][0] = a;\nassign matrix[0][1] = b;\nassign matrix[1][0] = c;\nassign matrix[1][1] = a ^ b ^ c;\nassign y = matrix[sel[1]][sel[0]];\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        width - 1
    );
    GeneratedCase {
        family: "multidimensional_unpacked",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn packed_union_case() -> GeneratedCase {
    let width = 8;
    let value = "({a[3:0], a[7:4]} ^ b)";
    let candidate = format!(
        "{}typedef union packed {{ logic [7:0] whole; struct packed {{ logic [3:0] high; logic [3:0] low; }} halves; }} overlay_t;\noverlay_t overlay;\nassign overlay.whole = a;\nassign y = {{overlay.halves.low, overlay.halves.high}} ^ b;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "packed_union",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn unary_operator_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(sel[1:0] == 2'd0 ? -a : sel[1:0] == 2'd1 ? {WIDTH{~(&b)}} : sel[1:0] == 2'd2 ? {WIDTH{~(|b)}} : {WIDTH{~(^b)}})"
        .replace("WIDTH", &width.to_string());
    let candidate = format!(
        "{}wire [{}:0] negative = -a;\nwire nand_reduction = ~&b;\nwire nor_reduction = ~|b;\nwire xnor_reduction = ~^b;\nassign y = sel[1:0] == 2'd0 ? negative : sel[1:0] == 2'd1 ? {{{width}{{nand_reduction}}}} : sel[1:0] == 2'd2 ? {{{width}{{nor_reduction}}}} : {{{width}{{xnor_reduction}}}};\nassign flag = ^y;\nendmodule\n",
        continuous_header(width),
        width - 1
    );
    GeneratedCase {
        family: "unary_operator",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn mixed_signed_width_case() -> GeneratedCase {
    let width = 8;
    let header = "module top(input logic signed [3:0] narrow, input logic signed [7:0] wide, output wire [7:0] y, output wire flag);\n";
    let extended = "{{4{narrow[3]}}, narrow}";
    let reference = format!(
        "{header}assign y = $signed({extended}) + wide;\nassign flag = $signed({extended}) < wide;\nendmodule\n"
    );
    let candidate = format!(
        "{header}wire signed [7:0] extended = narrow;\nassign y = extended + wide;\nassign flag = narrow < wide;\nendmodule\n"
    );
    GeneratedCase {
        family: "mixed_signed_width",
        width,
        reference,
        candidate,
    }
}

fn generate_conditional_case() -> GeneratedCase {
    let width = 8;
    let value = "((a + b) ^ ~(a + b) ^ (a - b))";
    let candidate = format!(
        "module transform #(parameter int MODE = 0) (input logic [7:0] x, z, output logic [7:0] result);\ngenerate\n    case (MODE)\n        0: assign result = x + z;\n        1: assign result = ~(x + z);\n        default: assign result = x - z;\n    endcase\nendgenerate\nendmodule\n{}wire [7:0] first, second, third;\ntransform #(.MODE(0)) u_first(.x(a), .z(b), .result(first));\ntransform #(.MODE(1)) u_second(.x(a), .z(b), .result(second));\ntransform #(.MODE(2)) u_third(.x(a), .z(b), .result(third));\nassign y = first ^ second ^ third;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "generate_conditional",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn function_loop_break_case() -> GeneratedCase {
    let width = 8;
    let value = "(a & (~a + 8'b1))";
    let candidate = format!(
        "{}function automatic logic [7:0] first_set(input logic [7:0] value);\n    begin\n        first_set = '0;\n        for (int index = 0; index < 8; index++) begin\n            if (value[index]) begin\n                first_set[index] = 1'b1;\n                break;\n            end\n        end\n    end\nendfunction\nassign y = first_set(a);\nassign flag = |y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "function_loop_break",
        width,
        reference: reference(width, value, &format!("|({value})")),
        candidate,
    }
}

fn nested_member_select_case() -> GeneratedCase {
    let width = 8;
    let value = "({a[3:0], a[7:4]} ^ b)";
    let candidate = format!(
        "{}typedef struct packed {{ logic [1:0][3:0] lanes; }} nested_t;\nnested_t nested;\nassign nested.lanes = a;\nassign y = {{nested.lanes[0], nested.lanes[1]}} ^ b;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "nested_member_select",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn latch_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a + b) ^ c";
    let candidate = format!(
        "{}always_latch begin\n    if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @* begin\n    if (en == 1'b1)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "latch",
        width,
        reference,
        candidate,
    }
}

fn active_low_latch_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a & b) | c";
    let candidate = format!(
        "{}always_latch begin\n    if (!en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @* begin\n    if (en == 1'b0)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: "active_low_latch",
        width,
        reference,
        candidate,
    }
}

fn reset_latch_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(a - b) ^ c";
    let active_low = rng.next() & 1 != 0;
    let reset_condition = if active_low { "!reset" } else { "reset" };
    let reference_reset = if active_low {
        "reset == 1'b0"
    } else {
        "reset == 1'b1"
    };
    let candidate = format!(
        "{}always_latch begin\n    if ({reset_condition})\n        y <= '0;\n    else if (en)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    let reference = format!(
        "{}always @* begin\n    if ({reference_reset})\n        y <= {{{width}{{1'b0}}}};\n    else if (en == 1'b1)\n        y <= {value};\nend\nassign flag = ^y;\nendmodule\n",
        sequential_header(width)
    );
    GeneratedCase {
        family: if active_low {
            "active_low_reset_latch"
        } else {
            "reset_latch"
        },
        width,
        reference,
        candidate,
    }
}

fn deep_expression_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let first_op = rng.choose(&["+", "-", "^", "&", "|", "*"]);
    let second_op = rng.choose(&["+", "-", "^", "&", "|", "*"]);
    let final_op = rng.choose(&["+", "-", "^", "&", "|"]);
    let left = format!("(a {first_op} b)");
    let right = format!("(b {second_op} c)");
    let selected = format!("(sel[0] ? {left} : {right})");
    let shifted = format!("(({selected} << shamt) | ({left} >> shamt))");
    let value = format!("({shifted} {final_op} {right})");
    let candidate = format!(
        "{}logic [{}:0] left, right, selected, shifted;\nalways_comb begin\n    left = a {first_op} b;\n    right = b {second_op} c;\n    selected = sel[0] ? left : right;\n    shifted = (selected << shamt) | (left >> shamt);\n    y = shifted {final_op} right;\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width),
        width - 1
    );
    GeneratedCase {
        family: "deep_expression",
        width,
        reference: reference(width, &value, &format!("^({value})")),
        candidate,
    }
}

fn hierarchy_parameter_case() -> GeneratedCase {
    let width = 8;
    let value = "(sel[0] ? ~(a + b) : (a + b)) ^ c";
    let candidate = format!(
        "module transform #(parameter int WIDTH = 8, parameter bit INVERT = 1'b0) (input logic [WIDTH-1:0] x, z, output logic [WIDTH-1:0] result);\n    if (INVERT)\n        assign result = ~(x + z);\n    else\n        assign result = x + z;\nendmodule\n{}wire [7:0] normal, inverted;\ntransform #(.WIDTH(8), .INVERT(1'b0)) u_normal(.x(a), .z(b), .result(normal));\ntransform #(.WIDTH(8), .INVERT(1'b1)) u_inverted(.x(a), .z(b), .result(inverted));\nassign y = (sel[0] ? inverted : normal) ^ c;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "hierarchy_parameter",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn casez_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "(sel[2] ? a : sel[1] ? b : c)";
    let candidate = format!(
        "{}always_comb begin\n    casez (sel)\n        3'b1??: y = a;\n        3'b01?: y = b;\n        default: y = c;\n    endcase\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "casez",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn case_inside_case(rng: &mut DeterministicRng) -> GeneratedCase {
    let width = width(rng);
    let value = "((sel >= 3'd1 && sel <= 3'd3) ? a : sel == 3'd5 ? b : c)";
    let candidate = format!(
        "{}always_comb begin\n    case (sel) inside\n        [3'd1:3'd3]: y = a;\n        3'd5: y = b;\n        default: y = c;\n    endcase\nend\nassign flag = ^y;\nendmodule\n",
        procedural_header(width)
    );
    GeneratedCase {
        family: "case_inside",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn replication_cast_case() -> GeneratedCase {
    let width = 8;
    let value = "({{4{a[3]}}, a[3:0]} ^ {b[1:0], b[1:0], b[1:0], b[1:0]})";
    let candidate = format!(
        "{}wire signed [3:0] narrow = a[3:0];\nwire [7:0] extended = narrow;\nwire [7:0] repeated = {{4{{b[1:0]}}}};\nassign y = extended ^ repeated;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "replication_cast",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}

fn constant_operator_case() -> GeneratedCase {
    let width = 8;
    let value = "((a >> 2) ^ (a & 8'h07) ^ (8'h01 << sel))";
    let candidate = format!(
        "{}wire [7:0] quotient = a / 8'd4;\nwire [7:0] remainder = a % 8'd8;\nwire [7:0] power = 8'd2 ** sel;\nassign y = quotient ^ remainder ^ power;\nassign flag = ^y;\nendmodule\n",
        continuous_header(width)
    );
    GeneratedCase {
        family: "constant_operator",
        width,
        reference: reference(width, value, &format!("^({value})")),
        candidate,
    }
}
