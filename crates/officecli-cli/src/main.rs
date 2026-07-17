use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use officecli_core::{DocumentNode, OfficeFormat, OfficePackage};
use quick_xml::escape::escape;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(
    name = "officecli",
    version,
    about = "Rust Office document automation CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Create {
        output: PathBuf,
        #[arg(value_enum)]
        format: Option<FormatArg>,
    },
    Inspect {
        input: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Get {
        input: PathBuf,
        path: String,
        #[arg(long)]
        part: Option<String>,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    Query {
        input: PathBuf,
        selector: String,
        #[arg(long)]
        part: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Set {
        input: PathBuf,
        path: String,
        #[arg(long = "prop")]
        properties: Vec<String>,
        #[arg(long)]
        part: Option<String>,
        #[arg(long)]
        find: Option<String>,
        #[arg(long)]
        replace: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    AddPart {
        input: PathBuf,
        name: String,
        content: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Remove {
        input: PathBuf,
        path: String,
        #[arg(long)]
        part: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    RemovePart {
        input: PathBuf,
        name: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Add {
        input: PathBuf,
        path: String,
        #[arg(long = "type")]
        element_type: String,
        #[arg(long = "prop")]
        properties: Vec<String>,
        #[arg(long)]
        part: Option<String>,
        #[arg(long)]
        xml: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Move {
        input: PathBuf,
        source: String,
        target: String,
        #[arg(long)]
        part: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Swap {
        input: PathBuf,
        first: String,
        second: String,
        #[arg(long)]
        part: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    View {
        input: PathBuf,
        mode: ViewMode,
        #[arg(long)]
        json: bool,
    },
    Raw {
        input: PathBuf,
        #[arg(long)]
        part: Option<String>,
    },
    RawSet {
        input: PathBuf,
        #[arg(long)]
        part: String,
        xml: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Merge {
        input: PathBuf,
        output: PathBuf,
        values: String,
    },
    Dump {
        input: PathBuf,
        #[arg(long)]
        part: Option<String>,
    },
    Batch {
        input: PathBuf,
        #[arg(long)]
        commands: Option<String>,
        #[arg(long)]
        input_json: Option<PathBuf>,
        #[arg(long)]
        stop_on_error: bool,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Validate {
        input: PathBuf,
    },
    Open {
        input: PathBuf,
    },
    Save {
        input: PathBuf,
    },
    Close {
        input: PathBuf,
    },
    ListParts {
        input: PathBuf,
    },
    GetPart {
        input: PathBuf,
        name: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    SetPart {
        input: PathBuf,
        name: String,
        content: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FormatArg {
    Docx,
    Xlsx,
    Pptx,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ViewMode {
    Text,
    Outline,
    Stats,
    Annotated,
    Html,
}

impl From<FormatArg> for OfficeFormat {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Docx => Self::Docx,
            FormatArg::Xlsx => Self::Xlsx,
            FormatArg::Pptx => Self::Pptx,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BatchCommand {
    #[serde(alias = "op")]
    command: String,
    part: Option<String>,
    path: Option<String>,
    name: Option<String>,
    source: Option<String>,
    target: Option<String>,
    first: Option<String>,
    second: Option<String>,
    xml: Option<String>,
    content: Option<String>,
    #[serde(default)]
    props: BTreeMap<String, String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Create { output, format } => {
            let format = format
                .map(Into::into)
                .or_else(|| format_from_path(&output))
                .context("create requires --format or a .docx/.xlsx/.pptx extension")?;
            write_package(&output, OfficePackage::create(format)?)?;
            println!("created {}", output.display());
        }
        Command::Inspect { input, json } => {
            let summary = load(&input)?.summary()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!(
                    "format: {:?}\nbytes: {}\nparts: {}",
                    summary.format,
                    summary.bytes,
                    summary.parts.len()
                );
            }
        }
        Command::Get {
            input,
            path,
            part,
            depth: _,
            json,
        }
        | Command::Query {
            input,
            selector: path,
            part,
            json,
        } => {
            let package = load(&input)?;
            let nodes = match part {
                Some(part) => package.query_xml(&part, &path)?,
                None => package.query_path(&path)?,
            };
            print_nodes(&nodes, json)?;
        }
        Command::Set {
            input,
            path,
            properties,
            part,
            find,
            replace,
            output,
        } => {
            let package = load(&input)?;
            let changed = if let (Some(find), Some(replace)) = (find, replace) {
                let mut values = BTreeMap::new();
                values.insert(find, replace);
                package.merge_text(&values)?
            } else if let Some(part) = part {
                package.set_xml(&part, &path, &parse_properties(&properties)?)?
            } else {
                package.set_path(&path, &parse_properties(&properties)?)?
            };
            write_package(&output.unwrap_or(input), changed)?;
        }
        Command::AddPart {
            input,
            name,
            content,
            output,
        }
        | Command::SetPart {
            input,
            name,
            content,
            output,
        } => {
            let package = load(&input)?.with_part(&name, &fs::read(&content)?)?;
            write_package(&output.unwrap_or(input), package)?;
        }
        Command::Remove {
            input,
            path,
            part,
            output,
        } => {
            let package = load(&input)?;
            let package = if let Some(part) = part {
                package.remove_xml(&part, &path)?
            } else {
                package.remove_path(&path)?
            };
            write_package(&output.unwrap_or(input), package)?;
        }
        Command::RemovePart {
            input,
            name,
            output,
        } => {
            let package = load(&input)?.remove_part(&name)?;
            write_package(&output.unwrap_or(input), package)?;
        }
        Command::Add {
            input,
            path,
            element_type,
            properties,
            part,
            xml,
            output,
        } => {
            let package = load(&input)?;
            let props = parse_properties(&properties)?;
            let fragment = if let Some(xml) = xml {
                fs::read_to_string(xml)?
            } else {
                build_fragment(package.format(), &element_type, &props)?
            };
            write_package(
                &output.unwrap_or(input),
                match part {
                    Some(part) => package.insert_xml(&part, &path, &fragment)?,
                    None => package.insert_path(&path, &fragment)?,
                },
            )?;
        }
        Command::Move {
            input,
            source,
            target,
            part,
            output,
        } => {
            let package = load(&input)?;
            write_package(
                &output.unwrap_or(input),
                match part {
                    Some(part) => package.move_xml(&part, &source, &target)?,
                    None => {
                        let (source_part, source_path) = package.resolve_path(&source)?;
                        let (_, target_path) = package.resolve_path(&target)?;
                        package.move_xml(&source_part, &source_path, &target_path)?
                    }
                },
            )?;
        }
        Command::Swap {
            input,
            first,
            second,
            part,
            output,
        } => {
            let package = load(&input)?;
            write_package(
                &output.unwrap_or(input),
                match part {
                    Some(part) => package.swap_xml(&part, &first, &second)?,
                    None => {
                        let (first_part, first_path) = package.resolve_path(&first)?;
                        let (_, second_path) = package.resolve_path(&second)?;
                        package.swap_xml(&first_part, &first_path, &second_path)?
                    }
                },
            )?;
        }
        Command::View { input, mode, json } => {
            let package = load(&input)?;
            match mode {
                ViewMode::Text | ViewMode::Annotated => println!("{}", package.text_content()?),
                ViewMode::Outline => {
                    let part = default_xml_part(package.format());
                    let nodes = package.query_xml(part, "/")?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&nodes)?);
                    } else {
                        print_nodes(&nodes, false)?;
                    }
                }
                ViewMode::Stats => {
                    let summary = package.summary()?;
                    let text = package.text_content()?;
                    let stats = serde_json::json!({
                        "format": summary.format,
                        "bytes": summary.bytes,
                        "parts": summary.parts.len(),
                        "characters": text.chars().count(),
                        "words": text.split_whitespace().count()
                    });
                    if json {
                        println!("{}", serde_json::to_string_pretty(&stats)?);
                    } else {
                        println!("{}", stats);
                    }
                }
                ViewMode::Html => {
                    let raw_text = package.text_content()?;
                    let text = escape(&raw_text);
                    println!(
                        "<!doctype html><meta charset=\"utf-8\"><title>OfficeCLI preview</title><pre>{text}</pre>"
                    );
                }
            }
        }
        Command::Raw { input, part } => {
            let package = load(&input)?;
            let name = part.unwrap_or_else(|| default_xml_part(package.format()).to_owned());
            print!("{}", package.read_xml_part(&name)?);
        }
        Command::RawSet {
            input,
            part,
            xml,
            output,
        } => {
            let package = load(&input)?.with_part(&part, &fs::read(&xml)?)?;
            write_package(&output.unwrap_or(input), package)?;
        }
        Command::Merge {
            input,
            output,
            values,
        } => {
            let package = load(&input)?;
            let values = read_json_object(&values)?;
            write_package(&output, package.merge_text(&values)?)?;
        }
        Command::Dump { input, part } => {
            let package = load(&input)?;
            if let Some(part) = part {
                println!("{}", package.read_xml_part(&part)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&package.summary()?)?);
            }
        }
        Command::Batch {
            input,
            commands,
            input_json,
            stop_on_error,
            json,
            output,
        } => {
            let source = if let Some(commands) = commands {
                commands
            } else if let Some(input_json) = input_json {
                fs::read_to_string(input_json)?
            } else {
                let mut source = String::new();
                io::stdin().read_to_string(&mut source)?;
                source
            };
            let commands: Vec<BatchCommand> =
                serde_json::from_str(&source).context("batch input must be a JSON array")?;
            let mut package = load(&input)?;
            let mut errors = Vec::new();
            for (index, command) in commands.into_iter().enumerate() {
                let result = apply_batch_command(&package, command);
                match result {
                    Ok(changed) => package = changed,
                    Err(error) => {
                        errors.push(serde_json::json!({"index":index,"error":error.to_string()}));
                        if stop_on_error {
                            break;
                        }
                    }
                }
            }
            write_package(&output.unwrap_or(input), package)?;
            if json {
                println!("{}", serde_json::json!({"errors": errors}));
            } else if !errors.is_empty() {
                for error in errors {
                    eprintln!("batch error: {}", error);
                }
                bail!("batch completed with errors");
            }
        }
        Command::Validate { input }
        | Command::Open { input }
        | Command::Save { input }
        | Command::Close { input } => {
            load(&input)?.validate()?;
            println!("valid {}", input.display());
        }
        Command::ListParts { input } => {
            for part in load(&input)?.summary()?.parts {
                println!("{}\t{} bytes", part.name, part.bytes);
            }
        }
        Command::GetPart {
            input,
            name,
            output,
        } => {
            let bytes = load(&input)?.read_part(&name)?;
            if let Some(output) = output {
                fs::write(output, bytes)?;
            } else {
                print!("{}", String::from_utf8_lossy(&bytes));
            }
        }
    }
    Ok(())
}

fn apply_batch_command(package: &OfficePackage, command: BatchCommand) -> Result<OfficePackage> {
    let explicit_part = command.part.clone();
    let part = command
        .part
        .unwrap_or_else(|| default_xml_part(package.format()).to_owned());
    match command.command.as_str() {
        "set" => {
            let path = command.path.unwrap_or_else(|| "/".to_owned());
            Ok(if explicit_part.is_none() {
                package.set_path(&path, &command.props)?
            } else {
                package.set_xml(&part, &path, &command.props)?
            })
        }
        "raw-set" => Ok(package.with_part(&part, command.xml.unwrap_or_default().as_bytes())?),
        "add-part" => Ok(package.with_part(
            &command.name.unwrap_or(part),
            command
                .content
                .or(command.xml)
                .unwrap_or_default()
                .as_bytes(),
        )?),
        "add" => {
            let path = command.path.unwrap_or_else(|| "/".to_owned());
            let fragment = command.content.or(command.xml).unwrap_or_default();
            Ok(if explicit_part.is_none() {
                package.insert_path(&path, &fragment)?
            } else {
                package.insert_xml(&part, &path, &fragment)?
            })
        }
        "remove" => match command.path {
            Some(path) if explicit_part.is_none() => Ok(package.remove_path(&path)?),
            Some(path) => Ok(package.remove_xml(&part, &path)?),
            None => Ok(package.remove_part(&command.name.unwrap_or(part))?),
        },
        "move" => Ok(package.move_xml(
            &part,
            &command
                .source
                .or(command.path)
                .context("move requires source or path")?,
            &command.target.context("move requires target")?,
        )?),
        "swap" => Ok(package.swap_xml(
            &part,
            &command.first.context("swap requires first")?,
            &command.second.context("swap requires second")?,
        )?),
        "merge" => {
            let values: BTreeMap<String, String> = command.props;
            Ok(package.merge_text(&values)?)
        }
        other => bail!("unsupported batch command: {other}"),
    }
}

fn build_fragment(
    format: OfficeFormat,
    element_type: &str,
    properties: &BTreeMap<String, String>,
) -> Result<String> {
    let text = escape(
        properties
            .get("text")
            .map(String::as_str)
            .unwrap_or_default(),
    );
    match (format, element_type.to_ascii_lowercase().as_str()) {
        (OfficeFormat::Docx, "paragraph" | "para") => Ok(format!(
            "<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:r><w:t>{text}</w:t></w:r></w:p>"
        )),
        (OfficeFormat::Docx, "run") => Ok(format!(
            "<w:r xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:t>{text}</w:t></w:r>"
        )),
        (OfficeFormat::Xlsx, "cell") => {
            let reference = properties
                .get("ref")
                .map(|value| format!(" r=\"{}\"", escape(value)))
                .unwrap_or_default();
            let value = properties
                .get("value")
                .or_else(|| properties.get("text"))
                .map(String::as_str)
                .unwrap_or_default();
            Ok(format!(
                "<c xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"{reference} t=\"inlineStr\"><is><t>{}</t></is></c>",
                escape(value)
            ))
        }
        (OfficeFormat::Xlsx, "row") => Ok(format!(
            "<row xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><c><v>{text}</v></c></row>"
        )),
        (OfficeFormat::Pptx, "shape" | "textbox") => Ok(format!(
            "<p:sp xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"><p:nvSpPr><p:cNvPr id=\"2\" name=\"TextBox 2\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\" sz=\"1800\"/><a:t>{text}</a:t></a:r><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>"
        )),
        _ => bail!(
            "element type `{element_type}` requires --xml with a format-specific OOXML fragment"
        ),
    }
}

fn print_nodes(nodes: &[DocumentNode], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(nodes)?);
    } else {
        for node in nodes {
            println!(
                "{} ({}) {:?}{}",
                node.path,
                node.tag,
                node.attributes,
                node.text
                    .as_deref()
                    .map(|text| format!(" \"{text}\""))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

fn parse_properties(properties: &[String]) -> Result<BTreeMap<String, String>> {
    properties
        .iter()
        .map(|property| {
            property
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .with_context(|| format!("property must use key=value: {property}"))
        })
        .collect()
}

fn read_json_object(value: &str) -> Result<BTreeMap<String, String>> {
    let source = fs::read_to_string(value).unwrap_or_else(|_| value.to_owned());
    Ok(serde_json::from_str(&source).context("merge values must be a JSON object or file")?)
}

fn write_package(path: &PathBuf, package: OfficePackage) -> Result<()> {
    fs::write(path, package.bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn load(path: &PathBuf) -> Result<OfficePackage> {
    Ok(OfficePackage::open(fs::read(path)?)?)
}

fn format_from_path(path: &PathBuf) -> Option<OfficeFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "docx" => Some(OfficeFormat::Docx),
        "xlsx" => Some(OfficeFormat::Xlsx),
        "pptx" => Some(OfficeFormat::Pptx),
        _ => None,
    }
}

fn default_xml_part(format: OfficeFormat) -> &'static str {
    match format {
        OfficeFormat::Docx => "word/document.xml",
        OfficeFormat::Xlsx => "xl/workbook.xml",
        OfficeFormat::Pptx => "ppt/presentation.xml",
    }
}
