//! `rdf:XMLLiteral` values: an XML fragment's canonical form (Exclusive XML
//! Canonicalization, as RDF 2004 defines the value), so that two literals are the same
//! value iff their forms are equal.
//!
//! The lexical forms are well-balanced, self-contained XML content (RDF 1.1 Concepts,
//! `rdf:XMLLiteral`): balanced elements, declared prefixes, only the predefined and
//! character references, no document type declaration. The whole fragment is checked
//! first. Of the well-formed ones, those with comments or processing instructions aren't
//! decided here: their value depends on what this canonical form leaves out. The others
//! are decided: elements, attributes, text, references, CDATA sections, and namespace
//! declarations (rendered where visibly used, as exclusive canonicalisation does).

/// The namespace the `xml` prefix is bound to (Namespaces in XML 1.0 §3).
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// The namespace of `xmlns` itself, which no prefix may be bound to (Namespaces in XML 1.0
/// §3).
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// `NCName (':' NCName)?` (Namespaces in XML 1.0 §4, QName): element and attribute names.
fn is_qname(name: &str) -> bool {
    match name.split_once(':') {
        Some((prefix, local)) => super::text::is_ncname(prefix) && super::text::is_ncname(local),
        None => super::text::is_ncname(name),
    }
}

/// Why a fragment has no canonical form here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotCanonical {
    /// Not well-balanced, self-contained XML content: no lexical form of the datatype.
    Malformed,
    /// Well-formed, with a comment or processing instruction: its value isn't decided here.
    Undecided,
}

/// The canonical form of the fragment `text`, or why it has none; the whole fragment is
/// checked before a comment or processing instruction leaves it undecided.
pub fn classify(text: &str) -> Result<String, NotCanonical> {
    let mut p = Parser {
        s: text.as_bytes(),
        at: 0,
        out: String::new(),
        scopes: Vec::new(),
        rendered: Vec::new(),
        undecided: false,
    };
    if p.content(None).is_none() || p.at != p.s.len() {
        return Err(NotCanonical::Malformed);
    }
    if p.undecided {
        return Err(NotCanonical::Undecided);
    }
    Ok(p.out)
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    out: String,
    /// Per open element: its namespace declarations (prefix, IRI).
    scopes: Vec<Vec<(String, String)>>,
    /// Per open element: the declarations its canonical form renders.
    rendered: Vec<Vec<(String, String)>>,
    /// A comment or processing instruction was read: well-formed so far, but not decided.
    undecided: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn starts(&self, prefix: &str) -> bool {
        self.s[self.at..].starts_with(prefix.as_bytes())
    }

    /// `<!--` text `-->` (XML 1.0 §2.5): characters without `--`, not ending in `-`.
    fn comment(&mut self) -> Option<()> {
        self.at += 4;
        let end = self.s[self.at..].windows(3).position(|w| w == b"-->")?;
        let body = std::str::from_utf8(&self.s[self.at..self.at + end]).ok()?;
        if body.contains("--") || body.ends_with('-') || !body.chars().all(super::text::is_xml_char)
        {
            return None;
        }
        self.at += end + 3;
        Some(())
    }

    /// `<?` target (space text)? `?>` (XML 1.0 §2.6): a target that is an NCName (Namespaces
    /// in XML 1.0 §7) and not `xml` in any case, text without `?>`.
    fn instruction(&mut self) -> Option<()> {
        self.at += 2;
        let end = self.s[self.at..].windows(2).position(|w| w == b"?>")?;
        let body = std::str::from_utf8(&self.s[self.at..self.at + end]).ok()?;
        let (target, text) = body
            .split_once([' ', '\t', '\n', '\r'])
            .unwrap_or((body, ""));
        if !super::text::is_ncname(target)
            || target.eq_ignore_ascii_case("xml")
            || !text.chars().all(super::text::is_xml_char)
        {
            return None;
        }
        self.at += end + 2;
        Some(())
    }

    fn space(&mut self) -> bool {
        let from = self.at;
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
        self.at > from
    }

    fn name(&mut self) -> Option<String> {
        let from = self.at;
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() || matches!(b, b'=' | b'>' | b'/' | b'<' | b'"' | b'\'') {
                break;
            }
            self.at += 1;
        }
        let name = std::str::from_utf8(&self.s[from..self.at]).ok()?;
        is_qname(name).then(|| name.to_owned())
    }

    /// A reference after `&`, resolved.
    fn reference(&mut self) -> Option<char> {
        let end = self.s[self.at..].iter().position(|&b| b == b';')?;
        let body = std::str::from_utf8(&self.s[self.at..self.at + end]).ok()?;
        self.at += end + 1;
        let c = match body {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            // `&#[0-9]+;` or `&#x[0-9a-fA-F]+;` (XML 1.0 §4.1, CharRef): the digits are
            // checked here, since Rust's parsers also take a sign.
            _ => {
                let (digits, radix) = match body.strip_prefix("#x") {
                    Some(hex) => (hex, 16),
                    None => (body.strip_prefix('#')?, 10),
                };
                if digits.is_empty() || !digits.bytes().all(|b| (b as char).is_digit(radix)) {
                    return None;
                }
                char::from_u32(u32::from_str_radix(digits, radix).ok()?)?
            }
        };
        super::text::is_xml_char(c).then_some(c)
    }

    /// One character of character data (line ends normalised to `\n`).
    fn char(&mut self) -> Option<char> {
        let rest = std::str::from_utf8(&self.s[self.at..]).ok()?;
        let c = rest.chars().next()?;
        self.at += c.len_utf8();
        if c == '\r' {
            if self.peek() == Some(b'\n') {
                self.at += 1;
            }
            return Some('\n');
        }
        super::text::is_xml_char(c).then_some(c)
    }

    fn escape_text(&mut self, c: char) {
        match c {
            '&' => self.out.push_str("&amp;"),
            '<' => self.out.push_str("&lt;"),
            '>' => self.out.push_str("&gt;"),
            '\r' => self.out.push_str("&#xD;"),
            c => self.out.push(c),
        }
    }

    /// Content up to the end tag of `element` (or the end of the input).
    fn content(&mut self, element: Option<&str>) -> Option<()> {
        loop {
            match self.peek() {
                None => return element.is_none().then_some(()),
                Some(b'<') if self.starts("</") => {
                    let name = element?;
                    self.at += 2;
                    let end = self.name()?;
                    self.space();
                    if end != name || self.peek() != Some(b'>') {
                        return None;
                    }
                    self.at += 1;
                    return Some(());
                }
                Some(b'<') if self.starts("<![CDATA[") => {
                    self.at += 9;
                    let end = self.s[self.at..].windows(3).position(|w| w == b"]]>")?;
                    let text = std::str::from_utf8(&self.s[self.at..self.at + end]).ok()?;
                    self.at += end + 3;
                    for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
                        if !super::text::is_xml_char(c) {
                            return None;
                        }
                        self.escape_text(c);
                    }
                }
                // Comments and processing instructions are checked, then leave the fragment
                // undecided; any other declaration has no place in content.
                Some(b'<') if self.starts("<!--") => {
                    self.comment()?;
                    self.undecided = true;
                }
                Some(b'<') if self.starts("<?") => {
                    self.instruction()?;
                    self.undecided = true;
                }
                Some(b'<') if self.starts("<!") => return None,
                Some(b'<') => self.element()?,
                Some(b'&') => {
                    self.at += 1;
                    let c = self.reference()?;
                    self.escape_text(c);
                }
                // Raw `]]>` outside a CDATA section is not character data (XML 1.0 §2.4).
                Some(b']') if self.starts("]]>") => return None,
                Some(_) => {
                    let c = self.char()?;
                    self.escape_text(c);
                }
            }
        }
    }

    fn element(&mut self) -> Option<()> {
        self.at += 1;
        let name = self.name()?;
        let mut declarations: Vec<(String, String)> = Vec::new();
        let mut attributes: Vec<(String, String)> = Vec::new();
        loop {
            let spaced = self.space();
            match self.peek()? {
                b'/' | b'>' => break,
                _ if !spaced => return None,
                _ => {}
            }
            let attribute = self.name()?;
            self.space();
            if self.peek()? != b'=' {
                return None;
            }
            self.at += 1;
            self.space();
            let quote = self.peek()?;
            if quote != b'"' && quote != b'\'' {
                return None;
            }
            self.at += 1;
            let mut value = String::new();
            loop {
                match self.peek()? {
                    q if q == quote => {
                        self.at += 1;
                        break;
                    }
                    b'<' => return None,
                    b'&' => {
                        self.at += 1;
                        value.push(self.reference()?);
                    }
                    _ => {
                        // Attribute-value normalisation: white space characters as spaces.
                        let c = self.char()?;
                        value.push(if matches!(c, '\t' | '\n') { ' ' } else { c });
                    }
                }
            }
            if attribute == "xmlns" || attribute.starts_with("xmlns:") {
                let prefix = attribute.strip_prefix("xmlns").unwrap_or("");
                let prefix = prefix.strip_prefix(':').unwrap_or(prefix).to_owned();
                // `xml` is bound to its namespace, and nothing else is; `xmlns` is bound to
                // none and its namespace to no prefix; a prefix can't be bound to the empty
                // IRI, only the default can (Namespaces in XML 1.0 §3, §2).
                if declarations.iter().any(|(p, _)| *p == prefix)
                    || (prefix == "xml") != (value == XML_NAMESPACE)
                    || prefix == "xmlns"
                    || value == XMLNS_NAMESPACE
                    || (!prefix.is_empty() && value.is_empty())
                {
                    return None;
                }
                declarations.push((prefix, value));
            } else {
                if attributes.iter().any(|(a, _)| *a == attribute) {
                    return None;
                }
                attributes.push((attribute, value));
            }
        }
        // The tag ends in `/>` or `>`, nothing else (XML 1.0 §3.1, STag and EmptyElemTag).
        let empty = self.starts("/>");
        if empty {
            self.at += 2;
        } else if self.peek() == Some(b'>') {
            self.at += 1;
        } else {
            return None;
        }
        // Exclusive canonicalisation renders a declaration on each element that visibly
        // uses its prefix (in its name or an attribute's), unless an output ancestor
        // already rendered the same; declarations nothing uses are left out.
        self.scopes.push(declarations);
        let prefix_of = |qname: &str| qname.split_once(':').map_or("", |(p, _)| p).to_owned();
        let mut used = vec![prefix_of(&name)];
        for (a, _) in &attributes {
            if a.contains(':') {
                used.push(prefix_of(a));
            }
        }
        used.sort();
        used.dedup();
        let mut rendered: Vec<(String, String)> = Vec::new();
        for prefix in used.iter().filter(|p| *p != "xml") {
            let iri = match self.lookup(prefix) {
                Some(iri) => iri,
                // An unprefixed name without a default namespace: none to render, but an
                // ancestor's rendered default is undone.
                None if prefix.is_empty() => String::new(),
                // A prefix declared outside the fragment.
                None => return None,
            };
            let before = self.rendered_value(prefix).unwrap_or_default();
            if before != iri {
                rendered.push((prefix.clone(), iri));
            }
        }
        // Attributes by namespace IRI, then local name (unqualified ones first).
        let mut keyed: Vec<(String, String, String, String)> = Vec::new();
        for (a, v) in attributes {
            let (ns, local) = match a.split_once(':') {
                Some((p, l)) => (self.lookup(p).unwrap_or_default(), l.to_owned()),
                None => (String::new(), a.clone()),
            };
            keyed.push((ns, local, a, v));
        }
        keyed.sort();
        // Attributes Unique (Namespaces §6.3): by expanded name, so neighbours once sorted.
        if keyed
            .windows(2)
            .any(|w| (&w[0].0, &w[0].1) == (&w[1].0, &w[1].1))
        {
            return None;
        }
        self.out.push('<');
        self.out.push_str(&name);
        for (prefix, iri) in &rendered {
            self.out.push_str(if prefix.is_empty() {
                " xmlns"
            } else {
                " xmlns:"
            });
            self.out.push_str(prefix);
            self.out.push_str("=\"");
            self.escape_attribute(iri);
            self.out.push('"');
        }
        self.rendered.push(rendered);
        for (_, _, a, v) in &keyed {
            self.out.push(' ');
            self.out.push_str(a);
            self.out.push_str("=\"");
            self.escape_attribute(v);
            self.out.push('"');
        }
        self.out.push('>');
        if !empty {
            self.content(Some(&name))?;
        }
        self.out.push_str("</");
        self.out.push_str(&name);
        self.out.push('>');
        self.scopes.pop();
        self.rendered.pop();
        Some(())
    }

    /// The value the nearest output ancestor rendered for `prefix`.
    fn rendered_value(&self, prefix: &str) -> Option<String> {
        self.rendered
            .iter()
            .rev()
            .flat_map(|s| s.iter())
            .find(|(p, _)| p == prefix)
            .map(|(_, iri)| iri.clone())
    }

    /// The namespace IRI `prefix` is bound to; `xml`'s binding needs no declaration.
    fn lookup(&self, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(XML_NAMESPACE.to_owned());
        }
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter())
            .find(|(p, _)| p == prefix)
            .map(|(_, iri)| iri.clone())
    }

    fn escape_attribute(&mut self, v: &str) {
        for c in v.chars() {
            match c {
                '&' => self.out.push_str("&amp;"),
                '<' => self.out.push_str("&lt;"),
                '"' => self.out.push_str("&quot;"),
                '\t' => self.out.push_str("&#x9;"),
                '\n' => self.out.push_str("&#xA;"),
                '\r' => self.out.push_str("&#xD;"),
                c => self.out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NotCanonical, classify};

    /// The canonical form where there is one (malformed and undecided alike: `None`).
    fn canonical(text: &str) -> Option<String> {
        classify(text).ok()
    }

    /// RDF 1.1 Concepts (`rdf:XMLLiteral`: well-balanced, self-contained XML content), XML
    /// 1.0 §2.5–2.6, §4.1 (WFC Entity Declared), Namespaces in XML 1.0 §5: what isn't a
    /// lexical form is malformed, also after a comment or processing instruction; a
    /// well-formed one with either is undecided. Every row is checked.
    #[test]
    fn malformed_and_undecided_are_told_apart() {
        let rows: &[(&str, Result<&str, NotCanonical>)] = &[
            ("<!-- c --><a/>", Err(NotCanonical::Undecided)),
            ("<?pi x?>", Err(NotCanonical::Undecided)),
            ("<a><!--c--></a>", Err(NotCanonical::Undecided)),
            ("<!-- c --><a", Err(NotCanonical::Malformed)),
            ("<?pi x?><a></b>", Err(NotCanonical::Malformed)),
            ("<!-- a -- b -->", Err(NotCanonical::Malformed)),
            ("<!-- a --->", Err(NotCanonical::Malformed)),
            ("<?xml version='1.0'?>", Err(NotCanonical::Malformed)),
            ("<?p:q x?>", Err(NotCanonical::Malformed)),
            ("<!DOCTYPE a>", Err(NotCanonical::Malformed)),
            ("<a>", Err(NotCanonical::Malformed)),
            ("&nbsp;", Err(NotCanonical::Malformed)),
            ("<p:a/>", Err(NotCanonical::Malformed)),
            // CharRef is unsigned digits (XML 1.0 §4.1), in text and attribute values alike.
            ("&#x+41;", Err(NotCanonical::Malformed)),
            ("<!--c-->&#x+41;", Err(NotCanonical::Malformed)),
            ("&#+65;", Err(NotCanonical::Malformed)),
            ("&#x;", Err(NotCanonical::Malformed)),
            ("<a b='&#x+41;'/>", Err(NotCanonical::Malformed)),
            // A tag ends in `>` or `/>` (§3.1); names are QNames, a prefix isn't bound to the
            // empty IRI, `xmlns` isn't declared nor its namespace bound (Namespaces §3, §4).
            ("<a /x></a>", Err(NotCanonical::Malformed)),
            ("<!--c--><a /x></a>", Err(NotCanonical::Malformed)),
            ("<:a/>", Err(NotCanonical::Malformed)),
            (
                "<a xmlns:p='urn:x'><p:b:c/></a>",
                Err(NotCanonical::Malformed),
            ),
            ("<a xmlns:p=''><p:b/></a>", Err(NotCanonical::Malformed)),
            ("<a xmlns:xmlns='urn:x'/>", Err(NotCanonical::Malformed)),
            (
                "<a xmlns:p='http://www.w3.org/2000/xmlns/'/>",
                Err(NotCanonical::Malformed),
            ),
            // Guards (right on the base too).
            ("&#x41;&#65;", Ok("AA")),
            ("<a b='&#x41;'/>", Ok("<a b=\"A\"></a>")),
            ("<a />", Ok("<a></a>")),
            (
                "<a xmlns='urn:x'><b xmlns=''/></a>",
                Ok("<a xmlns=\"urn:x\"><b xmlns=\"\"></b></a>"),
            ),
            ("<a/>", Ok("<a></a>")),
            ("", Ok("")),
        ];
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|&(text, expected)| {
                let got = classify(text);
                (got.as_deref().map_err(|e| *e) != expected).then(|| format!("{text:?}: {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn equal_fragments_have_equal_forms() {
        let x = "http://www.w3.org/1999/xhtml";
        let a = canonical(&format!("<b xmlns=\"{x}\" c='1' a=\"2\"/>text")).unwrap();
        let b = canonical(&format!("<b  a='2' xmlns=\"{x}\"   c=\"1\"></b>te&#x78;t")).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, format!("<b xmlns=\"{x}\" a=\"2\" c=\"1\"></b>text"));
        assert_eq!(canonical("a<![CDATA[<x>]]>"), Some("a&lt;x&gt;".to_owned()));
        // Text differs: different values.
        assert_ne!(canonical("\n<br/>"), canonical("<br/>"));
        assert_ne!(canonical("<b>Good!</b>"), canonical("<b>Bad!</b>"));
    }

    /// Declarations move to where they are used (the W3C test FS2RDF-literals-ar's
    /// literal): unused ones go, used ones render once per output path.
    #[test]
    fn declarations_render_where_used() {
        let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
        let owl = "http://www.w3.org/2002/07/owl#";
        let text = [
            &format!("<rdf:RDF xmlns:ex=\"urn:ex\" xmlns:owl=\"{owl}\" xmlns:rdf=\"{rdf}\">"),
            "<owl:Ontology></owl:Ontology><rdf:Description rdf:about=\"x\">",
            "<owl:differentFrom rdf:resource=\"y\"/></rdf:Description></rdf:RDF>",
        ]
        .concat();
        let expected = [
            &format!("<rdf:RDF xmlns:rdf=\"{rdf}\">"),
            &format!("<owl:Ontology xmlns:owl=\"{owl}\"></owl:Ontology>"),
            "<rdf:Description rdf:about=\"x\">",
            &format!("<owl:differentFrom xmlns:owl=\"{owl}\" rdf:resource=\"y\">"),
            "</owl:differentFrom></rdf:Description></rdf:RDF>",
        ]
        .concat();
        assert_eq!(canonical(&text).unwrap(), expected);
        // The same declared where it is used: the same value.
        let moved = [
            &format!("<rdf:RDF xmlns:rdf=\"{rdf}\"><owl:Ontology xmlns:owl=\"{owl}\"/>"),
            "<rdf:Description rdf:about=\"x\">",
            &format!("<owl:differentFrom xmlns:owl=\"{owl}\" rdf:resource=\"y\"/>"),
            "</rdf:Description></rdf:RDF>",
        ]
        .concat();
        assert_eq!(canonical(&text), canonical(&moved));
        // A default namespace undone below an element that rendered one.
        assert_eq!(
            canonical("<a xmlns=\"urn:x\"><b xmlns=\"\"/></a>").unwrap(),
            "<a xmlns=\"urn:x\"><b xmlns=\"\"></b></a>"
        );
    }

    #[test]
    fn undecided_and_malformed_fragments() {
        for text in [
            "<!-- c --><a/>",
            "<?pi x?>",
            "<a>",
            "<a></b>",
            "&nbsp;",
            "<p:a/>",
            "<a x='1' x='2'/>",
        ] {
            assert_eq!(canonical(text), None, "{text}");
        }
        assert_eq!(canonical(""), Some(String::new()));
        assert_eq!(
            canonical("<a xmlns:p=\"urn:x\"/>"),
            Some("<a></a>".to_owned())
        );
        assert_eq!(canonical("plain"), Some("plain".to_owned()));
    }

    /// XML 1.0 §2.4 (`CharData` excludes `]]>`): raw `]]>` in content is not well-formed,
    /// unlike its escaped form. Namespaces in XML 1.0 §6.3 (Attributes Unique): no two
    /// attributes with the same expanded name, whatever their prefixes. §3: `xml` is bound
    /// to its namespace and no other prefix (nor the default) may be. Malformed: no value.
    #[test]
    fn namespace_malformed_fragments_have_no_value() {
        let xml = "http://www.w3.org/XML/1998/namespace";
        let malformed = [
            "a]]>b".to_owned(),
            "<a>]]></a>".to_owned(),
            "<a xmlns:p=\"urn:x\" xmlns:q=\"urn:x\" p:x=\"1\" q:x=\"2\"/>".to_owned(),
            "<a xmlns:xml=\"urn:x\"/>".to_owned(),
            format!("<a xmlns:p=\"{xml}\" p:z=\"1\"/>"),
            format!("<a xmlns=\"{xml}\"/>"),
        ];
        // Every case is checked, so a failure lists each one given a value.
        let valued: Vec<(&String, String)> = malformed
            .iter()
            .filter_map(|text| Some((text, canonical(text)?)))
            .collect();
        assert!(valued.is_empty(), "{valued:#?}");
        // Guards (right on the base too): the escaped form, and two prefixes on two IRIs.
        assert_eq!(canonical("a]]&gt;b"), Some("a]]&gt;b".to_owned()));
        assert_eq!(
            canonical("<a xmlns:p=\"urn:x\" xmlns:q=\"urn:y\" q:x=\"2\" p:x=\"1\"/>").unwrap(),
            "<a xmlns:p=\"urn:x\" xmlns:q=\"urn:y\" p:x=\"1\" q:x=\"2\"></a>"
        );
    }

    /// Namespaces in XML 1.0 §3: `xml` is bound to its namespace whether declared or not,
    /// so declaring it changes nothing. Canonical XML 1.0 §2.2 orders attributes by
    /// namespace IRI, then local name; exclusive canonicalisation renders no `xml`
    /// declaration. `http://a/` sorts before `http://www.w3.org/XML/1998/namespace`.
    #[test]
    fn the_xml_prefix_is_bound_whether_declared_or_not() {
        let xml = "http://www.w3.org/XML/1998/namespace";
        let expected = "<a xmlns:p=\"http://a/\" p:b=\"2\" xml:z=\"1\"></a>";
        let texts = [
            "<a xmlns:p=\"http://a/\" xml:z=\"1\" p:b=\"2\"/>".to_owned(),
            "<a xmlns:p=\"http://a/\" p:b=\"2\" xml:z=\"1\"/>".to_owned(),
            // A guard (right on the base too): with the declaration, xml:z was resolved.
            format!("<a xmlns:p=\"http://a/\" xmlns:xml=\"{xml}\" xml:z=\"1\" p:b=\"2\"/>"),
        ];
        // Every case is checked, so a failure lists each wrong one.
        let wrong: Vec<(&String, Option<String>)> = texts
            .iter()
            .map(|text| (text, canonical(text)))
            .filter(|(_, form)| form.as_deref() != Some(expected))
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
