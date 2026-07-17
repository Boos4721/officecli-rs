# OfficeCLI Rust

An independent Rust workspace for safe OOXML package automation. It provides
one shared `officecli-core` implementation to the CLI, HTTP service, and MCP
stdio adapter.

The current migration supports DOCX, XLSX, and PPTX package detection,
validated creation with core OOXML relationships, part listing/reading/writing,
XML node selection, attribute/text edits, XML insertion/removal/move/swap,
placeholder merge, batch edits, raw package operations, and text/outline/stats
views. Format-specific advanced handlers, high-fidelity rendering, durable
storage, authentication, and full upstream command parity remain follow-on
work.

## Workspace

- `officecli-core`: bounded ZIP/OOXML package access and shared XML node model.
- `officecli`: local CLI with `create`, `get`, `query`, `set`, `add`, `remove`,
  `move`, `swap`, `view`, `raw`, `raw-set`, `add-part`, `merge`, `dump`,
  `batch`, `validate`, and package inspection commands.
- `officecli-server`: versioned in-memory HTTP API on port `26315`.
- `officecli-mcp`: line-delimited MCP JSON-RPC adapter exposing the shared
  inspection surface.

## Local usage

```bash
cargo test --workspace
cargo run -p officecli -- create report.docx
cargo run -p officecli -- get report.docx '/document/body/p[1]' \
  --part word/document.xml --json
cargo run -p officecli -- set report.docx '/document/body/p[1]/r/t[1]' \
  --part word/document.xml --prop 'text=Updated'
cargo run -p officecli-server
```

XML paths use local element names and 1-based sibling indexes. Use `--part`
to choose the OOXML XML part explicitly. All package and part sizes are
bounded, archive paths are checked for traversal, and mutations rewrite the
package through the same validation path.

## HTTP API

The service exposes `/health`, `/ready`, document creation/inspection,
part listing/read/write/download, plus:

- `GET /v1/office/documents/{id}/view/{mode}` where mode is `text`, `outline`,
  `stats`, or `html`.
- `POST /v1/office/documents/{id}/commands`
- `POST /v1/office/documents/{id}/batch`
- `POST /v1/office/jobs` and `GET /v1/office/jobs/{id}`

Mutation commands use JSON such as:

```json
{
  "command": "set",
  "part": "word/document.xml",
  "path": "/document/body/p[1]/r/t[1]",
  "props": {"text": "Updated"}
}
```

The server persists package snapshots under `OFFICECLI_DATA_DIR` (default
`/tmp/officecli-data`) and restores UUID documents on restart. It still needs
BoosAPI workspace ownership, authentication, and billing integration before
multi-user production traffic. The initial jobs endpoint records synchronous
execution; a durable background Worker is still required for long renders and
resource-heavy operations.

## Release

Tags matching `v*` run the GitHub Actions workflow in
`.github/workflows/release.yml`, which tests the workspace, builds the CLI for
Linux/macOS/Windows targets, publishes checksummed release assets, and builds a
GHCR container for the HTTP service. A real online deployment still requires
server, domain, database/object-storage, and CI credentials.

The project is Apache-2.0 licensed. `NOTICE` preserves attribution to the
upstream OfficeCLI project at <https://github.com/iOfficeAI/OfficeCLI>.
