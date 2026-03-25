use crate::error::{NreseError, NreseResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IriRef<'a> {
    value: &'a str,
}

impl<'a> IriRef<'a> {
    pub fn new(value: &'a str) -> NreseResult<Self> {
        if value.contains(':') {
            Ok(Self { value })
        } else {
            Err(NreseError::InvalidIri(value.to_owned()))
        }
    }

    pub fn as_str(self) -> &'a str {
        self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TripleRef<'a> {
    pub subject: IriRef<'a>,
    pub predicate: IriRef<'a>,
    pub object: IriRef<'a>,
}

impl<'a> TripleRef<'a> {
    pub fn new(subject: IriRef<'a>, predicate: IriRef<'a>, object: IriRef<'a>) -> Self {
        Self {
            subject,
            predicate,
            object,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IriRef, TripleRef};

    #[test]
    fn iri_ref_rejects_non_iri_values() {
        let result = IriRef::new("not-an-iri");
        assert!(result.is_err());
    }

    #[test]
    fn triple_ref_preserves_components() {
        let subject = IriRef::new("urn:test:s").expect("subject iri");
        let predicate = IriRef::new("urn:test:p").expect("predicate iri");
        let object = IriRef::new("urn:test:o").expect("object iri");
        let triple = TripleRef::new(subject, predicate, object);

        assert_eq!(triple.subject.as_str(), "urn:test:s");
        assert_eq!(triple.predicate.as_str(), "urn:test:p");
        assert_eq!(triple.object.as_str(), "urn:test:o");
    }
}
