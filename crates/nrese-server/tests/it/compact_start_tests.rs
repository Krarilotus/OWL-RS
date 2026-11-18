//! A server started on an empty data directory answers through `owl:sameAs` from its first
//! commits, `reasoner.equality = "compact"` included. This build's server binary
//! ([`super::upgrade_tests::Server`]), so the start path, its reasoning before it serves
//! included, is the one an operator runs.

use super::upgrade_tests::{Server, configure};

const PREFIXES: &str = "PREFIX ex: <http://example.com/>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    PREFIX owl: <http://www.w3.org/2002/07/owl#> ";

/// The class table's data (`status_class_tests.rs`), in its two parts.
const FIRST: &str = "ex:Dog rdfs:subClassOf ex:Animal . ex:rex a ex:Dog .
    ex:Employee rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:worksFor ;
    owl:someValuesFrom owl:Thing ] .";
const SECOND: &str = "ex:a a ex:Dog . ex:a owl:sameAs ex:b . ex:bob a ex:Employee .";

/// Under every ruleset that closes `owl:sameAs`: those whose closure of an empty store is
/// empty (`rdfs-plus`, `owl-horst`: no axioms) as well as those with axioms (`owl2-rl`);
/// under `reasoner.equality` `representatives` (the default) and `compact`; the data in one
/// commit and in two.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_started_empty_answers_through_its_equalities()
-> Result<(), Box<dyn std::error::Error>> {
    let mut wrong = Vec::new();
    for mode in ["rdfs-plus", "owl-horst", "owl2-rl"] {
        for (equality, way) in [
            ("representatives", "load"),
            ("representatives", "commit"),
            ("compact", "load"),
            ("compact", "commit"),
        ] {
            let dir = tempfile::tempdir()?;
            let data = dir.path().join("data");
            std::fs::create_dir_all(&data)?;
            let file = configure(dir.path(), &data)?;
            let reasoner = "[reasoner]\nmode = \"owl2-rl\"\n";
            let text = std::fs::read_to_string(&file)?;
            if !text.contains(reasoner) {
                return Err(format!("the configuration has no {reasoner:?}:\n{text}").into());
            }
            let setting = format!("[reasoner]\nmode = \"{mode}\"\nequality = \"{equality}\"\n");
            std::fs::write(&file, text.replace(reasoner, &setting))?;
            let server = Server::start(&file).await?;
            let commits = match way {
                "load" => vec![format!("{FIRST} {SECOND}")],
                _ => vec![FIRST.to_owned(), SECOND.to_owned()],
            };
            for commit in commits {
                server
                    .update("nrese", &format!("{PREFIXES} INSERT DATA {{ {commit} }}"))
                    .await?;
            }
            let query = format!("{PREFIXES} SELECT ?x WHERE {{ ?x a ex:Animal }}");
            let rows = server.query("nrese", &query).await?;
            let right = rows.iter().any(|row| row.contains("example.com/b>"));
            println!(
                "CASE {mode} {equality} {way}: {} {rows:?}",
                if right { "right" } else { "WRONG" }
            );
            if !right {
                wrong.push(format!("{mode} {equality} {way}"));
            }
        }
    }
    assert!(wrong.is_empty(), "ex:b missing under {wrong:?}");
    Ok(())
}
