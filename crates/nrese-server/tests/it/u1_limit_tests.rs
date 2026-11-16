//! A key longer than one of U1's rules may hold (two atoms per property, numbered by `u8`),
//! off the RL route, through `nrese-server load` under `owl2-dl`: the load ends as a load of
//! consistent data does, never with a panic.

use std::process::Command;

use crate::upgrade_tests::configure;

const EX: &str = "http://example.com/";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// A union superclass (outside OWL 2 RL), `:K owl:hasKey ( :k1 … :kn )` and two `:K` with
/// the same values (Turtle, full IRIs).
fn long_key(n: usize) -> String {
    let members: Vec<String> = (1..=n).map(|i| format!("<{EX}k{i}>")).collect();
    let mut data = format!(
        "<{EX}A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> [ <{OWL}unionOf> ( <{EX}B> <{EX}C> ) ] .\n\
         <{EX}K> <{OWL}hasKey> ( {} ) .\n",
        members.join(" ")
    );
    for x in ["x", "y"] {
        data.push_str(&format!("<{EX}{x}> a <{EX}K>"));
        for i in 1..=n {
            data.push_str(&format!(" ; <{EX}k{i}> <{EX}v{i}>"));
        }
        data.push_str(" .\n");
    }
    data
}

#[test]
fn load_takes_a_long_key_off_the_rules_route() -> Result<(), Box<dyn std::error::Error>> {
    let mut wrong = Vec::new();
    for n in [127, 128, 251] {
        let dir = tempfile::tempdir()?;
        let config = configure(dir.path(), &dir.path().join("data"))?;
        let file = dir.path().join("key.ttl");
        std::fs::write(&file, long_key(n))?;
        let output = Command::new(env!("CARGO_BIN_EXE_nrese-server"))
            .arg("load")
            .arg("--config")
            .arg(&config)
            .args(["--set", "reasoner.mode=owl2-dl"])
            .arg(&file)
            .env("NO_COLOR", "1")
            .env_remove("RUST_LOG")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last: Vec<&str> = stderr.lines().rev().take(4).collect();
            wrong.push(format!("{n} properties: {}: {last:?}", output.status));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}
