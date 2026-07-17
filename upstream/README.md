# Upstream C# to Rust Port

The port-upstream-csharp-to-rust GitHub Actions workflow periodically fetches
the maintained C# OfficeCLI source from:

<https://github.com/iOfficeAI/OfficeCLI>

It stores the source snapshot under upstream/officecli-csharp, then generates
the compileable Rust compatibility layer and port report under
upstream/officecli-rust. It updates the
automation/port-upstream-csharp-to-rust branch instead of changing the Rust
implementation directly. Create or update a review PR from that branch and
review the generated missing-command list before implementing behavior in
officecli-core.
