//! `rdf:XMLLiteral` values: an XML fragment's canonical form (Exclusive XML
//! Canonicalization, as RDF 2004 defines the value), so that two literals are the same
//! value iff their forms are equal.
//!
//! Only fragments whose canonical form doesn't depend on what this leaves out are decided:
//! elements, attributes, text, character and the predefined entity references, CDATA
//! sections, and namespace declarations (rendered where visibly used, as exclusive
//! canonicalisation does). Comments, processing instructions, document type declarations,
//! other entities and prefixes declared outside the fragment give `None`: the caller
//! doesn't know the value.

/// The canonical form of the fragment `text`, if it is well-formed and decided here.
pub fn canonical(text: &str) -> Option<String> {
    let mut p = Parser {
        s: text.as_bytes(),
        at: 0,
        out: String::new(),
        scopes: Vec::new(),
        rendered: Vec::new(),
    };
    p.content(None)?;
    (p.at == p.s.len()).then_some(p.out)
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    out: String,
    /// Per open element: its namespace declarations (prefix, IRI).
    scopes: Vec<Vec<(String, String)>>,
    /// Per open element: the declarations its canonical form renders.
    rendered: Vec<Vec<(String, String)>>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn starts(&self, prefix: &str) -> bool {
        self.s[self.at..].starts_with(prefix.as_bytes())
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
        (super::text::is_name(name)).then(|| name.to_owned())
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
            _ => {
                let code = if let Some(hex) = body.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    body.strip_prefix('#')?.parse().ok()?
                };
                char::from_u32(code)?
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
                // Comments, processing instructions, declarations: not decided.
                Some(b'<') if self.starts("<!") || self.starts("<?") => return None,
                Some(b'<') => self.element()?,
                Some(b'&') => {
                    self.at += 1;
                    let c = self.reference()?;
                    self.escape_text(c);
                }
                Some(b'>') if self.starts("]]>") => return None,
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
                if declarations.iter().any(|(p, _)| *p == prefix) {
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
        let empty = self.starts("/>");
        if empty {
            self.at += 2;
        } else {
            self.at += 1;
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

    fn lookup(&self, prefix: &str) -> Option<String> {
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
    use super::canonical;

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
}
