use crate::Config;
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::sync::Arc;

/// Незнайомі вузли й текст зберігаємо разом із вихідними XML-фрагментами.
#[derive(Clone, Debug, PartialEq)]
pub struct XmlNode {
    pub name: String,
    pub attributes: Config,
    pub children: Vec<XmlContent>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum XmlContent {
    Element(XmlNode),
    Raw {
        source: Arc<str>,
        start: usize,
        end: usize,
    },
}

impl XmlNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Config::new(),
            children: Vec::new(),
        }
    }
    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
    pub fn set(&mut self, key: &str, value: impl ToString) {
        self.attributes.insert(key.into(), value.to_string());
    }
    pub fn elements(&self) -> impl Iterator<Item = &XmlNode> {
        self.children.iter().filter_map(|c| match c {
            XmlContent::Element(n) => Some(n),
            _ => None,
        })
    }
    pub fn element(&self, name: &str) -> Option<&XmlNode> {
        self.elements().find(|n| n.local_name() == name)
    }
    pub fn element_mut(&mut self, name: &str) -> Option<&mut XmlNode> {
        self.children.iter_mut().find_map(|c| match c {
            XmlContent::Element(n) if n.local_name() == name => Some(n),
            _ => None,
        })
    }
    pub fn push(&mut self, node: XmlNode) {
        self.children.push(XmlContent::Element(node));
    }
    pub fn remove_elements(&mut self, name: &str) {
        self.children
            .retain(|c| !matches!(c, XmlContent::Element(n) if n.local_name() == name));
    }
    pub fn metadata(&self) -> Config {
        self.elements()
            .filter(|n| n.local_name() == "metadata")
            .filter_map(|n| Some((n.attr("key")?.to_owned(), n.attr("value")?.to_owned())))
            .collect()
    }
    pub fn set_metadata(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string();
        for child in &mut self.children {
            if let XmlContent::Element(n) = child
                && n.local_name() == "metadata"
                && n.attr("key") == Some(key)
            {
                n.set("value", &value);
                return;
            }
        }
        let mut n = XmlNode::new("metadata");
        n.set("key", key);
        n.set("value", value);
        self.push(n);
    }
    pub fn replace_metadata(&mut self, config: &Config, reserved: &[&str]) {
        self.children.retain(|c| !matches!(c, XmlContent::Element(n) if n.local_name() == "metadata"
            && n.attr("key").is_some_and(|key| !reserved.contains(&key) && !config.contains_key(key))));
        for (key, value) in config {
            if !reserved.contains(&key.as_str()) {
                self.set_metadata(key, value);
            }
        }
    }
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (key, value) in &self.attributes {
            out.push(' ');
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&quick_xml::escape::escape(value));
            out.push('"');
        }
        if self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for child in &self.children {
            match child {
                XmlContent::Element(n) => n.write(out),
                XmlContent::Raw { source, start, end } => out.push_str(&source[*start..*end]),
            }
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
    /// Великі `<mesh>` лишаються сирим фрагментом, без DOM на мільйони вузлів.
    pub(crate) fn parse(source: Arc<str>) -> Result<Self> {
        let mut reader = Reader::from_str(&source);
        let mut stack: Vec<XmlNode> = Vec::new();
        let mut root = None;
        loop {
            let start = reader.buffer_position() as usize;
            match reader.read_event()? {
                Event::Start(e) if e.local_name().as_ref() == "mesh" => {
                    reader.read_to_end(e.name())?;
                    let end = reader.buffer_position() as usize;
                    stack
                        .last_mut()
                        .context("mesh поза коренем")?
                        .children
                        .push(XmlContent::Raw {
                            source: source.clone(),
                            start,
                            end,
                        });
                }
                Event::Start(e) => {
                    ensure!(stack.len() < 128, "надто глибокий XML");
                    stack.push(node(&e)?);
                }
                Event::Empty(e) => {
                    let n = node(&e)?;
                    if let Some(parent) = stack.last_mut() {
                        parent.push(n);
                    } else {
                        ensure!(root.is_none(), "кілька коренів XML");
                        root = Some(n);
                    }
                }
                Event::End(_) => {
                    let n = stack.pop().context("неочікуваний кінець XML")?;
                    if let Some(parent) = stack.last_mut() {
                        parent.push(n);
                    } else {
                        ensure!(root.is_none(), "кілька коренів XML");
                        root = Some(n);
                    }
                }
                Event::Eof => break,
                _ => {
                    let end = reader.buffer_position() as usize;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(XmlContent::Raw {
                            source: source.clone(),
                            start,
                            end,
                        });
                    }
                }
            }
        }
        if !stack.is_empty() {
            bail!("незакритий XML");
        }
        root.context("немає кореня XML")
    }
}

pub(crate) fn attrs(e: &BytesStart<'_>) -> Result<Config> {
    e.attributes()
        .map(|a| {
            let a = a?;
            Ok((
                a.key.as_ref().to_owned(),
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                    .into_owned(),
            ))
        })
        .collect()
}
fn node(e: &BytesStart<'_>) -> Result<XmlNode> {
    Ok(XmlNode {
        name: e.name().as_ref().to_owned(),
        attributes: attrs(e)?,
        children: Vec::new(),
    })
}
