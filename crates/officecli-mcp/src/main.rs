use anyhow::{Context, Result, bail};
use officecli_core::OfficePackage;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    path::Path,
};

fn main() -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line).context("invalid JSON-RPC request")?;
        if request.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let response = handle_request(&request)
            .map(|result| json!({"jsonrpc":"2.0","id":id,"result":result}))
            .unwrap_or_else(|error| {
                json!({
                    "jsonrpc":"2.0",
                    "id":id,
                    "error":{"code":-32603,"message":error.to_string()}
                })
            });
        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle_request(request: &Value) -> Result<Value> {
    match request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "initialize" => Ok(json!({
            "protocolVersion":"2024-11-05",
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"officecli-rs","version":env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools":[{
            "name":"officecli",
            "description":"Inspect and safely edit DOCX, XLSX, and PPTX OOXML packages.",
            "inputSchema":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}
        }]})),
        "tools/call" => {
            let command = request
                .pointer("/params/arguments/command")
                .and_then(Value::as_str)
                .context("tools/call requires arguments.command")?;
            let text = run_command(command)?;
            Ok(json!({"content":[{"type":"text","text":text}]}))
        }
        "ping" => Ok(json!({})),
        method => bail!("unsupported MCP method: {method}"),
    }
}

fn run_command(command: &str) -> Result<String> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    match tokens.as_slice() {
        ["help"] => Ok("officecli summary <file> | raw <file> [part] | get <file> <part> <path> | validate <file>".to_owned()),
        ["summary", path] => {
            let package = load(path)?;
            Ok(serde_json::to_string_pretty(&package.summary()?)?)
        }
        ["validate", path] => {
            load(path)?.validate()?;
            Ok("valid".to_owned())
        }
        ["raw", path] => {
            let package = load(path)?;
            Ok(package.read_xml_part(default_xml_part(&package))?)
        }
        ["raw", path, part] => Ok(load(path)?.read_xml_part(part)?),
        ["get", path, selector] => {
            let nodes = load(path)?.query_path(selector)?;
            Ok(serde_json::to_string_pretty(&nodes)?)
        }
        ["get", path, part, selector] => {
            let nodes = load(path)?.query_xml(part, selector)?;
            Ok(serde_json::to_string_pretty(&nodes)?)
        }
        _ => bail!("unsupported command; use `help` for the MCP command surface"),
    }
}

fn load(path: &str) -> Result<OfficePackage> {
    Ok(OfficePackage::open(std::fs::read(Path::new(path))?)?)
}

fn default_xml_part(package: &OfficePackage) -> &'static str {
    match package.format() {
        officecli_core::OfficeFormat::Docx => "word/document.xml",
        officecli_core::OfficeFormat::Xlsx => "xl/workbook.xml",
        officecli_core::OfficeFormat::Pptx => "ppt/presentation.xml",
    }
}
