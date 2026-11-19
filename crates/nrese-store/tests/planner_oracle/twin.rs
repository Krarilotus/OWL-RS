//! A draw marker has one real spelling and one deterministic spelling. Keep
//! its placement identical so grouping and correlations are tested on both.

#[derive(Clone, Copy, Debug)]
pub enum Draw {
    Value {
        call: &'static str,
        column: &'static str,
        keys: &'static [&'static str],
    },
    Filter {
        keys: &'static [&'static str],
        independent: bool,
    },
    Average,
}

#[derive(Clone)]
pub struct Query {
    // $DRAW and, for the second UNION arm, $OTHER mark expression sites.
    pub text: String,
    pub draw: Option<Draw>,
    // A separate SELECT over the input of the site counts occurrences before
    // joins can copy them. By default each key names one input solution.
    pub scope: Option<String>,
    pub reduced: bool,
}

impl Query {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            draw: None,
            scope: None,
            reduced: false,
        }
    }

    pub fn drawing(text: String, draw: Draw) -> Self {
        Self {
            draw: Some(draw),
            ..Self::plain(text)
        }
    }

    pub fn real(&self) -> String {
        match self.draw {
            None => self.text.clone(),
            Some(draw) => {
                let call = match draw {
                    Draw::Value { call, .. } => call,
                    Draw::Filter { .. } => "RAND() < 0.5",
                    Draw::Average => "IF(RAND() < 0.5, 1, 1 / 0)",
                };
                self.text.replace("$DRAW", call).replace("$OTHER", call)
            }
        }
    }

    pub fn twin(&self) -> String {
        let (first, other) = match self.draw {
            None => return self.text.clone(),
            Some(Draw::Filter { .. }) => ("true".into(), "true".into()),
            Some(Draw::Average) => ("1".into(), "1".into()),
            Some(Draw::Value { keys, .. }) => {
                (key_expression("first", keys), key_expression("other", keys))
            }
        };
        self.text.replace("$DRAW", &first).replace("$OTHER", &other)
    }
}

pub fn key_expression(site: &str, keys: &[&str]) -> String {
    // A length prefix distinguishes even strings that contain separators.
    // Generated keys use fixed RDF term types within each column.
    let mut args = vec![format!("\"{site}\"")];
    for key in keys {
        let value = format!("STR(?{key})");
        args.push(format!(
            "IF(BOUND(?{key}), CONCAT(STR(STRLEN({value})), \":\", {value}), \"-\")"
        ));
    }
    format!("CONCAT({})", args.join(", "))
}
