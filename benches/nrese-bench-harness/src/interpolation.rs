use anyhow::{Result, bail};

use crate::model::CompatHeaders;

pub fn expand_headers_env_placeholders(headers: &mut CompatHeaders) -> Result<()> {
    for value in headers.values_mut() {
        *value = expand_env_placeholders(value)?;
    }
    Ok(())
}

fn expand_env_placeholders(input: &str) -> Result<String> {
    expand_with_lookup(input, |name| std::env::var(name).ok())
}

fn expand_with_lookup(
    input: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_offset) = input[cursor..].find("${") {
        let start = cursor + start_offset;
        output.push_str(&input[cursor..start]);

        let token_start = start + 2;
        let Some(end_offset) = input[token_start..].find('}') else {
            bail!("unterminated environment placeholder in '{input}'");
        };
        let end = token_start + end_offset;
        let variable_name = &input[token_start..end];
        if variable_name.is_empty() {
            bail!("empty environment placeholder in '{input}'");
        }

        let Some(value) = lookup(variable_name) else {
            bail!("missing environment variable '{variable_name}' required by workload pack");
        };
        output.push_str(&value);
        cursor = end + 1;
    }

    output.push_str(&input[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{expand_headers_env_placeholders, expand_with_lookup};
    use crate::model::CompatHeaders;

    #[test]
    fn expands_exact_placeholder() {
        let rendered = expand_with_lookup("${TOKEN}", |name| {
            if name == "TOKEN" {
                Some("secret".to_owned())
            } else {
                None
            }
        })
        .expect("rendered");

        assert_eq!(rendered, "secret");
    }

    #[test]
    fn expands_embedded_placeholder() {
        let rendered = expand_with_lookup("Bearer ${TOKEN}", |name| {
            if name == "TOKEN" {
                Some("secret".to_owned())
            } else {
                None
            }
        })
        .expect("rendered");

        assert_eq!(rendered, "Bearer secret");
    }

    #[test]
    fn rejects_missing_placeholder_value() {
        let error = expand_with_lookup("Bearer ${TOKEN}", |_| None).expect_err("missing value");

        assert!(error.to_string().contains("missing environment variable 'TOKEN'"));
    }

    #[test]
    fn expands_all_header_values() {
        let mut headers = CompatHeaders::new();
        headers.insert("authorization".to_owned(), "Bearer ${TOKEN}".to_owned());
        headers.insert("x-test".to_owned(), "static".to_owned());

        let previous = std::env::var("TOKEN").ok();
        unsafe {
            std::env::set_var("TOKEN", "secret");
        }
        let result = expand_headers_env_placeholders(&mut headers);
        match previous {
            Some(value) => unsafe {
                std::env::set_var("TOKEN", value);
            },
            None => unsafe {
                std::env::remove_var("TOKEN");
            },
        }

        result.expect("headers");
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer secret")
        );
        assert_eq!(headers.get("x-test").map(String::as_str), Some("static"));
    }
}
