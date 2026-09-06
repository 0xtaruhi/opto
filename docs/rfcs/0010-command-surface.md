<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0010: Opto command surface

- Status: accepted
- Implementation: the current flat Tcl command and typed database interface;
  individual supported commands and fields are recorded in `docs/architecture.md`

## Summary

Opto uses a flat Tcl command surface with a coherent typed database model. The
normal synthesis lifecycle is deliberately short:

```tcl
set_db hdl_search_path {. ./rtl}
set_db lib_search_path ./lib
set_db synth_effort high
set_db clock_gating true

read_libs slow.lib macros.lib
read_hdl rtl/top.sv rtl/alu.sv
elaborate top

read_sdc constraints/top.sdc
synth

report_qor
report_timing -max_paths 10
write_hdl build/top.v
save build/top.ock
```

There is one public synthesis command: `synth`. Effort is a typed database
property, not a command-name fork. Generic optimization, technology mapping,
and post-map closure remain internal stages whose boundaries may evolve
without breaking user scripts.

One coherent object and property model spans configuration, synthesis, timing,
power, and future implementation or verification domains. Internal stages are
not exposed as historical command families or report ensembles. Commands
remain flat because
`report_timing` and `write_hdl` are clearer than mandatory `report timing` and
`write hdl` nesting.

## Design position

The command system follows these priorities, in order:

1. One concept has one canonical spelling.
2. The common path stays short: `read_hdl`, `elaborate`, `synth`.
3. Configuration and object properties use one typed `set_db` / `get_db`
   model.
4. Operations with side effects remain explicit action commands.
5. SDC remains a standards boundary and keeps its standard command names.
6. Every mutation is validated, atomic, and owned by one subsystem.
7. Internal pipeline stages and compatibility-only concepts are not public
   API.

Opto's command catalog, typed parser, database schema, report schemas, and
tests define this interface. Public standards such as Tcl, SDC, Liberty, SPEF,
and SystemVerilog remain external format boundaries; another product's command
inventory or abbreviations do not.

## Naming rules

Commands live directly in the Tcl global namespace. Opto has no mandatory
parent commands, command ensembles, or domain subcommands.

- Commands and options use lowercase snake case and must match exactly.
- Commands begin with an action: `read_hdl`, `create_scenario`,
  `report_timing`, `save`.
- Prefix abbreviations, aliases, deprecated spellings, and fallback dispatch
  are rejected.
- A command name identifies one operation. Different effort levels or policy
  presets never create sibling commands.
- An option exists only when it changes observable behavior.
- A scalar option may not be specified more than once.
- Names inherited from a standard, such as SDC, retain the standard spelling.

The vocabulary is intentionally small:

| Form | Meaning |
| --- | --- |
| `read_*` | Import and validate an external representation |
| `write_*` | Atomically export a representation or complete database |
| `create_*` | Create a named persistent database object |
| `remove_*` / `reset_*` | Delete or clear state owned by that command |
| `check_*` | Validate without changing design semantics |
| `report_*` | Render a human or machine-readable view |
| `get_db` / `set_db` | Query or change schema-declared database state |

Analysis verbs such as `synth`, `place`, `route`, `prove`, or `check_cdc` are
first-class flat commands when Opto implements those operations. They are not
forced into a generic `run` command.

## Canonical lifecycle

### Library loading

`read_libs` imports Liberty libraries atomically. It searches
`lib_search_path`, validates all inputs, interns shared names, builds compact
library arenas, and publishes nothing if any input fails.

All successfully loaded libraries are visible database objects. A future MMMC
scenario selects an explicit library collection. Opto does not provide hidden
library variables or synthetic wildcard entries.

`read_libs` is the single library-ingestion command and accepts one or more
files.

### HDL loading

`read_hdl` parses and records SystemVerilog source units. It does not elaborate
a top and does not select a current design.

```tcl
read_hdl rtl/pkg.sv rtl/top.sv
```

The files in one invocation form one ordered source batch. Repeated invocations
append batches in command order. Include directories and defines are explicit
typed options or root database properties; there is no logical WORK-library
setup ceremony.

Primary files are independent SystemVerilog compilation units by default.
`read_hdl -compilation_unit rtl/defines.sv rtl/top.sv` groups the ordered files
from that invocation into one compilation unit so preprocessor state can cross
primary-file boundaries.

`read_hdl` is the single HDL-ingestion command and never performs elaboration.
VHDL options are not advertised until a real VHDL frontend and mixed-language
model exist.

### Elaboration

`elaborate <top>` resolves packages, parameters, hierarchy, and loaded library
references, then publishes one elaborated design atomically. A successful
elaboration sets the root `current_design` property to the new design.

Hierarchy resolution is part of elaboration. There is no separate public
`link` phase whose state can disagree with the elaborated database.
`check_design` validates the current elaborated graph without repairing it.

Multiple elaborated designs may coexist. Selection uses the database model:

```tcl
get_db current_design
set_db current_design [lindex [get_db designs cpu_top] 0]
```

Design selection remains part of the database model rather than a parallel
explicit-design argument convention on every operation.

### Constraints

`read_sdc` evaluates a constraint file atomically against the selected design
and scenario. Implemented SDC commands remain usable interactively because SDC
is a public standards boundary, not because Opto imitates another shell.

An SDC failure publishes no partial clocks, exceptions, delays, derates, or
design rules. Unsupported SDC commands and options are errors rather than
warnings followed by a partial constraint set.

### Synthesis

`synth` is the only public logic-synthesis action. Its policy comes from typed
root and scenario properties:

```tcl
set_db synth_effort medium
set_db clock_gating true
synth
```

The initial `synth_effort` values are `low`, `medium`, and `high`. They alter
deterministic search budgets within one pipeline. They do not select different
engines, representations, mappers, or fallback paths.

`clock_gating` enables the owned clock-gating transformation and its reporting.
It is enabled by default, and `set_db clock_gating false` declines it. A target
without an integrated clock gate produces an empty gate catalog, so the setting
costs nothing there. It is not encoded in another synthesis command name. Properties that do not
affect the implemented pipeline are not exposed.

A successful `synth` publishes one mapped design revision, provenance, QoR
summary, timing state, and compact incremental records. Failure leaves the
previous revision intact.

## Database interface

### Why `get_db` and `set_db`

One database interface scales better than separate application-variable,
attribute, current-object, and object-list command families. It also gives
future timing, power, physical, and formal domains a shared vocabulary.

Configuration and object properties use the typed database API rather than
parallel variable systems or one setter command per property.

This is not an unrestricted string-addressed database. Every root property,
object class, relationship, and object property is declared in one schema with:

- its Tcl and internal types;
- readable object classes;
- whether it is mutable;
- lifecycle availability;
- validation and ownership rules;
- invalidated generations and derived caches;
- Opto checkpoint persistence behavior;
- computational cost classification.

Unknown or unavailable properties are errors. Read-only properties cannot be
set. A write to a collection validates every member before publishing any
change.

### Query forms

`get_db` has three exact forms:

```tcl
# Read one root property.
get_db current_design

# Query one object class in the current context.
get_db insts * -if {.is_sequential == true}

# Project one property from a list of object handles.
get_db $registers .name
```

Initial object-class nouns are `designs`, `ports`, `insts`, `pins`, `nets`,
`clocks`, `libraries`, and `lib_cells`. Design objects use stable typed handles;
the two Liberty classes currently return immutable canonical names because
Liberty data is process-local. `scenarios` is added only with end-to-end MMMC
support. Names, patterns, filters, and relationships share one typed query
engine and deterministic ordering. An explicit `-of <objects>` changes the
query context without silently changing the current design.

Object properties start with a dot, making them syntactically distinct from
root properties and object-class nouns. Opto does not accept arbitrary path
strings, recursive textual traversal, or property-name abbreviations.

Projection returns one Tcl list element per input object and preserves input
order. Missing stage-dependent values are typed unavailable values or explicit
errors according to the property's schema; they are never silently converted
to zero or an empty string.

### Mutation forms

`set_db` has two exact forms:

```tcl
# Change one mutable root property.
set_db synth_effort high

# Change one mutable property on a typed object list.
set_db $instances .dont_touch true
```

Root writes and object writes are atomic. `set_db` returns the number of
changed targets. Setting a value equal to the existing value returns zero and
does not invalidate caches.

Domain operations do not become properties merely to reduce the command
count. Reading files, elaborating, synthesizing, reporting, and writing outputs
remain explicit commands. SDC constraint creation also remains in SDC.

### Handles and Tcl lists

Database queries return ordinary Tcl lists whose elements are typed opaque
object handles. Consequently normal Tcl `foreach`, `lindex`, and `llength`
replace a separate collection-control command family.

An object handle encodes a process generation, object class, and stable object
ID. Its printable form is not a design path. A stale handle fails explicitly;
it never rebinds by name after elaboration, synthesis, or checkpoint
replacement.

SDC queries such as `get_ports`, `get_cells`, `get_pins`, `get_nets`, and
`get_clocks` return the same handle-list representation. They remain because
they are part of SDC. Native shell automation should prefer `get_db` and does
not need `foreach_in_collection`, `sizeof_collection`,
`filter_collection`, or collection-set commands.

## Reports and automation data

Reports keep flat descriptive names:

- `report_area`;
- `report_qor`;
- `report_resources`;
- `report_timing`;
- `report_clock`;
- `report_power`.

Opto does not use report ensembles. A report name is already unambiguous, and
`report_timing` composes naturally with SDC and existing EDA vocabulary.

Where structured data exists, reports share these options:

```tcl
report_timing -format text
report_qor -format json -output build/qor.json
```

`text` is stable human-readable output. `json` is a versioned schema for
automation. `-output` writes atomically; without it the report is the Tcl
result. Scripts use `get_db` for database facts and do not parse human report
text.

Report fields state units explicitly. A field is not published until its
metric exists, and a failed analysis cannot leave a report backed by mixed
revisions.

## Output and checkpoints

Output operations have format-specific names rather than one overloaded
`write` command:

```tcl
write_hdl build/top.v
write_sdc build/top.sdc
save build/top.ock
```

`write_hdl` exports the selected synthesized design. `write_sdc` exports the
selected constraint state. `save` writes a complete Opto checkpoint.

`resume <file>` restores an Opto checkpoint across processes. The checkpoint
is not a netlist-only cache: it preserves design revisions, constraints,
current design, synthesis artifacts, stable IDs, provenance, database
settings, and validated incremental state required to continue work. Liberty
databases remain process-local and must be loaded with `read_libs` before
continuation that depends on them.

Checkpoint decoding validates the schema, frontend and cache ABIs, lengths,
checksums, IDs, and cross-owner references before atomically replacing live
session state. The public names are `save` and `resume`; no external-format
compatibility aliases are retained.

## Scenarios and MMMC

Scenarios are database objects, not another parallel command framework. Once
timing, libraries, parasitics, power, reports, and synthesis consume them end
to end, the flow is:

```tcl
set slow [create_scenario slow]
set_db current_scenario $slow
set_db $slow .libraries [get_db libraries slow_*]
read_sdc constraints/slow.sdc
read_parasitics parasitics/slow.spef
synth
```

`create_scenario` returns the created handle. Selection uses
`get_db current_scenario` and `set_db current_scenario`; no
`current_scenario` or `get_scenarios` command is added. Querying uses
`get_db scenarios`.

No scenario property or command is exposed before it affects the complete
analysis and synthesis flow. Read-only analyses snapshot the current design,
scenario, and relevant database generations before parallel work begins.

## Errors, transactions, and return values

Every persistent command is atomic. A failed command publishes no partial
design, library, constraint, scenario, parasitic, synthesis, analysis, or
checkpoint state.

Errors retain Tcl behavior and add structured codes:

```text
OPTO <command-family> <reason>
```

Source-backed failures include the primary HDL, Liberty, SDC, or parasitic
location and command context. Unsupported commands or arguments fail during
parsing. No command returns success after ignoring requested behavior.

Return values follow a small consistent set:

- queries and creators return object handles or Tcl lists of handles;
- `set_db` returns a changed-target count;
- reports return rendered content;
- read, analysis, check, and write commands return a typed success summary;
- commands that produce a new design revision return its design handle.

## Growth beyond logic synthesis

The same flat model extends without a new executable or a command hierarchy:

```tcl
read_def floorplan.def
place
route
check_cdc
report_cdc
prove_equivalence
report_congestion
```

New domains add database object classes and schema-declared properties rather
than their own variable system. They add an action command only for a real
operation. Internal phases remain internal unless users must independently
control and consume a stable product boundary.

## Opto interface decisions

The public surface makes these deliberate choices:

- `synth` is the only public synthesis operation; internal stages remain
  private;
- reports stay flat rather than using report ensembles;
- `get_db` and `set_db` are schema-typed and do not allow arbitrary database
  mutation;
- object queries return standard Tcl lists of typed handles rather than
  requiring a separate collection-control language;
- elaboration owns hierarchy resolution, so there is no separate `link`
  lifecycle state;
- format-specific output commands replace an overloaded generic `write`;
- no command or option abbreviations, compatibility aliases, inert options,
  fake properties, or missing report fields;
- no automatic name rebinding for stale objects;
- no partial persistent mutation after an error;
- no project mode, manifest build model, or second user-facing executable.

These are reductions in historical debt, not attempts to make the shell
unfamiliar.

## Command cutover

The command inventory is defined atomically by the declarative schema. Tests,
help, completion, examples, and documentation change with that schema. Opto
does not ship deprecated handlers or migration aliases.

## Implementation requirements

The declarative command schema remains the single source for parsing, help,
completion, SDC availability, and inventory tests. A database schema becomes
the single source for root properties, object classes, object properties,
mutability, invalidation, and checkpoint persistence.

Required validation includes:

- exact public command, option, root-property, and object-property inventories;
- database type, lifecycle, mutability, and atomic rollback tests;
- handle identity, ordering, Tcl-list behavior, and stale-handle tests;
- SDC version, sandbox, and query-interoperability tests;
- report text and JSON snapshots;
- Opto checkpoint cross-process continuation tests;
- scenario isolation and generation tests when scenarios are exposed;
- determinism across worker counts;
- repeatable area, timing, power, cell-composition, runtime, and peak-RSS
  benchmarks.

## Rejected alternatives

### Parent commands and subcommands

`design elaborate`, `synth run`, and `report timing` add hierarchy without
clarifying these already-specific operations. Opto keeps flat commands.

### Compatibility-only command retention

Aliases, duplicate lifecycle states, inert flags, and compatibility-only
command families would enlarge the public contract without adding behavior.
Familiarity alone does not justify command-name policy forks.

### Public synthesis stages

Exposing optimization, mapping, and closure as separate commands would make
implementation boundaries into a script contract. Opto publishes the stable
operation, `synth`, and keeps its stages private.

### Unrestricted database mutation

A generic setter without a schema would bypass ownership, validation, and
cache invalidation. Every Opto property is typed and every mutation is
transactional.

### Explicit artifact plumbing

Requiring every command to pass design, scenario, implementation, and analysis
handles is precise but cumbersome for interactive EDA Tcl. Opto uses explicit
database selection and validates handle ownership and revision internally.
