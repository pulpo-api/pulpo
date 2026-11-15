//! SOAP: operations read from a WSDL (1.1) and envelopes written from its XSD schema.
//!
//! A SOAP request is an HTTP `POST` with an XML envelope; picking an operation writes the
//! envelope and the `Content-Type`/`SOAPAction` headers, so sending is plain HTTP.

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const WSDL: &str = "http://schemas.xmlsoap.org/wsdl/";
const SOAP11: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
const SOAP12: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
const XSD: &str = "http://www.w3.org/2001/XMLSchema";
pub const ENVELOPE11: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const ENVELOPE12: &str = "http://www.w3.org/2003/05/soap-envelope";
const MAX_DEPTH: usize = 8;

/// Where a SOAP request's operation comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoapTarget {
    /// The WSDL: a workspace file or a URL.
    #[serde(default)]
    pub wsdl: String,
    /// `Service/Port/Operation`.
    #[serde(default)]
    pub operation: String,
}

/// An operation the app can pick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoapOperation {
    /// `Service/Port/Operation`.
    pub path: String,
    pub service: String,
    pub port: String,
    pub name: String,
    /// The endpoint (`soap:address`).
    pub address: String,
    pub soap_action: String,
    /// `1.1` or `1.2`.
    pub version: String,
    /// `document` or `rpc`.
    pub style: String,
    /// The first element of the body, as `{namespace}name`.
    pub body_element: String,
    /// A sample envelope with every field of the request.
    pub envelope: String,
    /// The headers to send it with.
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
}

pub fn is_wsdl(text: &str) -> bool {
    text.contains(WSDL) && text.contains("definitions")
}

// ------------------------------------------------------------------- reading

type QName = (String, String);
/// A message part: its name and its `element` or `type`.
type Part = (String, Option<QName>, Option<QName>);

fn qname(node: Node, value: &str) -> QName {
    match value.split_once(':') {
        Some((prefix, local)) => (
            node.lookup_namespace_uri(Some(prefix))
                .unwrap_or("")
                .to_string(),
            local.to_string(),
        ),
        None => (
            node.lookup_namespace_uri(None).unwrap_or("").to_string(),
            value.to_string(),
        ),
    }
}

fn children<'a, 'i>(
    node: Node<'a, 'i>,
    ns: &'a str,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children().filter(move |c| {
        c.is_element() && c.tag_name().namespace() == Some(ns) && c.tag_name().name() == name
    })
}

fn child<'a, 'i>(node: Node<'a, 'i>, ns: &'a str, name: &'a str) -> Option<Node<'a, 'i>> {
    children(node, ns, name).next()
}

/// The XSD of a WSDL: global elements and types by `{namespace}name`.
struct Schema<'a, 'i> {
    elements: HashMap<QName, Node<'a, 'i>>,
    types: HashMap<QName, Node<'a, 'i>>,
    /// Namespaces whose local elements are qualified.
    qualified: HashSet<String>,
}

impl<'a, 'i> Schema<'a, 'i> {
    fn read(root: Node<'a, 'i>) -> Self {
        let mut s = Schema {
            elements: HashMap::new(),
            types: HashMap::new(),
            qualified: HashSet::new(),
        };
        for types in children(root, WSDL, "types") {
            for schema in children(types, XSD, "schema") {
                let ns = schema
                    .attribute("targetNamespace")
                    .unwrap_or("")
                    .to_string();
                if schema.attribute("elementFormDefault") == Some("qualified") {
                    s.qualified.insert(ns.clone());
                }
                for item in schema.children().filter(|c| c.is_element()) {
                    let Some(name) = item.attribute("name") else {
                        continue;
                    };
                    let key = (ns.clone(), name.to_string());
                    match item.tag_name().name() {
                        "element" => {
                            s.elements.insert(key, item);
                        }
                        "complexType" | "simpleType" => {
                            s.types.insert(key, item);
                        }
                        _ => {}
                    }
                }
            }
        }
        s
    }
}

/// Builds sample XML for elements, declaring the namespaces it uses.
struct Writer<'s, 'a, 'i> {
    schema: &'s Schema<'a, 'i>,
    prefixes: Vec<(String, String)>,
}

impl Writer<'_, '_, '_> {
    fn prefix(&mut self, ns: &str) -> String {
        if let Some((_, p)) = self.prefixes.iter().find(|(n, _)| n == ns) {
            return p.clone();
        }
        let p = format!("ns{}", self.prefixes.len() + 1);
        self.prefixes.push((ns.to_string(), p.clone()));
        p
    }

    fn tag(&mut self, ns: &str, name: &str, qualified: bool) -> String {
        if qualified && !ns.is_empty() {
            format!("{}:{name}", self.prefix(ns))
        } else {
            name.to_string()
        }
    }

    /// A global element (`{ns}name`).
    fn global(&mut self, element: &QName, indent: usize, out: &mut String) {
        let tag = self.tag(&element.0, &element.1, true);
        match self.schema.elements.get(element).copied() {
            Some(decl) => self.element_body(decl, &tag, &element.0, indent, 0, out),
            None => out.push_str(&format!("{}<{tag}>?</{tag}>\n", pad(indent))),
        }
    }

    /// The content of an element declaration, written as `<tag>…</tag>`.
    fn element_body(
        &mut self,
        decl: Node,
        tag: &str,
        ns: &str,
        indent: usize,
        depth: usize,
        out: &mut String,
    ) {
        let p = pad(indent);
        if depth > MAX_DEPTH {
            out.push_str(&format!("{p}<{tag}/>\n"));
            return;
        }
        if let Some(t) = decl.attribute("type") {
            let t = qname(decl, t);
            if t.0 == XSD {
                out.push_str(&format!("{p}<{tag}>{}</{tag}>\n", sample(&t.1)));
                return;
            }
            match self.schema.types.get(&t).copied() {
                Some(ty) => self.type_body(ty, tag, &t.0, indent, depth, out),
                None => out.push_str(&format!("{p}<{tag}>?</{tag}>\n")),
            }
            return;
        }
        if let Some(ty) = decl
            .children()
            .find(|c| c.is_element() && matches!(c.tag_name().name(), "complexType" | "simpleType"))
        {
            self.type_body(ty, tag, ns, indent, depth, out);
            return;
        }
        out.push_str(&format!("{p}<{tag}>?</{tag}>\n"));
    }

    fn type_body(
        &mut self,
        ty: Node,
        tag: &str,
        ns: &str,
        indent: usize,
        depth: usize,
        out: &mut String,
    ) {
        let p = pad(indent);
        if ty.tag_name().name() == "simpleType" {
            out.push_str(&format!("{p}<{tag}>{}</{tag}>\n", simple_sample(ty)));
            return;
        }
        let mut inner = String::new();
        self.particles(ty, ns, indent + 1, depth + 1, &mut inner);
        if inner.is_empty() {
            out.push_str(&format!("{p}<{tag}/>\n"));
        } else {
            out.push_str(&format!("{p}<{tag}>\n{inner}{p}</{tag}>\n"));
        }
    }

    /// The child elements of a complex type: sequence, all, choice (first option) and
    /// extensions of a base type.
    fn particles(&mut self, node: Node, ns: &str, indent: usize, depth: usize, out: &mut String) {
        for c in node
            .children()
            .filter(|c| c.is_element() && c.tag_name().namespace() == Some(XSD))
        {
            match c.tag_name().name() {
                "sequence" | "all" => self.particles(c, ns, indent, depth, out),
                "choice" => {
                    out.push_str(&format!("{}<!--Choice: one of these-->\n", pad(indent)));
                    if let Some(first) = c.children().find(|x| x.is_element()) {
                        self.particle(first, ns, indent, depth, out);
                    }
                }
                "complexContent" | "simpleContent" => {
                    for ext in c.children().filter(|x| x.is_element()) {
                        if let Some(base) = ext.attribute("base") {
                            let base = qname(ext, base);
                            if let Some(b) = self.schema.types.get(&base).copied() {
                                if depth <= MAX_DEPTH {
                                    self.particles(b, &base.0, indent, depth + 1, out);
                                }
                            }
                        }
                        self.particles(ext, ns, indent, depth, out);
                    }
                }
                "element" => self.particle(c, ns, indent, depth, out),
                _ => {}
            }
        }
    }

    fn particle(&mut self, el: Node, ns: &str, indent: usize, depth: usize, out: &mut String) {
        if el.tag_name().name() != "element" {
            if matches!(el.tag_name().name(), "sequence" | "choice" | "all") {
                self.particles(el.parent().unwrap_or(el), ns, indent, depth, out);
            }
            return;
        }
        let optional = el.attribute("minOccurs") == Some("0");
        let many = matches!(el.attribute("maxOccurs"), Some(m) if m != "0" && m != "1");
        let p = pad(indent);
        if optional {
            out.push_str(&format!("{p}<!--Optional:-->\n"));
        }
        if many {
            out.push_str(&format!("{p}<!--Zero or more repetitions:-->\n"));
        }
        if let Some(r) = el.attribute("ref") {
            let r = qname(el, r);
            let tag = self.tag(&r.0, &r.1, true);
            match self.schema.elements.get(&r).copied() {
                Some(decl) => self.element_body(decl, &tag, &r.0, indent, depth, out),
                None => out.push_str(&format!("{p}<{tag}>?</{tag}>\n")),
            }
            return;
        }
        let Some(name) = el.attribute("name") else {
            return;
        };
        let qualified = el.attribute("form") == Some("qualified")
            || (el.attribute("form") != Some("unqualified") && self.schema.qualified.contains(ns));
        let tag = self.tag(ns, name, qualified);
        self.element_body(el, &tag, ns, indent, depth, out);
    }
}

fn pad(indent: usize) -> String {
    "   ".repeat(indent)
}

/// SoapUI's sample values: `?` for text, a typed sample otherwise.
fn sample(xsd_type: &str) -> &'static str {
    match xsd_type {
        "int" | "integer" | "long" | "short" | "byte" | "decimal" | "nonNegativeInteger"
        | "positiveInteger" | "unsignedInt" | "unsignedLong" | "unsignedShort" | "double"
        | "float" => "0",
        "boolean" => "false",
        "dateTime" => "2024-01-01T00:00:00",
        "date" => "2024-01-01",
        "time" => "00:00:00",
        _ => "?",
    }
}

fn simple_sample(simple: Node) -> String {
    let restriction = simple
        .descendants()
        .find(|d| d.tag_name().name() == "restriction");
    if let Some(r) = restriction {
        if let Some(first) = r
            .children()
            .find(|c| c.tag_name().name() == "enumeration")
            .and_then(|e| e.attribute("value"))
        {
            return first.to_string();
        }
        if let Some(base) = r.attribute("base") {
            return sample(&qname(r, base).1).to_string();
        }
    }
    "?".into()
}

/// Every operation of every SOAP port in a WSDL, with a sample envelope.
pub fn operations(wsdl: &str) -> Result<Vec<SoapOperation>, String> {
    let doc = Document::parse(wsdl).map_err(|e| format!("not valid XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "definitions" || root.tag_name().namespace() != Some(WSDL) {
        return Err("not a WSDL 1.1 document (wsdl:definitions)".into());
    }
    let tns = root.attribute("targetNamespace").unwrap_or("").to_string();
    let schema = Schema::read(root);

    // message → parts (name, element or type).
    let mut messages: HashMap<String, Vec<Part>> = HashMap::new();
    for m in children(root, WSDL, "message") {
        let parts = children(m, WSDL, "part")
            .map(|p| {
                (
                    p.attribute("name").unwrap_or("").to_string(),
                    p.attribute("element").map(|e| qname(p, e)),
                    p.attribute("type").map(|t| qname(p, t)),
                )
            })
            .collect();
        messages.insert(m.attribute("name").unwrap_or("").to_string(), parts);
    }
    // portType → operation → (input message, documentation).
    let mut port_types: HashMap<String, HashMap<String, (String, Option<String>)>> = HashMap::new();
    for pt in children(root, WSDL, "portType") {
        let mut ops = HashMap::new();
        for op in children(pt, WSDL, "operation") {
            let input = child(op, WSDL, "input")
                .and_then(|i| i.attribute("message"))
                .map(|m| qname(op, m).1)
                .unwrap_or_default();
            let doc = child(op, WSDL, "documentation")
                .and_then(|d| d.text())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            ops.insert(op.attribute("name").unwrap_or("").to_string(), (input, doc));
        }
        port_types.insert(pt.attribute("name").unwrap_or("").to_string(), ops);
    }

    let mut out = Vec::new();
    for service in children(root, WSDL, "service") {
        let service_name = service.attribute("name").unwrap_or("Service");
        for port in children(service, WSDL, "port") {
            let (version, soap_ns) = if let Some(a) = child(port, SOAP11, "address") {
                ("1.1", a)
            } else if let Some(a) = child(port, SOAP12, "address") {
                ("1.2", a)
            } else {
                continue; // an HTTP binding, not SOAP
            };
            let address = soap_ns.attribute("location").unwrap_or("").to_string();
            let binding_name = qname(port, port.attribute("binding").unwrap_or("")).1;
            let Some(binding) = children(root, WSDL, "binding")
                .find(|b| b.attribute("name") == Some(binding_name.as_str()))
            else {
                continue;
            };
            let bns = if version == "1.1" { SOAP11 } else { SOAP12 };
            let binding_style = child(binding, bns, "binding")
                .and_then(|b| b.attribute("style"))
                .unwrap_or("document");
            let port_type = qname(binding, binding.attribute("type").unwrap_or("")).1;
            let ops = port_types.get(&port_type);
            for bop in children(binding, WSDL, "operation") {
                let name = bop.attribute("name").unwrap_or("").to_string();
                let soap_op = child(bop, bns, "operation");
                let action = soap_op
                    .and_then(|o| o.attribute("soapAction"))
                    .unwrap_or("")
                    .to_string();
                let style = soap_op
                    .and_then(|o| o.attribute("style"))
                    .unwrap_or(binding_style)
                    .to_string();
                let rpc_ns = child(bop, WSDL, "input")
                    .and_then(|i| child(i, bns, "body"))
                    .and_then(|b| b.attribute("namespace"))
                    .unwrap_or(&tns)
                    .to_string();
                let (input, documentation) =
                    ops.and_then(|o| o.get(&name)).cloned().unwrap_or_default();
                let parts = messages.get(&input).cloned().unwrap_or_default();
                let mut writer = Writer {
                    schema: &schema,
                    prefixes: Vec::new(),
                };
                let mut body = String::new();
                let body_element;
                if style == "rpc" {
                    let tag = writer.tag(&rpc_ns, &name, true);
                    body_element = format!("{{{rpc_ns}}}{name}");
                    body.push_str(&format!("{}<{tag}>\n", pad(2)));
                    for (part, element, ty) in &parts {
                        match (element, ty) {
                            (Some(e), _) => writer.global(e, 3, &mut body),
                            (None, Some(t)) if t.0 == XSD => body.push_str(&format!(
                                "{}<{part}>{}</{part}>\n",
                                pad(3),
                                sample(&t.1)
                            )),
                            (None, Some(t)) => match schema.types.get(t).copied() {
                                Some(node) => writer.type_body(node, part, &t.0, 3, 0, &mut body),
                                None => body.push_str(&format!("{}<{part}>?</{part}>\n", pad(3))),
                            },
                            (None, None) => {}
                        }
                    }
                    body.push_str(&format!("{}</{tag}>\n", pad(2)));
                } else {
                    body_element = parts
                        .iter()
                        .find_map(|(_, e, _)| e.clone())
                        .map(|(ns, n)| format!("{{{ns}}}{n}"))
                        .unwrap_or_default();
                    for (_, element, _) in &parts {
                        if let Some(e) = element {
                            writer.global(e, 2, &mut body);
                        }
                    }
                }
                let (env_ns, content_type) = if version == "1.1" {
                    (ENVELOPE11, "text/xml; charset=utf-8".to_string())
                } else {
                    (
                        ENVELOPE12,
                        format!("application/soap+xml; charset=utf-8; action=\"{action}\""),
                    )
                };
                let declarations: String = writer
                    .prefixes
                    .iter()
                    .map(|(ns, p)| format!(" xmlns:{p}=\"{ns}\""))
                    .collect();
                let envelope = format!(
                    "<soapenv:Envelope xmlns:soapenv=\"{env_ns}\"{declarations}>\n{}<soapenv:Header/>\n{}<soapenv:Body>\n{body}{}</soapenv:Body>\n</soapenv:Envelope>\n",
                    pad(1),
                    pad(1),
                    pad(1)
                );
                let mut headers = vec![("Content-Type".to_string(), content_type)];
                if version == "1.1" {
                    headers.push(("SOAPAction".to_string(), format!("\"{action}\"")));
                }
                out.push(SoapOperation {
                    path: format!(
                        "{service_name}/{}/{name}",
                        port.attribute("name").unwrap_or("")
                    ),
                    service: service_name.to_string(),
                    port: port.attribute("name").unwrap_or("").to_string(),
                    name,
                    address: address.clone(),
                    soap_action: action,
                    version: version.into(),
                    style,
                    body_element,
                    envelope,
                    headers,
                    documentation,
                });
            }
        }
    }
    if out.is_empty() {
        return Err("the WSDL has no SOAP operations (no soap:address in its ports)".into());
    }
    Ok(out)
}

// ------------------------------------------------------------------- loading

/// The WSDL text: a URL (fetched) or a workspace file (by path, else by file name
/// anywhere in the workspace).
pub async fn load(root: &Path, location: &str) -> Result<String, String> {
    let location = location.trim();
    if location.starts_with("http://") || location.starts_with("https://") {
        let response = reqwest::get(location)
            .await
            .map_err(|e| format!("could not fetch {location}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{location}: {}", response.status()));
        }
        return response.text().await.map_err(|e| e.to_string());
    }
    let path = crate::collection::find_in_workspace(root, location)
        .ok_or_else(|| format!("no WSDL {location} in the workspace"))?;
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// An import plan: a folder per service, a request per operation (SOAP 1.1 ports first;
/// a SOAP 1.2 port of the same operation is left out).
pub fn plan(wsdl: &str, file_name: &str) -> Result<crate::import::ImportPlan, String> {
    use crate::import::{ImportFormat, ImportNode, ImportPlan};
    let mut ops = operations(wsdl)?;
    ops.sort_by_key(|o| (o.service.clone(), o.version.clone()));
    let mut seen = HashSet::new();
    let mut folders: Vec<(String, Vec<ImportNode>)> = Vec::new();
    for op in ops {
        if !seen.insert((op.service.clone(), op.name.clone())) {
            continue;
        }
        let mut request =
            crate::http::HttpRequest::new(crate::HttpMethod::Post, op.address.clone());
        for (name, value) in &op.headers {
            request
                .headers
                .push(crate::http::Header::new(name.clone(), value.clone()));
        }
        request.body = Some(op.envelope.clone());
        request.soap = Some(SoapTarget {
            wsdl: file_name.to_string(),
            operation: op.path.clone(),
        });
        let node = ImportNode::Request {
            name: op.name.clone(),
            request,
            scripts: Default::default(),
        };
        match folders.iter_mut().find(|(s, _)| *s == op.service) {
            Some((_, list)) => list.push(node),
            None => folders.push((op.service.clone(), vec![node])),
        }
    }
    let nodes = if folders.len() == 1 {
        folders.pop().map(|(_, n)| n).unwrap_or_default()
    } else {
        folders
            .into_iter()
            .map(|(name, children)| ImportNode::Folder {
                name,
                children,
                scripts: Default::default(),
                auth: None,
            })
            .collect()
    };
    Ok(ImportPlan {
        format: ImportFormat::Wsdl,
        folder: Some(
            Path::new(file_name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "SOAP".into()),
        ),
        folder_scripts: Default::default(),
        folder_auth: None,
        nodes,
        environments: Vec::new(),
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALCULATOR: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<wsdl:definitions xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/" xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
  xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/" xmlns:s="http://www.w3.org/2001/XMLSchema"
  xmlns:tns="http://tempuri.org/" targetNamespace="http://tempuri.org/">
  <wsdl:types>
    <s:schema elementFormDefault="qualified" targetNamespace="http://tempuri.org/">
      <s:element name="Add">
        <s:complexType><s:sequence>
          <s:element minOccurs="1" maxOccurs="1" name="intA" type="s:int"/>
          <s:element minOccurs="1" maxOccurs="1" name="intB" type="s:int"/>
          <s:element minOccurs="0" maxOccurs="1" name="note" type="s:string"/>
          <s:element minOccurs="0" maxOccurs="unbounded" name="tag" type="tns:Color"/>
          <s:element name="who" type="tns:Person"/>
        </s:sequence></s:complexType>
      </s:element>
      <s:complexType name="Person"><s:sequence>
        <s:element name="name" type="s:string"/>
        <s:element name="born" type="s:date"/>
      </s:sequence></s:complexType>
      <s:simpleType name="Color"><s:restriction base="s:string">
        <s:enumeration value="RED"/><s:enumeration value="BLUE"/>
      </s:restriction></s:simpleType>
      <s:element name="AddResponse"><s:complexType><s:sequence>
        <s:element name="AddResult" type="s:int"/>
      </s:sequence></s:complexType></s:element>
    </s:schema>
  </wsdl:types>
  <wsdl:message name="AddSoapIn"><wsdl:part name="parameters" element="tns:Add"/></wsdl:message>
  <wsdl:message name="AddSoapOut"><wsdl:part name="parameters" element="tns:AddResponse"/></wsdl:message>
  <wsdl:portType name="CalculatorSoap">
    <wsdl:operation name="Add">
      <wsdl:documentation>Adds two integers.</wsdl:documentation>
      <wsdl:input message="tns:AddSoapIn"/><wsdl:output message="tns:AddSoapOut"/>
    </wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="CalculatorSoap" type="tns:CalculatorSoap">
    <soap:binding transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="Add">
      <soap:operation soapAction="http://tempuri.org/Add" style="document"/>
      <wsdl:input><soap:body use="literal"/></wsdl:input><wsdl:output><soap:body use="literal"/></wsdl:output>
    </wsdl:operation>
  </wsdl:binding>
  <wsdl:binding name="CalculatorSoap12" type="tns:CalculatorSoap">
    <soap12:binding transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="Add">
      <soap12:operation soapAction="http://tempuri.org/Add" style="document"/>
      <wsdl:input><soap12:body use="literal"/></wsdl:input><wsdl:output><soap12:body use="literal"/></wsdl:output>
    </wsdl:operation>
  </wsdl:binding>
  <wsdl:service name="Calculator">
    <wsdl:port name="CalculatorSoap" binding="tns:CalculatorSoap"><soap:address location="http://www.dneonline.com/calculator.asmx"/></wsdl:port>
    <wsdl:port name="CalculatorSoap12" binding="tns:CalculatorSoap12"><soap12:address location="http://www.dneonline.com/calculator.asmx"/></wsdl:port>
  </wsdl:service>
</wsdl:definitions>"#;

    #[test]
    fn lists_operations_and_writes_envelopes() {
        let ops = operations(CALCULATOR).unwrap();
        assert_eq!(ops.len(), 2);
        let add = &ops[0];
        assert_eq!(add.path, "Calculator/CalculatorSoap/Add");
        assert_eq!(add.version, "1.1");
        assert_eq!(add.soap_action, "http://tempuri.org/Add");
        assert_eq!(add.body_element, "{http://tempuri.org/}Add");
        assert_eq!(add.documentation.as_deref(), Some("Adds two integers."));
        assert_eq!(
            add.headers,
            vec![
                (
                    "Content-Type".to_string(),
                    "text/xml; charset=utf-8".to_string()
                ),
                (
                    "SOAPAction".to_string(),
                    "\"http://tempuri.org/Add\"".to_string()
                ),
            ]
        );
        assert_eq!(
            add.envelope,
            "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\" xmlns:ns1=\"http://tempuri.org/\">\n   <soapenv:Header/>\n   <soapenv:Body>\n      <ns1:Add>\n         <ns1:intA>0</ns1:intA>\n         <ns1:intB>0</ns1:intB>\n         <!--Optional:-->\n         <ns1:note>?</ns1:note>\n         <!--Optional:-->\n         <!--Zero or more repetitions:-->\n         <ns1:tag>RED</ns1:tag>\n         <ns1:who>\n            <ns1:name>?</ns1:name>\n            <ns1:born>2024-01-01</ns1:born>\n         </ns1:who>\n      </ns1:Add>\n   </soapenv:Body>\n</soapenv:Envelope>\n"
        );
        // The envelope itself is valid XML.
        Document::parse(&add.envelope).unwrap();
        let soap12 = &ops[1];
        assert_eq!(soap12.version, "1.2");
        assert!(soap12.envelope.contains(ENVELOPE12));
        assert_eq!(
            soap12.headers[0].1,
            "application/soap+xml; charset=utf-8; action=\"http://tempuri.org/Add\""
        );
        assert!(is_wsdl(CALCULATOR));
        assert!(operations("<a/>").unwrap_err().contains("not a WSDL"));
    }

    #[test]
    fn writes_rpc_bodies() {
        let rpc = r#"<definitions xmlns="http://schemas.xmlsoap.org/wsdl/" xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
  xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:tns="urn:hello" targetNamespace="urn:hello">
  <message name="SayIn"><part name="name" type="xsd:string"/><part name="times" type="xsd:int"/></message>
  <portType name="HelloPort"><operation name="Say"><input message="tns:SayIn"/></operation></portType>
  <binding name="HelloBinding" type="tns:HelloPort">
    <soap:binding style="rpc" transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="Say"><soap:operation soapAction="say"/><input><soap:body use="encoded" namespace="urn:hello:rpc"/></input></operation>
  </binding>
  <service name="Hello"><port name="HelloPort" binding="tns:HelloBinding"><soap:address location="http://localhost/hello"/></port></service>
</definitions>"#;
        let ops = operations(rpc).unwrap();
        assert_eq!(ops[0].style, "rpc");
        assert_eq!(ops[0].body_element, "{urn:hello:rpc}Say");
        assert!(
            ops[0].envelope.contains(
                "<ns1:Say>\n         <name>?</name>\n         <times>0</times>\n      </ns1:Say>"
            ),
            "{}",
            ops[0].envelope
        );
    }

    #[test]
    fn imports_one_request_per_operation() {
        let plan = plan(CALCULATOR, "calculator.wsdl").unwrap();
        assert_eq!(plan.folder.as_deref(), Some("calculator"));
        assert_eq!(plan.nodes.len(), 1, "the SOAP 1.2 port of Add is left out");
        let crate::import::ImportNode::Request { name, request, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(name, "Add");
        assert_eq!(request.url, "http://www.dneonline.com/calculator.asmx");
        assert_eq!(
            request.soap.as_ref().unwrap().operation,
            "Calculator/CalculatorSoap/Add"
        );
        assert!(request.body.as_deref().unwrap().contains("<ns1:Add>"));
    }

    #[tokio::test]
    async fn finds_the_wsdl_by_name_in_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("contracts/soap")).unwrap();
        std::fs::write(
            dir.path().join("contracts/soap/calculator.wsdl"),
            CALCULATOR,
        )
        .unwrap();
        assert!(load(dir.path(), "contracts/soap/calculator.wsdl")
            .await
            .is_ok());
        assert!(load(dir.path(), "calculator.wsdl").await.is_ok());
        assert!(load(dir.path(), "other.wsdl")
            .await
            .unwrap_err()
            .contains("no WSDL"));
    }
}
