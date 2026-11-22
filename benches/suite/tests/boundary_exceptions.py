"""Non-evidence writes and overlay-independent starts for the boundary check."""

# Match the file, function, operation and concrete destination or argv together.
WRITES = {
    # Transient ERA container build context.
    ("benches/breadth/era-shacl/run.py", "images", "copy", "context / 'perf_lab'"),
    ("benches/breadth/era-shacl/run.py", "images", "copy", "context / 'nrese-era.sh'"),
    # N-Triples generated as ERA input.
    ("benches/breadth/era-shacl/run.py", "triples", "open", "nt"),
    # Staged Git source mirror.
    ("benches/breadth/kit.py", "checkout", "rename", "mirror"),
    # ANN sources and binaries installed into an upstream build checkout.
    ("benches/breadth/vectors/ann.py", "checkout", "copytree", "target"),
    ("benches/breadth/vectors/ann.py", "checkout", "copy", "src / 'ann_benchmarks/algorithms' / override.parent.name / override.name"),
    ("benches/breadth/vectors/ann.py", "checkout", "write_text", "dockerfile"),
    ("benches/breadth/vectors/ann.py", "setup", "copy", "src / 'ann_benchmarks/algorithms/nrese/nrese-server'"),
    ("benches/breadth/vectors/ann.py", "images", "copy", "src / 'ann_benchmarks/algorithms/nrese/nrese-server'"),
    # Fast competitor query, expected-answer and taxonomy inputs.
    ("benches/fast/compete.py", "printed_dir", "write_text", "target / f'{name}.rq'"),
    ("benches/fast/compete.py", "suite_run", "write_text", "known"),
    ("benches/fast/compete.py", "nrese_taxonomy", "write_text", "signature_file"),
    ("benches/fast/compete.py", "merged_queries", "write_text", "directory / f\"{case['name']}--{query.name}\""),
    # Fast query and taxonomy intermediates used as inputs to the next step.
    ("benches/fast/fast.py", "vector_queries", "write_text", "directory / f'knn-{q}.rq'"),
    ("benches/fast/fast.py", "parse_queries", "write_text", "directory / f'{name}.rq'"),
    ("benches/fast/fast.py", "cache_log", "write_text", "directory / f'{i:05d}.rq'"),
    ("benches/fast/fast.py", "measure", "write_text", "SCRATCH / f'{name}.tsv'"),
    ("benches/fast/fast.py", "same_taxonomy", "write_text", "SCRATCH / f\"{case['name']}.reference.tax\""),
    # Content-addressed links to generated datasets.
    ("benches/fast/fast.py", "adopt", "link", "target"),
    ("benches/fast/fast.py", "share", "link", "blob"),
    ("benches/fast/fast.py", "share", "link", "linked"),
    ("benches/fast/fast.py", "share", "replace", "path"),
    # Fresh compiled binaries for a gate block.
    ("benches/fast/fast.py", "fresh_copies", "copytree", "target"),
    # Standalone support-stress fixture data and expected answers.
    ("benches/fast/prepare-support-stress.py", "main", "replace", "kept"),
    ("benches/fast/prepare-support-stress.py", "main", "write_text", "answers_path"),
    # The attempt descriptor was opened exclusively at its checked destination above.
    ("benches/fast/release_gate.py", "attempt_record", "fdopen", "fd"),
    # A local W3C check marks the log directory it owns; the gate refuses that directory, so it is never evidence.
    ("benches/fast/release_gate.py", "main", "write_text", "args.logs / LOCAL_MARK"),
    # Generated datasets and archived queries in staged input directories.
    ("benches/fast/release_gate.py", "make_dataset", "replace", "target"),
    ("benches/fast/release_gate.py", "extract_query_set", "extractall", "aside"),
    ("benches/fast/release_gate.py", "extract_query_set", "rename", "target"),
    # Datalog downloads, extraction, generation and input-file hashes.
    ("benches/reasoning/datalog/datalog.py", "fetch", "rename", "path"),
    ("benches/reasoning/datalog/datalog.py", "prepare", "extractall", "staging"),
    ("benches/reasoning/datalog/datalog.py", "prepare", "rename", "target"),
    ("benches/reasoning/datalog/datalog.py", "prepare", "open", "partial"),
    ("benches/reasoning/datalog/datalog.py", "prepare", "rename", "facts"),
    ("benches/reasoning/datalog/datalog.py", "prepare", "write_text", "target.parent / f\"{case['data']}.files.json\""),
    ("benches/reasoning/datalog/datalog.py", "prepare_oxford", "extract", "staging"),
    ("benches/reasoning/datalog/datalog.py", "prepare_oxford", "open", "staging / 'facts.nt.partial'"),
    ("benches/reasoning/datalog/datalog.py", "prepare_oxford", "rename", "target / 'facts.nt'"),
    ("benches/reasoning/datalog/datalog.py", "prepare_lubm", "open", "staging / 'facts.nt'"),
    ("benches/reasoning/datalog/datalog.py", "prepare_lubm", "rename", "target / 'facts.nt'"),
    ("benches/reasoning/datalog/datalog.py", "prepare_triples", "open", "table.with_suffix('.tsv.partial')"),
    ("benches/reasoning/datalog/datalog.py", "prepare_triples", "rename", "table"),
    ("benches/reasoning/datalog/datalog.py", "prepare_triples", "rename", "trident"),
    # Datalog engine rules and configuration in the per-run work directory.
    ("benches/reasoning/datalog/datalog.py", "Run.run_nrese", "write_text", "self.work / 'program.n3'"),
    ("benches/reasoning/datalog/datalog.py", "Run.run_souffle", "write_text", "self.work / 'program.dl'"),
    ("benches/reasoning/datalog/datalog.py", "Run.run_nemo", "write_text", "self.work / 'program.rls'"),
    ("benches/reasoning/datalog/datalog.py", "Run.run_karma", "write_text", "self.work / 'program.dlog'"),
    ("benches/reasoning/datalog/datalog.py", "Run.run_karma", "write_text", "self.work / 'edb.conf'"),
    # Datalog translation outputs rules or a translation error from the source program.
    ("benches/reasoning/datalog/datalog.py", "cmd_translate", "write_text", "out / 'program.dl'"),
    ("benches/reasoning/datalog/datalog.py", "cmd_translate", "write_text", "out / 'program.rls'"),
    ("benches/reasoning/datalog/datalog.py", "cmd_translate", "write_text", "out / name"),
    ("benches/reasoning/datalog/datalog.py", "cmd_translate", "write_text", "out / (name + '.not-expressible')"),
    # Datalog maintenance sample inputs and rules.
    ("benches/reasoning/datalog/maintain.py", "samples", "write_text", "cache"),
    ("benches/reasoning/datalog/maintain.py", "run_case", "write_text", "rules / 'program.n3'"),
    # Maintenance update data, sample and input receipt.
    ("benches/reasoning/maintenance/maintenance.py", "prepare", "write_text", "new"),
    ("benches/reasoning/maintenance/maintenance.py", "prepare", "write_text", "sample"),
    ("benches/reasoning/maintenance/maintenance.py", "prepare", "write_text", "out / 'changes.json'"),
    # SPB configuration, copied inputs and generated-input index.
    ("benches/reasoning/spb/spb.py", "properties", "write_text", "path"),
    ("benches/reasoning/spb/spb.py", "generate_now", "copy", "work / 'definitions.properties'"),
    ("benches/reasoning/spb/spb.py", "generate_now", "write_text", "out / 'generated' / 'files.json'"),
    ("benches/reasoning/spb/spb.py", "one", "copy", "work / 'definitions.properties'"),
    ("benches/reasoning/spb/spb.py", "one", "copy", "work / 'generated' / info.name"),
    # Suite runner configuration, programs and closure input in per-cell work directories.
    ("benches/suite/suitekit/adapters.py", "Jena.load", "write_text", "ctx.work / 'fuseki.ttl'"),
    ("benches/suite/suitekit/adapters.py", "Jena.serve", "write_text", "ctx.work / 'fuseki.ttl'"),
    ("benches/suite/suitekit/adapters.py", "Graphdb.load", "write_text", "path"),
    ("benches/suite/suitekit/adapters.py", "Rdfox.load", "write_text", "ctx.work / 'load.rdfox'"),
    ("benches/suite/suitekit/adapters.py", "ClosureAdapter.serve", "open", "ctx.work / 'closure.rdf.nt'"),
    # Public dataset catalogue from source URLs and hashes.
    ("benches/suite/suitekit/datasets.py", "save", "write_text", "root / 'benches/datasets.toml'"),
    # Empty dataset fixture and extracted source files in newly claimed scratch.
    ("benches/suite/suitekit/driver.py", "Suite.cycle", "write_bytes", "ctx.work / 'empty.nt'"),
    ("benches/suite/suitekit/driver.py", "export_commit", "open", "target"),
    # Kernel lock file without run evidence.
    ("benches/suite/suitekit/lock.py", "lock", "open", "str(path)"),
    # Published standings and status from public ledger records.
    ("benches/suite/suitekit/standings.py", "main", "write_text", "path"),
    ("benches/suite/suitekit/status.py", "main", "write_text", "root / 'benches/STATUS.md'"),
    # Verify binary cache and its eviction timestamp.
    ("scripts/verify/verify.py", "_target_of", "copy", "binary"),
    ("scripts/verify/verify.py", "_target_of", "write_text", "entry / '.last-used'"),
}

STARTS = {
    # Fixed standalone support-stress generator.
    ("benches/fast/prepare-support-stress.py", "main", "subprocess.run", "[perf_lab, 'generate', 'support-stress', str(made), *command[4:]]"),
    # Git wrappers read stored objects, refs and tree bytes.
    ("benches/fast/release_gate.py", "checkout_receipt.git", "subprocess.run", "['git', *args]"),
    ("benches/fast/release_gate.py", "measured_at.git", "subprocess.run", "['git', '-C', repo, *args]"),
    ("benches/fast/release_gate.py", "git_line", "subprocess.run", "['git', '-C', checkout, *args]"),
    ("benches/suite/abba.py", "git", "subprocess.run", "['git', *args]"),
    ("benches/suite/suitekit/cases.py", "load_at", "subprocess.run", "['git', '-C', str(repo), *a]"),
    ("benches/suite/suitekit/checkout.py", "git", "subprocess.run", "['git', '-C', checkout, *args]"),
    ("benches/suite/suitekit/hosts.py", "git", "subprocess.run", "['git', '-C', str(where), *args]"),
    ("scripts/verify/verifykit/jobs.py", "git", "subprocess.run", "['git', '-C', str(repo), *args]"),
    # Windows process-tree cleanup by PID.
    ("benches/fast/release_gate.py", "stop_tree", "subprocess.run", "['taskkill', '/T', '/F', '/PID', str(process.pid)]"),
    ("benches/suite/suitekit/processes.py", "stop", "subprocess.run", "['taskkill', '/T', '/F', '/PID', str(process.pid)]"),
    # Fixed Docker commands convert or generate input data.
    ("benches/reasoning/datalog/datalog.py", "prepare_oxford", "subprocess.run", "['docker', 'run', '--rm', '-u', f'{os.getuid()}:{os.getgid()}', '-v', f'{staging}:/in:ro', '--entrypoint', '/opt/apache-jena-6.2.0/bin/riot', 'nrese-bench/jena:6.2.0', '--quiet', '--output=nt', f'/in/{turtle}']"),
    ("benches/reasoning/maintenance/maintenance.py", "prepare", "subprocess.run", "['docker', 'run', '--rm', '-u', f'{os.getuid()}:{os.getgid()}', '-v', f'{out}:/out', '-w', '/out', 'nrese-bench/lubm-uba', '-u', str(n + 1), '-f', 'NTRIPLES', '--consolidate', 'Maximal', '-t', '8', '-q', '-o', '/out/gen']"),
    # Git tree and blob streams for a new scratch checkout.
    ("benches/suite/suitekit/driver.py", "export_commit", "subprocess.run", "[*git, 'ls-tree', '-r', '-z', '--full-tree', commit]"),
    ("benches/suite/suitekit/driver.py", "export_commit", "subprocess.Popen", "[*git, 'cat-file', '--batch']"),
    # Compiler version probes with no benchmark inputs.
    ("benches/suite/suitekit/driver.py", "toolchain_here", "subprocess.run", "[exe, '-vV']"),
    ("benches/suite/suitekit/driver.py", "toolchain_in", "subprocess.run", "[ns.executable(), *ns.run_args(ROOT, 'suite'), '--rm', '-v', f'{tree.as_posix()}:/src:ro', '-w', '/src', image, 'sh', '-c', 'rustc -vV && cargo -vV']"),
    # The standalone lock trampoline forwards its environment unchanged.
    ("benches/suite/suitekit/lock.py", "main", "subprocess.call", "argv[2:]"),
    # Fixed Git, Docker metadata and memory probes at the manifest helper's callers.
    ("benches/suite/suitekit/manifest.py", "_out", "subprocess.run", "command"),
    # The verify server binary runs in an explicitly isolated scenario environment.
    ("scripts/verify/verifykit/server.py", "Server.start", "subprocess.Popen", "self.argv()"),
}
