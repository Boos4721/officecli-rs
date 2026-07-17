# officecli

Rust CLI for inspecting and editing DOCX, XLSX, and PPTX OOXML packages.

## Install

```bash
cargo install officecli
```

## Quick Start

```bash
officecli create report.docx
officecli view report.docx text
officecli add report.docx /body --type paragraph --prop 'text=Hello from Rust'
officecli get report.docx '/body/p[1]' --json
officecli validate report.docx
```

Semantic paths use the upstream OfficeCLI convention:

- Word: `/body/p[1]`, `/body/p[1]/r[1]/t[1]`
- Excel: `/Sheet1/A1`
- PowerPoint: `/slide[1]`

Use `--part` when operating on a specific OOXML XML part. Use `--xml` with
`add` when a format-specific fragment is required.

## Commands

The current CLI includes:

```text
create inspect get query set add remove remove-part move swap view
raw raw-set add-part set-part merge dump batch validate open save close
list-parts get-part
```

Examples:

```bash
officecli set report.docx '/body/p[1]/r[1]/t[1]' --prop 'text=Updated'
officecli add workbook.xlsx /Sheet1 --type cell --prop ref=A2 --prop value=42
officecli add slides.pptx '/slide[1]' --type shape --prop 'text=Quarterly results'
officecli batch report.docx --commands '[{"command":"set","path":"/body/p[1]/r[1]/t[1]","props":{"text":"Done"}}]'
officecli view report.docx html > report.html
```

`officecli-core` applies package size limits, rejects unsafe ZIP part names,
validates XML parts, and performs mutations through a validated package
rewrite. Advanced Office semantics such as formulas, pivots, charts, and
high-fidelity rendering are being ported incrementally.

Repository: <https://github.com/Boos4721/officecli-rs>
