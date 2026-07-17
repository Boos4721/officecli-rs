//! Safe, format-neutral OOXML package primitives.
//!
//! DOCX, XLSX and PPTX are ZIP packages containing XML parts. Keeping package
//! access separate from format-specific handlers gives the CLI, HTTP service,
//! and MCP adapter one deterministic foundation.

pub mod upstream_compat;

use quick_xml::Reader;
use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use thiserror::Error;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_PART_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OfficeFormat {
    Docx,
    Xlsx,
    Pptx,
}

impl OfficeFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSummary {
    pub format: OfficeFormat,
    pub bytes: usize,
    pub parts: Vec<PartSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartSummary {
    pub name: String,
    pub bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentNode {
    pub tag: String,
    pub path: String,
    pub attributes: BTreeMap<String, String>,
    pub text: Option<String>,
    pub children: Vec<DocumentNode>,
}

#[derive(Debug, Error)]
pub enum OfficeError {
    #[error("package exceeds the {MAX_PACKAGE_BYTES} byte limit")]
    PackageTooLarge,
    #[error("part exceeds the {MAX_PART_BYTES} byte limit: {0}")]
    PartTooLarge(String),
    #[error("not a supported Office package")]
    UnsupportedPackage,
    #[error("invalid package: {0}")]
    InvalidPackage(String),
    #[error("part not found: {0}")]
    PartNotFound(String),
    #[error("invalid part name")]
    InvalidPartName,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML error: {0}")]
    Xml(String),
}

#[derive(Debug, Clone)]
pub struct OfficePackage {
    bytes: Vec<u8>,
    format: OfficeFormat,
}

impl OfficePackage {
    pub fn open(bytes: Vec<u8>) -> Result<Self, OfficeError> {
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(OfficeError::PackageTooLarge);
        }
        let format = detect_format(&bytes)?;
        let package = Self { bytes, format };
        package.validate()?;
        Ok(package)
    }

    pub fn create(format: OfficeFormat) -> Result<Self, OfficeError> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            let options = SimpleFileOptions::default();
            for (name, content) in minimal_parts(format) {
                writer.start_file(name, options)?;
                writer.write_all(content.as_bytes())?;
            }
            writer.finish()?;
        }
        Self::open(output.into_inner())
    }

    pub fn format(&self) -> OfficeFormat {
        self.format
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn summary(&self) -> Result<PackageSummary, OfficeError> {
        let mut archive = ZipArchive::new(Cursor::new(&self.bytes))?;
        let mut parts = Vec::with_capacity(archive.len());
        let mut total_uncompressed = 0_u64;
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            validate_part_name(entry.name())?;
            if entry.size() > MAX_PART_BYTES as u64 {
                return Err(OfficeError::PartTooLarge(entry.name().to_owned()));
            }
            total_uncompressed = total_uncompressed
                .checked_add(entry.size())
                .ok_or(OfficeError::PackageTooLarge)?;
            if total_uncompressed > MAX_PACKAGE_BYTES as u64 {
                return Err(OfficeError::PackageTooLarge);
            }
            parts.push(PartSummary {
                name: entry.name().to_owned(),
                bytes: entry.size(),
                compressed_bytes: entry.compressed_size(),
            });
        }
        Ok(PackageSummary {
            format: self.format,
            bytes: self.bytes.len(),
            parts,
        })
    }

    pub fn read_part(&self, name: &str) -> Result<Vec<u8>, OfficeError> {
        validate_part_name(name)?;
        let mut archive = ZipArchive::new(Cursor::new(&self.bytes))?;
        let mut entry = archive
            .by_name(name)
            .map_err(|_| OfficeError::PartNotFound(name.to_owned()))?;
        if entry.size() as usize > MAX_PART_BYTES {
            return Err(OfficeError::PartTooLarge(name.to_owned()));
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_xml_part(&self, name: &str) -> Result<String, OfficeError> {
        let bytes = self.read_part(name)?;
        let mut reader = Reader::from_reader(bytes.as_slice());
        let mut buffer = Vec::new();
        loop {
            match reader.read_event_into(&mut buffer) {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => buffer.clear(),
                Err(error) => return Err(OfficeError::Xml(error.to_string())),
            }
        }
        String::from_utf8(bytes).map_err(|error| OfficeError::Xml(error.to_string()))
    }

    pub fn with_part(&self, name: &str, content: &[u8]) -> Result<Self, OfficeError> {
        validate_part_name(name)?;
        if content.len() > MAX_PART_BYTES {
            return Err(OfficeError::PartTooLarge(name.to_owned()));
        }
        let mut source = ZipArchive::new(Cursor::new(&self.bytes))?;
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            let options = SimpleFileOptions::default();
            let mut replaced = false;
            for index in 0..source.len() {
                let mut entry = source.by_index(index)?;
                let entry_name = entry.name().to_owned();
                if entry_name == name {
                    writer.start_file(&entry_name, options)?;
                    writer.write_all(content)?;
                    replaced = true;
                } else {
                    let mut bytes = Vec::new();
                    entry.read_to_end(&mut bytes)?;
                    writer.start_file(&entry_name, options)?;
                    writer.write_all(&bytes)?;
                }
            }
            if !replaced {
                writer.start_file(name, options)?;
                writer.write_all(content)?;
            }
            writer.finish()?;
        }
        Self::open(output.into_inner())
    }

    pub fn remove_part(&self, name: &str) -> Result<Self, OfficeError> {
        validate_part_name(name)?;
        let mut source = ZipArchive::new(Cursor::new(&self.bytes))?;
        if source.by_name(name).is_err() {
            return Err(OfficeError::PartNotFound(name.to_owned()));
        }
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            let options = SimpleFileOptions::default();
            for index in 0..source.len() {
                let mut entry = source.by_index(index)?;
                if entry.name() == name {
                    continue;
                }
                let entry_name = entry.name().to_owned();
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                writer.start_file(entry_name, options)?;
                writer.write_all(&content)?;
            }
            writer.finish()?;
        }
        Self::open(output.into_inner())
    }

    pub fn query_xml(&self, part: &str, path: &str) -> Result<Vec<DocumentNode>, OfficeError> {
        let xml = self.read_xml_part(part)?;
        let document =
            Document::parse(&xml).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let nodes = select_nodes(document.root_element(), path)?;
        Ok(nodes.into_iter().map(node_to_document_node).collect())
    }

    pub fn set_xml(
        &self,
        part: &str,
        path: &str,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, OfficeError> {
        let original = self.read_xml_part(part)?;
        let document =
            Document::parse(&original).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let ranges: Vec<_> = select_nodes(document.root_element(), path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        if ranges.is_empty() {
            return Err(OfficeError::InvalidPackage(format!(
                "XML path matched no nodes: {path}"
            )));
        }
        drop(document);
        let mut changed = original;
        for range in ranges.into_iter().rev() {
            changed = apply_xml_properties(&changed, range, properties)?;
        }
        self.with_part(part, changed.as_bytes())
    }

    pub fn merge_text(&self, values: &BTreeMap<String, String>) -> Result<Self, OfficeError> {
        let mut package = self.clone();
        for part in self.summary()?.parts {
            if !part.name.ends_with(".xml") {
                continue;
            }
            let mut content = String::from_utf8(package.read_part(&part.name)?)
                .map_err(|error| OfficeError::Xml(error.to_string()))?;
            for (key, value) in values {
                content = content.replace(&format!("{{{{{key}}}}}"), value);
            }
            package = package.with_part(&part.name, content.as_bytes())?;
        }
        Ok(package)
    }

    pub fn text_content(&self) -> Result<String, OfficeError> {
        let mut text = Vec::new();
        for part in self.summary()?.parts {
            if !part.name.ends_with(".xml")
                || part.name == "[Content_Types].xml"
                || part.name.contains("/_rels/")
                || part.name.starts_with("_rels/")
            {
                continue;
            }
            let xml = self.read_xml_part(&part.name)?;
            let document =
                Document::parse(&xml).map_err(|error| OfficeError::Xml(error.to_string()))?;
            text.extend(
                document
                    .descendants()
                    .filter(|node| node.is_text())
                    .filter_map(|node| node.text())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            );
        }
        Ok(text.join(" "))
    }

    pub fn resolve_path(&self, path: &str) -> Result<(String, String), OfficeError> {
        let path = if path.is_empty() { "/" } else { path };
        match self.format {
            OfficeFormat::Docx => {
                let xml_path = if path == "/" || path.starts_with("/document") {
                    path.to_owned()
                } else {
                    format!("/document{path}")
                };
                Ok(("word/document.xml".to_owned(), xml_path))
            }
            OfficeFormat::Xlsx => self.resolve_xlsx_path(path),
            OfficeFormat::Pptx => {
                if let Some(index) = parse_indexed_segment(path, "slide") {
                    let marker = format!("/slide[{index}]");
                    let suffix = path.strip_prefix(&marker).unwrap_or_default();
                    let xml_path = if suffix.is_empty() {
                        "/sld".to_owned()
                    } else if suffix.starts_with("/shape") || suffix.starts_with("/sp") {
                        format!("/sld/cSld/spTree{suffix}")
                    } else {
                        format!("/sld{suffix}")
                    };
                    Ok((format!("ppt/slides/slide{index}.xml"), xml_path))
                } else {
                    Ok(("ppt/presentation.xml".to_owned(), path.to_owned()))
                }
            }
        }
    }

    pub fn query_path(&self, path: &str) -> Result<Vec<DocumentNode>, OfficeError> {
        let (part, xml_path) = self.resolve_path(path)?;
        self.query_xml(&part, &xml_path)
    }

    pub fn set_path(
        &self,
        path: &str,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, OfficeError> {
        let (part, xml_path) = self.resolve_path(path)?;
        if self.format == OfficeFormat::Xlsx && xml_path.contains("/c[") {
            let mut cell_properties = properties.clone();
            if let Some(value) = cell_properties.remove("value") {
                cell_properties.insert("text".to_owned(), value);
            }
            if cell_properties.contains_key("text") {
                let inline_path = format!("{xml_path}/is/t");
                if !self.query_xml(&part, &inline_path)?.is_empty() {
                    return self.set_xml(&part, &inline_path, &cell_properties);
                }
                let value_path = format!("{xml_path}/v");
                if !self.query_xml(&part, &value_path)?.is_empty() {
                    return self.set_xml(&part, &value_path, &cell_properties);
                }
            }
        }
        self.set_xml(&part, &xml_path, properties)
    }

    pub fn insert_path(&self, path: &str, fragment: &str) -> Result<Self, OfficeError> {
        let (part, mut xml_path) = self.resolve_path(path)?;
        if self.format == OfficeFormat::Xlsx && xml_path == "/worksheet" {
            xml_path = "/worksheet/sheetData".to_owned();
        }
        if self.format == OfficeFormat::Pptx && xml_path == "/sld" {
            xml_path = "/sld/cSld/spTree".to_owned();
        }
        let fragment = if self.format == OfficeFormat::Xlsx
            && xml_path == "/worksheet/sheetData"
            && fragment.trim_start().starts_with("<c")
        {
            format!("<row r=\"1\">{fragment}</row>")
        } else {
            fragment.to_owned()
        };
        self.insert_xml(&part, &xml_path, &fragment)
    }

    pub fn remove_path(&self, path: &str) -> Result<Self, OfficeError> {
        let (part, xml_path) = self.resolve_path(path)?;
        self.remove_xml(&part, &xml_path)
    }

    fn resolve_xlsx_path(&self, path: &str) -> Result<(String, String), OfficeError> {
        let token = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        if token.is_empty() || token == "workbook" {
            return Ok(("xl/workbook.xml".to_owned(), "/workbook".to_owned()));
        }
        let workbook_xml = self.read_xml_part("xl/workbook.xml")?;
        let workbook =
            Document::parse(&workbook_xml).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let Some(sheet) = workbook
            .descendants()
            .find(|node| node.has_tag_name("sheet") && node.attribute("name") == Some(token))
        else {
            return Err(OfficeError::InvalidPackage(format!(
                "worksheet not found: {token}"
            )));
        };
        let relationship_id = sheet
            .attributes()
            .find(|attribute| attribute.name() == "id" || attribute.name() == "r:id")
            .map(|attribute| attribute.value().to_owned())
            .ok_or_else(|| {
                OfficeError::InvalidPackage(format!("worksheet has no relationship: {token}"))
            })?;
        let rels_xml = self.read_xml_part("xl/_rels/workbook.xml.rels")?;
        let rels =
            Document::parse(&rels_xml).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let target = rels
            .descendants()
            .find(|node| {
                node.has_tag_name("Relationship")
                    && node.attribute("Id") == Some(relationship_id.as_str())
            })
            .and_then(|node| node.attribute("Target"))
            .ok_or_else(|| {
                OfficeError::InvalidPackage(format!("worksheet relationship not found: {token}"))
            })?;
        let part = target
            .trim_start_matches('/')
            .strip_prefix("xl/")
            .map_or_else(|| format!("xl/{target}"), |rest| format!("xl/{rest}"));
        let marker = format!("/{token}");
        let suffix = path.strip_prefix(&marker).unwrap_or_default();
        let xml_path = if let Some(cell) = suffix.strip_prefix('/') {
            if is_cell_reference(cell) {
                let row =
                    cell.trim_start_matches(|character: char| character.is_ascii_alphabetic());
                format!("/worksheet/sheetData/row[@r='{row}']/c[@r='{cell}']")
            } else {
                format!("/worksheet/{cell}")
            }
        } else {
            "/worksheet".to_owned()
        };
        Ok((part, xml_path))
    }

    pub fn insert_xml(
        &self,
        part: &str,
        parent_path: &str,
        fragment: &str,
    ) -> Result<Self, OfficeError> {
        if fragment.trim().is_empty() {
            return Err(OfficeError::Xml("XML fragment cannot be empty".into()));
        }
        let original = self.read_xml_part(part)?;
        let document =
            Document::parse(&original).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let ranges: Vec<_> = select_nodes(document.root_element(), parent_path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        if ranges.is_empty() {
            return Err(OfficeError::InvalidPackage(format!(
                "XML path matched no nodes: {parent_path}"
            )));
        }
        if ranges.len() > 1 {
            return Err(OfficeError::InvalidPackage(format!(
                "XML path matched multiple parents: {parent_path}"
            )));
        }
        let range = ranges[0].clone();
        let source = &original[range.clone()];
        let closing = source
            .rfind("</")
            .ok_or_else(|| OfficeError::Xml("XML parent must have a closing tag".into()))?;
        let mut changed = original;
        changed.insert_str(range.start + closing, fragment);
        self.with_part(part, changed.as_bytes())
    }

    pub fn remove_xml(&self, part: &str, path: &str) -> Result<Self, OfficeError> {
        let original = self.read_xml_part(part)?;
        let document =
            Document::parse(&original).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let mut ranges: Vec<_> = select_nodes(document.root_element(), path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        if ranges.is_empty() {
            return Err(OfficeError::InvalidPackage(format!(
                "XML path matched no nodes: {path}"
            )));
        }
        ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
        let mut changed = original;
        for range in ranges {
            changed.replace_range(range, "");
        }
        self.with_part(part, changed.as_bytes())
    }

    pub fn move_xml(
        &self,
        part: &str,
        source_path: &str,
        target_path: &str,
    ) -> Result<Self, OfficeError> {
        let original = self.read_xml_part(part)?;
        let document =
            Document::parse(&original).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let source_ranges: Vec<_> = select_nodes(document.root_element(), source_path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        if source_ranges.len() != 1 {
            return Err(OfficeError::InvalidPackage(format!(
                "source path must match exactly one node: {source_path}"
            )));
        }
        let fragment = original[source_ranges[0].clone()].to_owned();
        let removed = self.remove_xml(part, source_path)?;
        removed.insert_xml(part, target_path, &fragment)
    }

    pub fn swap_xml(
        &self,
        part: &str,
        first_path: &str,
        second_path: &str,
    ) -> Result<Self, OfficeError> {
        let original = self.read_xml_part(part)?;
        let document =
            Document::parse(&original).map_err(|error| OfficeError::Xml(error.to_string()))?;
        let first: Vec<_> = select_nodes(document.root_element(), first_path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        let second: Vec<_> = select_nodes(document.root_element(), second_path)?
            .into_iter()
            .map(|node| node.range())
            .collect();
        if first.len() != 1 || second.len() != 1 {
            return Err(OfficeError::InvalidPackage(
                "swap paths must each match exactly one node".into(),
            ));
        }
        let first = first[0].clone();
        let second = second[0].clone();
        if first == second || first.start < second.end && second.start < first.end {
            return Err(OfficeError::InvalidPackage(
                "swap paths must identify distinct, non-overlapping nodes".into(),
            ));
        }
        let first_xml = original[first.clone()].to_owned();
        let second_xml = original[second.clone()].to_owned();
        let (later, later_xml, earlier, earlier_xml) = if first.start < second.start {
            (second, first_xml, first, second_xml)
        } else {
            (first, second_xml, second, first_xml)
        };
        let mut changed = original;
        changed.replace_range(later, &later_xml);
        changed.replace_range(earlier, &earlier_xml);
        self.with_part(part, changed.as_bytes())
    }

    pub fn validate(&self) -> Result<(), OfficeError> {
        let summary = self.summary()?;
        let required = ["[Content_Types].xml", "_rels/.rels"];
        for part in required {
            if !summary.parts.iter().any(|item| item.name == part) {
                return Err(OfficeError::InvalidPackage(format!(
                    "missing required part {part}"
                )));
            }
        }
        let format_part = match self.format {
            OfficeFormat::Docx => "word/document.xml",
            OfficeFormat::Xlsx => "xl/workbook.xml",
            OfficeFormat::Pptx => "ppt/presentation.xml",
        };
        if !summary.parts.iter().any(|item| item.name == format_part) {
            return Err(OfficeError::InvalidPackage(format!(
                "missing format part {format_part}"
            )));
        }
        for part in &summary.parts {
            if part.name.ends_with(".xml") {
                self.read_xml_part(&part.name)?;
            }
        }
        Ok(())
    }
}

fn select_nodes<'a>(root: Node<'a, 'a>, path: &str) -> Result<Vec<Node<'a, 'a>>, OfficeError> {
    let mut segments = path
        .trim()
        .trim_start_matches('/')
        .split('/')
        .filter(|item| !item.is_empty());
    let Some(first) = segments.next() else {
        return Ok(vec![root]);
    };
    let mut current = if segment_matches(root, first)? {
        vec![root]
    } else {
        vec![root]
            .into_iter()
            .flat_map(|node| node.children().filter(|child| child.is_element()))
            .filter(|node| segment_matches(*node, first).unwrap_or(false))
            .collect()
    };
    for segment in segments {
        let mut next = Vec::new();
        for parent in current {
            let matches: Vec<_> = parent
                .children()
                .filter(|child| child.is_element())
                .filter(|child| segment_matches(*child, segment).unwrap_or(false))
                .collect();
            next.extend(matches);
        }
        current = next;
    }
    Ok(current)
}

fn parse_indexed_segment(path: &str, name: &str) -> Option<usize> {
    let prefix = format!("/{name}[");
    let value = path.strip_prefix(&prefix)?.split_once(']')?.0;
    value.parse().ok()
}

fn is_cell_reference(value: &str) -> bool {
    let mut saw_column = false;
    let mut saw_row = false;
    for character in value.chars() {
        if character.is_ascii_alphabetic() && !saw_row {
            saw_column = true;
        } else if character.is_ascii_digit() && saw_column {
            saw_row = true;
        } else {
            return false;
        }
    }
    saw_column && saw_row
}

fn segment_matches(node: Node<'_, '_>, segment: &str) -> Result<bool, OfficeError> {
    let (name, predicate) = match segment.split_once('[') {
        Some((name, rest)) if rest.ends_with(']') => (name, Some(&rest[..rest.len() - 1])),
        Some(_) => {
            return Err(OfficeError::InvalidPackage(format!(
                "invalid XML path segment: {segment}"
            )));
        }
        None => (segment, None),
    };
    if node.tag_name().name() != name {
        return Ok(false);
    }
    let Some(predicate) = predicate else {
        return Ok(true);
    };
    if let Ok(index) = predicate.parse::<usize>() {
        let position = node
            .prev_siblings()
            .filter(|sibling| sibling.is_element() && sibling.tag_name().name() == name)
            .count();
        return Ok(position == index);
    }
    let Some(attribute) = predicate.strip_prefix('@') else {
        return Err(OfficeError::InvalidPackage(format!(
            "unsupported XML predicate: {predicate}"
        )));
    };
    let (key, value) = attribute.split_once('=').ok_or_else(|| {
        OfficeError::InvalidPackage(format!("invalid XML attribute predicate: {predicate}"))
    })?;
    let expected = value.trim_matches(['\"', '\'']);
    Ok(node.attribute(key).is_some_and(|actual| actual == expected))
}

fn node_to_document_node(node: Node<'_, '_>) -> DocumentNode {
    let path = node
        .ancestors()
        .skip(1)
        .filter(|ancestor| ancestor.is_element())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|ancestor| ancestor.tag_name().name().to_owned())
        .chain(std::iter::once(node.tag_name().name().to_owned()))
        .collect::<Vec<_>>()
        .join("/");
    DocumentNode {
        tag: node.tag_name().name().to_owned(),
        path: format!("/{path}"),
        attributes: node
            .attributes()
            .map(|attribute| (attribute.name().to_owned(), attribute.value().to_owned()))
            .collect(),
        text: (!node.children().any(|child| child.is_element()))
            .then(|| node.text().unwrap_or_default().to_owned())
            .filter(|text| !text.is_empty()),
        children: node
            .children()
            .filter(|child| child.is_element())
            .map(node_to_document_node)
            .collect(),
    }
}

fn apply_xml_properties(
    xml: &str,
    range: std::ops::Range<usize>,
    properties: &BTreeMap<String, String>,
) -> Result<String, OfficeError> {
    let source = &xml[range.clone()];
    let open_end =
        find_open_tag_end(source).ok_or_else(|| OfficeError::Xml("unterminated XML tag".into()))?;
    let mut opening = source[..=open_end].to_owned();
    for (key, value) in properties {
        if key == "text" {
            continue;
        }
        set_opening_attribute(&mut opening, key, value);
    }
    let mut replacement = String::with_capacity(source.len() + 64);
    replacement.push_str(&opening);
    if let Some(text) = properties.get("text") {
        let closing = source.rfind("</").ok_or_else(|| {
            OfficeError::Xml("text can only be set on an element with a closing tag".into())
        })?;
        replacement.push_str(&quick_xml::escape::escape(text));
        replacement.push_str(&source[closing..]);
    } else {
        replacement.push_str(&source[open_end + 1..]);
    }
    Ok(format!("{}{}", &xml[..range.start], replacement) + &xml[range.end..])
}

fn find_open_tag_end(source: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in source.char_indices() {
        match (quote, character) {
            (None, '\"') | (None, '\'') => quote = Some(character),
            (Some(current), character) if current == character => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn set_opening_attribute(opening: &mut String, key: &str, value: &str) {
    let escaped = quick_xml::escape::escape(value);
    let bytes = opening.as_bytes();
    let mut cursor = 1;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] == b'>' || bytes[cursor] == b'/' {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && bytes[cursor] != b'='
            && bytes[cursor] != b'>'
        {
            cursor += 1;
        }
        let name = &opening[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let Some(quote) = bytes.get(cursor).copied() else {
            break;
        };
        if quote != b'\"' && quote != b'\'' {
            continue;
        }
        let value_start = cursor + 1;
        cursor = value_start;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if name == key {
            opening.replace_range(value_start..cursor, &escaped);
            return;
        }
        cursor += 1;
    }
    let insert_at = opening.rfind('>').unwrap_or(opening.len());
    let prefix = if insert_at > 0 && opening.as_bytes()[insert_at - 1] == b'/' {
        1
    } else {
        0
    };
    opening.insert_str(insert_at - prefix, &format!(" {key}=\"{escaped}\""));
}

pub fn detect_format(bytes: &[u8]) -> Result<OfficeFormat, OfficeError> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| OfficeError::UnsupportedPackage)?;
    let names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|entry| entry.name().to_owned())
        })
        .collect();
    if names.iter().any(|name| name == "word/document.xml") {
        Ok(OfficeFormat::Docx)
    } else if names.iter().any(|name| name == "xl/workbook.xml") {
        Ok(OfficeFormat::Xlsx)
    } else if names.iter().any(|name| name == "ppt/presentation.xml") {
        Ok(OfficeFormat::Pptx)
    } else {
        Err(OfficeError::UnsupportedPackage)
    }
}

fn validate_part_name(name: &str) -> Result<(), OfficeError> {
    if name.is_empty() || name.starts_with('/') || name.contains("..") || name.contains('\\') {
        return Err(OfficeError::InvalidPartName);
    }
    Ok(())
}

fn minimal_parts(format: OfficeFormat) -> Vec<(&'static str, &'static str)> {
    match format {
        OfficeFormat::Docx => vec![
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", ROOT_DOCX_RELS),
            ("word/document.xml", DOCX_DOCUMENT),
            ("word/_rels/document.xml.rels", DOCX_DOCUMENT_RELS),
            ("word/styles.xml", DOCX_STYLES),
            ("word/settings.xml", DOCX_SETTINGS),
        ],
        OfficeFormat::Xlsx => vec![
            ("[Content_Types].xml", XLSX_CONTENT_TYPES),
            ("_rels/.rels", ROOT_XLSX_RELS),
            ("xl/workbook.xml", XLSX_WORKBOOK),
            ("xl/_rels/workbook.xml.rels", XLSX_WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", XLSX_SHEET),
        ],
        OfficeFormat::Pptx => vec![
            ("[Content_Types].xml", PPTX_CONTENT_TYPES),
            ("_rels/.rels", ROOT_PPTX_RELS),
            ("ppt/presentation.xml", PPTX_PRESENTATION),
            ("ppt/_rels/presentation.xml.rels", PPTX_PRESENTATION_RELS),
            ("ppt/slideMasters/slideMaster1.xml", PPTX_MASTER),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels",
                PPTX_MASTER_RELS,
            ),
            ("ppt/slideLayouts/slideLayout1.xml", PPTX_LAYOUT),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                PPTX_LAYOUT_RELS,
            ),
            ("ppt/slides/slide1.xml", PPTX_SLIDE),
            ("ppt/theme/theme1.xml", PPTX_THEME),
        ],
    }
}

const DOCX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>"#;
const ROOT_DOCX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOCX_DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>New document</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#;
const DOCX_DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#;
const DOCX_STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style></w:styles>"#;
const DOCX_SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:zoom w:percent="100"/></w:settings>"#;

const XLSX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
const ROOT_XLSX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const XLSX_WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets><calcPr calcId="191029"/></workbook>"#;
const XLSX_WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
const XLSX_SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>New document</t></is></c></row></sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#;

const PPTX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
const ROOT_PPTX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
const PPTX_PRESENTATION: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#;
const PPTX_PRESENTATION_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#;
const PPTX_MASTER: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name=""><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm/></p:grpSpPr></p:spTree></p:cSld><p:sldLayoutIdLst><p:sldLayoutId id="1" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles><p:clrMap accent1="accent1" accent2="accent2" bg1="lt1" bg2="lt2" tx1="dk1" tx2="dk2"/></p:sldMaster>"#;
const PPTX_MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
const PPTX_LAYOUT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm/></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;
const PPTX_LAYOUT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;
const PPTX_SLIDE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name=""><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm/></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#;
const PPTX_THEME: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2><a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2><a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4><a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6><a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Arial"/></a:majorFont><a:minorFont><a:latin typeface="Arial"/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst/><a:lnStyleLst/><a:effectStyleLst/><a:bgFillStyleLst/></a:fmtScheme></a:themeElements></a:theme>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reads_each_supported_format() {
        for format in [OfficeFormat::Docx, OfficeFormat::Xlsx, OfficeFormat::Pptx] {
            let package = OfficePackage::create(format).expect("minimal package");
            assert_eq!(package.format(), format);
            assert!(!package.summary().unwrap().parts.is_empty());
        }
    }

    #[test]
    fn resolves_format_semantic_paths() {
        let docx = OfficePackage::create(OfficeFormat::Docx).unwrap();
        assert_eq!(
            docx.resolve_path("/body/p[1]").unwrap(),
            (
                "word/document.xml".to_owned(),
                "/document/body/p[1]".to_owned()
            )
        );
        let xlsx = OfficePackage::create(OfficeFormat::Xlsx).unwrap();
        assert_eq!(
            xlsx.resolve_path("/Sheet1/A1").unwrap(),
            (
                "xl/worksheets/sheet1.xml".to_owned(),
                "/worksheet/sheetData/row[@r='1']/c[@r='A1']".to_owned()
            )
        );
        let pptx = OfficePackage::create(OfficeFormat::Pptx).unwrap();
        assert_eq!(
            pptx.resolve_path("/slide[1]").unwrap(),
            ("ppt/slides/slide1.xml".to_owned(), "/sld".to_owned())
        );
    }

    #[test]
    fn rejects_unsafe_part_names() {
        let package = OfficePackage::create(OfficeFormat::Docx).unwrap();
        assert!(matches!(
            package.read_part("../secret"),
            Err(OfficeError::InvalidPartName)
        ));
    }

    #[test]
    fn replaces_and_adds_parts() {
        let package = OfficePackage::create(OfficeFormat::Docx).unwrap();
        let changed = package.with_part("word/custom.xml", b"<custom/>").unwrap();
        assert_eq!(changed.read_part("word/custom.xml").unwrap(), b"<custom/>");
    }

    #[test]
    fn queries_and_edits_xml_nodes() {
        let package = OfficePackage::create(OfficeFormat::Docx).unwrap();
        let nodes = package
            .query_xml("word/document.xml", "/document/body/p[1]")
            .unwrap();
        assert_eq!(nodes[0].tag, "p");
        assert_eq!(nodes[0].path, "/document/body/p");
        assert_eq!(
            nodes[0].children[0].children[0].text.as_deref(),
            Some("New document")
        );

        let mut properties = BTreeMap::new();
        properties.insert("text".to_owned(), "Updated document".to_owned());
        let changed = package
            .set_xml(
                "word/document.xml",
                "/document/body/p[1]/r/t[1]",
                &properties,
            )
            .unwrap();
        assert!(
            changed
                .read_xml_part("word/document.xml")
                .unwrap()
                .contains("Updated document")
        );
    }

    #[test]
    fn removes_parts_and_merges_placeholders() {
        let package = OfficePackage::create(OfficeFormat::Docx)
            .unwrap()
            .with_part("word/custom.xml", b"<custom>{{name}}</custom>")
            .unwrap();
        let mut values = BTreeMap::new();
        values.insert("name".to_owned(), "OfficeCLI".to_owned());
        let merged = package.merge_text(&values).unwrap();
        assert_eq!(
            merged.read_part("word/custom.xml").unwrap(),
            b"<custom>OfficeCLI</custom>"
        );
        let removed = merged.remove_part("word/custom.xml").unwrap();
        assert!(matches!(
            removed.read_part("word/custom.xml"),
            Err(OfficeError::PartNotFound(_))
        ));
    }

    #[test]
    fn inserts_removes_moves_and_swaps_xml_nodes() {
        let package = OfficePackage::create(OfficeFormat::Docx).unwrap();
        let mut package = package
            .insert_xml(
                "word/document.xml",
                "/document/body",
                "<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:r><w:t>Second</w:t></w:r></w:p>",
            )
            .unwrap();
        assert_eq!(
            package
                .query_xml("word/document.xml", "/document/body/p")
                .unwrap()
                .len(),
            2
        );
        package = package
            .swap_xml(
                "word/document.xml",
                "/document/body/p[1]",
                "/document/body/p[2]",
            )
            .unwrap();
        assert!(
            package
                .read_xml_part("word/document.xml")
                .unwrap()
                .contains("Second")
        );
        package = package
            .move_xml("word/document.xml", "/document/body/p[1]", "/document/body")
            .unwrap();
        package = package
            .remove_xml("word/document.xml", "/document/body/p[1]")
            .unwrap();
        assert_eq!(
            package
                .query_xml("word/document.xml", "/document/body/p")
                .unwrap()
                .len(),
            1
        );
    }
}
