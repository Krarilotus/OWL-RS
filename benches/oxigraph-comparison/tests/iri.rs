//! `nrese_rdf::Iri` against `oxiri::Iri` on generated references: which texts each
//! accepts, and what resolving them against a set of bases gives.

use std::collections::BTreeMap;

use oxigraph_comparison::{iri_reference, rng};

const BASES: [&str; 6] = [
    "http://a/b/c/d;p?q",
    "http://example.org",
    "urn:x:y",
    "a:/b",
    "file:///C:/data/x.nt#f",
    "http://[::1]:8080/p/q/",
];

/// Where the two disagree, grouped by kind, with up to five examples each.
#[derive(Default)]
struct Differences(BTreeMap<String, Vec<String>>);

impl Differences {
    fn add(&mut self, kind: &str, example: String) {
        let examples = self.0.entry(kind.to_owned()).or_default();
        if examples.len() < 5 {
            examples.push(example);
        }
    }

    fn report(&self) -> String {
        self.0
            .iter()
            .map(|(kind, examples)| format!("{kind}:\n    {}", examples.join("\n    ")))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// RFC 3986 Appendix B: scheme, authority, path, query, fragment.
type Components<'a> = (Option<&'a str>, Option<&'a str>, &'a str, Option<&'a str>, Option<&'a str>);

fn split(text: &str) -> Components<'_> {
    let (rest, fragment) = match text.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (text, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (scheme, rest) = match rest.split_once(':') {
        Some((s, r)) if !s.is_empty() && !s.contains('/') => (Some(s), r),
        _ => (None, rest),
    };
    let (authority, path) = match rest.strip_prefix("//") {
        Some(after) => match after.find('/') {
            Some(i) => (Some(&after[..i]), &after[i..]),
            None => (Some(after), ""),
        },
        None => (None, rest),
    };
    (scheme, authority, path, query, fragment)
}

/// RFC 3986 §5.2.4, transcribed.
fn remove_dot_segments(path: &str) -> String {
    let mut input = path.to_owned();
    let mut output = String::new();
    while !input.is_empty() {
        if input.starts_with("../") {
            input.drain(..3);
        } else if input.starts_with("./") {
            input.drain(..2);
        } else if input.starts_with("/./") {
            input.replace_range(..3, "/");
        } else if input == "/." {
            input = "/".to_owned();
        } else if input.starts_with("/../") || input == "/.." {
            let n = if input == "/.." { 3 } else { 4 };
            input.replace_range(..n, "/");
            output.truncate(output.rfind('/').unwrap_or(0));
        } else if input == "." || input == ".." {
            input.clear();
        } else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |i| i + start);
            output.push_str(&input[..end]);
            input.drain(..end);
        }
    }
    output
}

/// RFC 3986 §5.2.2 and §5.3, transcribed, for references both accept as syntax. One
/// addition, which RFC 3986 leaves open: a path starting with `//` and no authority gets
/// `/.` in front, as the WHATWG URL standard does, so the result stays an IRI.
fn rfc_resolve(base: &str, reference: &str) -> String {
    let (b_scheme, b_authority, b_path, b_query, _) = split(base);
    let (r_scheme, r_authority, r_path, r_query, r_fragment) = split(reference);
    let (scheme, authority, path, query);
    if r_scheme.is_some() {
        (scheme, authority, path, query) = (r_scheme, r_authority, remove_dot_segments(r_path), r_query);
    } else if r_authority.is_some() {
        (scheme, authority, path, query) = (b_scheme, r_authority, remove_dot_segments(r_path), r_query);
    } else if r_path.is_empty() {
        (scheme, authority, path, query) = (b_scheme, b_authority, b_path.to_owned(), r_query.or(b_query));
    } else {
        let merged = if r_path.starts_with('/') {
            r_path.to_owned()
        } else if b_authority.is_some() && b_path.is_empty() {
            format!("/{r_path}")
        } else {
            match b_path.rfind('/') {
                Some(i) => format!("{}{r_path}", &b_path[..=i]),
                None => r_path.to_owned(),
            }
        };
        (scheme, authority, path, query) = (b_scheme, b_authority, remove_dot_segments(&merged), r_query);
    }
    let mut out = format!("{}:", scheme.unwrap_or_default());
    if let Some(a) = authority {
        out.push_str("//");
        out.push_str(a);
    } else if path.starts_with("//") {
        out.push_str("/.");
    }
    out.push_str(&path);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = r_fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Where oxiri departs from RFC 3986, the transcription above decides; nrese must match
/// it. The departures seen: dot segments kept in a reference with an authority (§5.2.2
/// removes them), the root `/` lost when `..` climbs to the top of a path without an
/// authority (`a:/b` + `../` is `a:/`), and no result where the path would start with
/// `//`.
fn explained(base: &str, reference: &str, ours: &Result<String, nrese_rdf::IriParseError>) -> bool {
    ours.as_ref().is_ok_and(|ours| *ours == rfc_resolve(base, reference))
}

#[test]
fn acceptance_and_resolution_agree() {
    let mut rng = rng(3987);
    let mut differences = Differences::default();
    let mut compared = 0;
    let mut departures = 0;
    for _ in 0..200_000 {
        let reference = iri_reference(&mut rng);
        compared += 1;
        let ours = nrese_rdf::Iri::parse(reference.as_str()).is_ok();
        let theirs = oxiri::Iri::parse(reference.as_str()).is_ok();
        if ours != theirs {
            differences.add(
                if ours { "accepted only by nrese" } else { "accepted only by oxiri" },
                format!("{reference:?}"),
            );
        }
        for base in BASES {
            let ours = nrese_rdf::Iri::parse(base).unwrap().resolve(&reference).map(|i| i.into_inner());
            let theirs = oxiri::Iri::parse(base).unwrap().resolve(&reference).map(|i| i.into_inner());
            if ours.as_ref().ok() != theirs.as_ref().ok() && explained(base, &reference, &ours) {
                departures += 1;
                continue;
            }
            match (ours, theirs) {
                (Ok(a), Ok(b)) if a != b => differences.add("resolved differently", format!("{base} + {reference:?}: nrese {a:?}, oxiri {b:?}")),
                (Ok(a), Err(_)) => differences.add("resolved only by nrese", format!("{base} + {reference:?} = {a:?}")),
                (Err(_), Ok(b)) => differences.add("resolved only by oxiri", format!("{base} + {reference:?} = {b:?}")),
                _ => {}
            }
        }
    }
    assert!(differences.0.is_empty(), "{compared} references compared\n{}", differences.report());
    println!("{compared} references; {departures} resolutions where oxiri departs from RFC 3986 and nrese follows it");
}
