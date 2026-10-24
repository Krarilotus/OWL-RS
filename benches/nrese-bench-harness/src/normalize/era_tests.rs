use super::*;
use std::fs;
use std::path::PathBuf;

fn set(v: &mut J, key: &'static str, value: J) {
    let Value::Object(o) = v else {
        panic!("object");
    };
    o.insert(key, value);
}
fn slot<'a>(v: &'a mut J, key: &str) -> &'a mut J {
    let Value::Object(o) = v else {
        panic!("object");
    };
    o.get_mut(key).unwrap()
}
fn row(v: &mut J) -> &mut J {
    let Value::Array(a) = slot(v, "results") else {
        panic!("array");
    };
    &mut a[0]
}
fn write_json(path: &Path, value: &J) {
    fs::write(path, value.to_string()).unwrap();
}
fn rebind(dir: &Path) {
    let mut anchors = json(&fs::read(dir.join("anchors.json")).unwrap()).unwrap();
    let Value::Array(sources) = slot(&mut anchors, "sources") else {
        panic!("sources");
    };
    for source in sources {
        let hash = digest(&fs::read(dir.join(string(source, "path").unwrap())).unwrap());
        set(source, "sha256", text(hash));
    }
    write_json(&dir.join("anchors.json"), &anchors);
    let mut meta = json(&fs::read(dir.join("observation.json")).unwrap()).unwrap();
    for (key, file) in [("report", "report.nq"), ("anchors", "anchors.json")] {
        set(
            slot(&mut meta, key),
            "sha256",
            text(digest(&fs::read(dir.join(file)).unwrap())),
        );
    }
    write_json(&dir.join("observation.json"), &meta);
}
fn long_path(dir: &Path, count: usize, tail: &str) {
    let old = fs::read_to_string(dir.join("report.nq")).unwrap();
    let mut rdf = old
        .lines()
        .filter(|l| !l.starts_with("_:path"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    rdf = rdf.replace("_:path0 .", "_:long0 .");
    let mut items = Vec::new();
    for i in 0..count {
        let iri = if i + 1 == count {
            tail.to_owned()
        } else {
            format!("urn:era-tiny:p{i}")
        };
        rdf += &format!("_:long{i} <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <{iri}> .\n");
        let rest = if i + 1 == count {
            "<http://www.w3.org/1999/02/22-rdf-syntax-ns#nil>".to_owned()
        } else {
            format!("_:long{}", i + 1)
        };
        rdf += &format!("_:long{i} <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> {rest} .\n");
        items.push(object([
            ("kind", text("Predicate")),
            ("iri", text(format!("<{iri}>"))),
        ]));
    }
    fs::write(dir.join("report.nq"), rdf).unwrap();
    let mut a = json(&fs::read(dir.join("anchors.json")).unwrap()).unwrap();
    set(
        &mut a,
        "fresh_report_nodes",
        Value::Array(
            ["root".to_owned(), "result".to_owned()]
                .into_iter()
                .chain((0..count).map(|i| format!("long{i}")))
                .map(text)
                .collect(),
        ),
    );
    write_json(&dir.join("anchors.json"), &a);
    let mut m = json(&fs::read(dir.join("observation.json")).unwrap()).unwrap();
    set(
        row(&mut m),
        "path",
        object([("kind", text("Sequence")), ("items", Value::Array(items))]),
    );
    write_json(&dir.join("observation.json"), &m);
}
/// Spec: N11 era-export-contract and Root thin-bridge instruction require strict
/// complete anchored RDF and all eight typed fields, with semantic loss != MATCH.
/// Hand-worked expected verdicts below are independent of comparator output.
#[test]
fn era_artifact_contract_table() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = if env!("CARGO_PKG_NAME") == "nrese-rdf-io" {
        manifest.join("../../../benches/nrese-bench-harness/fixtures/era-private")
    } else {
        manifest.join("fixtures/era-private")
    };
    let out = std::env::var_os("ERA_PROOF_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("era-glue-{}", std::process::id())));
    fs::create_dir_all(&out).unwrap();
    let mut cases_run = 0usize;
    for (case, expected) in [
        ("identical", "MATCH"),
        ("fresh-renamed", "MATCH"),
        ("original-message-language", "DIFFERENT"),
        ("numeric-lexical", "DIFFERENT"),
        ("source-anchor-sharing", "DIFFERENT"),
        ("named-graph-sharing", "DIFFERENT"),
        ("typed-source-constraint-absent", "DIFFERENT"),
        ("duplicate-result", "DIFFERENT"),
        ("duplicate-message", "DIFFERENT"),
        ("conforms", "DIFFERENT"),
        ("failure", "DIFFERENT"),
        ("applicability", "DIFFERENT"),
        ("introduced-role", "DIFFERENT"),
        ("rollback-role", "DIFFERENT"),
        ("top-versus-detail", "DIFFERENT"),
        ("list65-tail", "DIFFERENT"),
        ("malformed-rdf", "UNQUALIFIED"),
        ("missing-anchor", "UNQUALIFIED"),
        ("cancelled", "UNQUALIFIED"),
        ("named-projection", "UNQUALIFIED"),
        ("generated-message-projection", "MATCH"),
        ("source-message-projection-refused", "UNQUALIFIED"),
        ("runtime-arbitrary-receipt", "UNQUALIFIED"),
        ("runtime-unsupported-receipt", "UNQUALIFIED"),
        ("authored-runtime-relabel", "UNQUALIFIED"),
        ("typed-message-language", "DIFFERENT"),
    ] {
        // Same owning table, focused old-source control only in the proof.
        if std::env::var("ERA_CONTRACT_ONLY")
            .ok()
            .is_some_and(|only| only != case)
        {
            continue;
        }
        cases_run += 1;
        let work = out.join(format!("{case}-{}", std::process::id()));
        fs::create_dir(&work).unwrap();
        for side in ["left", "right"] {
            let dir = work.join(side);
            fs::create_dir(&dir).unwrap();
            for file in [
                "data.nq",
                "shapes.ttl",
                "report.nq",
                "anchors.json",
                "observation.json",
            ] {
                fs::copy(fixture.join(file), dir.join(file)).unwrap();
            }
        }
        let left = work.join("left");
        let right = work.join("right");
        let mut m = json(&fs::read(right.join("observation.json")).unwrap()).unwrap();
        let mut a = json(&fs::read(right.join("anchors.json")).unwrap()).unwrap();
        let mut rdf = fs::read_to_string(right.join("report.nq")).unwrap();
        match case {
            "identical"
            | "runtime-arbitrary-receipt"
            | "runtime-unsupported-receipt"
            | "authored-runtime-relabel" => {}
            "typed-message-language" => {
                let Value::Array(messages) = slot(row(&mut m), "messages") else {
                    panic!("messages");
                };
                messages[0] = text(messages[0].as_str().unwrap().replace("@fr", "@de"));
            }
            "fresh-renamed" => {
                for (old, new) in [
                    ("root", "freshRoot"),
                    ("result", "freshResult"),
                    ("path0", "fresh0"),
                    ("path1", "fresh1"),
                ] {
                    rdf = rdf.replace(&format!("_:{old} "), &format!("_:{new} "));
                }
                set(
                    &mut a,
                    "fresh_report_nodes",
                    Value::Array(
                        ["freshRoot", "freshResult", "fresh0", "fresh1"]
                            .into_iter()
                            .map(text)
                            .collect(),
                    ),
                );
            }
            "original-message-language" => {
                rdf = rdf.replace("\"étiquette requise\"@fr", "\"étiquette requise\"@de")
            }
            "numeric-lexical" => rdf = rdf.replace("\"01\"^^", "\"1\"^^"),
            "source-anchor-sharing" => {
                rdf = rdf.replace("#focusNode> _:item", "#focusNode> _:other")
            }
            "named-graph-sharing" => {
                let data = fs::read_to_string(right.join("data.nq")).unwrap();
                fs::write(
                    right.join("data.nq"),
                    data.replace("_:item <urn:era-tiny:code>", "_:other <urn:era-tiny:code>"),
                )
                .unwrap();
            }
            "typed-source-constraint-absent" => set(row(&mut m), "source_constraint", Value::Null),
            "duplicate-result" => {
                let Value::Array(rows) = slot(&mut m, "results") else {
                    panic!("rows");
                };
                rows.push(rows[0].clone());
            }
            "duplicate-message" => {
                let Value::Array(messages) = slot(row(&mut m), "messages") else {
                    panic!("messages");
                };
                messages.push(messages[0].clone());
            }
            "conforms" => set(&mut m, "conforms", Value::Boolean(true)),
            "failure" => set(
                &mut m,
                "failures",
                Value::Array(vec![text("retained validation failure")]),
            ),
            "applicability" => set(&mut m, "applicable", Value::Boolean(false)),
            "introduced-role" => set(&mut m, "observation_role", text("introduced")),
            "rollback-role" => set(
                &mut m,
                "observation_role",
                text("persistent-after-rollback"),
            ),
            "top-versus-detail" => rdf = rdf.replace("#result> _:result", "#detail> _:result"),
            "list65-tail" => {}
            "malformed-rdf" => rdf.push_str("\n<unterminated"),
            "missing-anchor" => {
                let Value::Array(bindings) = slot(&mut a, "bindings") else {
                    panic!("bindings");
                };
                bindings.remove(0);
            }
            "cancelled" => set(
                &mut m,
                "operational_outcome",
                object([
                    ("kind", text("Cancelled")),
                    ("payload", text("original typed tag retained")),
                ]),
            ),
            "named-projection" => set(
                &mut m,
                "projection",
                text("generated-detail-projection-not-yet-implemented"),
            ),
            "generated-message-projection" => {
                let mut lm = json(&fs::read(left.join("observation.json")).unwrap()).unwrap();
                set(
                    &mut lm,
                    "projection",
                    object([
                        (
                            "name",
                            text("source-message-preserving-generated-message-projection"),
                        ),
                        ("generated_message_terms", Value::Array(Vec::new())),
                    ]),
                );
                write_json(&left.join("observation.json"), &lm);
                set(
                    &mut m,
                    "projection",
                    object([
                        (
                            "name",
                            text("source-message-preserving-generated-message-projection"),
                        ),
                        (
                            "generated_message_terms",
                            Value::Array(vec![text("\"generated diagnostic\"@en")]),
                        ),
                    ]),
                );
                rdf.push_str("_:result <http://www.w3.org/ns/shacl#resultMessage> \"generated diagnostic\"@en .\n");
                let Value::Array(messages) = slot(row(&mut m), "messages") else {
                    panic!("messages");
                };
                messages.push(text("\"generated diagnostic\"@en"));
            }
            "source-message-projection-refused" => set(
                &mut m,
                "projection",
                object([
                    (
                        "name",
                        text("source-message-preserving-generated-message-projection"),
                    ),
                    (
                        "generated_message_terms",
                        Value::Array(vec![text("\"étiquette requise\"@fr")]),
                    ),
                ]),
            ),
            _ => unreachable!(),
        }
        fs::write(right.join("report.nq"), rdf).unwrap();
        write_json(&right.join("anchors.json"), &a);
        write_json(&right.join("observation.json"), &m);
        if case == "list65-tail" {
            long_path(&left, 65, "urn:era-tiny:last");
            long_path(&right, 65, "urn:era-tiny:changed-last");
        }
        rebind(&left);
        rebind(&right);
        // R1-F1 spec: relabelled authored declarations and arbitrary retained
        // bytes are not actual source/load/snapshot/call/report observations.
        if matches!(
            case,
            "runtime-arbitrary-receipt"
                | "runtime-unsupported-receipt"
                | "authored-runtime-relabel"
        ) {
            for dir in [&left, &right] {
                let mut meta = json(&fs::read(dir.join("observation.json")).unwrap()).unwrap();
                let mut anchors = json(&fs::read(dir.join("anchors.json")).unwrap()).unwrap();
                set(&mut meta, "capture_kind", text("same-validation-call"));
                set(&mut anchors, "capture_kind", text("same-validation-call"));
                if case != "authored-runtime-relabel" {
                    let file = if case == "runtime-arbitrary-receipt" {
                        "data.nq"
                    } else {
                        "unsupported-receipt.json"
                    };
                    if file == "unsupported-receipt.json" {
                        fs::write(
                            dir.join(file),
                            b"{\"schema\":\"unsupported-caller-assertion\"}",
                        )
                        .unwrap();
                    }
                    set(
                        &mut anchors,
                        "producer_receipt",
                        object([
                            ("path", text(file)),
                            ("sha256", text(digest(&fs::read(dir.join(file)).unwrap()))),
                        ]),
                    );
                }
                write_json(&dir.join("anchors.json"), &anchors);
                set(
                    slot(&mut meta, "anchors"),
                    "sha256",
                    text(digest(&fs::read(dir.join("anchors.json")).unwrap())),
                );
                write_json(&dir.join("observation.json"), &meta);
            }
        }
        let result = compare_paths(
            &left.join("observation.json"),
            &right.join("observation.json"),
        );
        write_json(&out.join(format!("{case}.json")), &result);
        assert_eq!(
            string(&result, "verdict").unwrap(),
            expected,
            "contract case {case}: {result}"
        );
        if matches!(
            case,
            "runtime-arbitrary-receipt"
                | "runtime-unsupported-receipt"
                | "authored-runtime-relabel"
        ) {
            assert_eq!(
                get(&result, "full_graph_certificate").unwrap().as_bool(),
                Some(false)
            );
        }
        if case == "generated-message-projection" {
            assert_eq!(
                get(&result, "full_graph_certificate").unwrap().as_bool(),
                Some(false)
            );
        }
        println!("ERA contract {case}: {expected}");
        fs::remove_dir_all(&work).unwrap();
    }
    assert!(cases_run > 0, "requested contract case was not selected");
}
