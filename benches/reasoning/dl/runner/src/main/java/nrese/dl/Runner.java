package nrese.dl;

import java.io.BufferedWriter;
import java.io.File;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Set;
import java.util.TreeSet;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicReference;

import org.semanticweb.owlapi.apibinding.OWLManager;
import org.semanticweb.owlapi.model.MissingImportHandlingStrategy;
import org.semanticweb.owlapi.model.OWLAxiom;
import org.semanticweb.owlapi.model.OWLClass;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.OWLOntologyLoaderConfiguration;
import org.semanticweb.owlapi.model.OWLOntologyManager;
import org.semanticweb.owlapi.model.parameters.Imports;
import org.semanticweb.owlapi.reasoner.InferenceType;
import org.semanticweb.owlapi.reasoner.Node;
import org.semanticweb.owlapi.reasoner.OWLReasoner;
import org.semanticweb.owlapi.reasoner.OWLReasonerFactory;
import org.semanticweb.owlapi.reasoner.SimpleConfiguration;

/**
 * NRESE's runner for the reference reasoners (docs/design/owl2-dl.md §11): one JVM runs a
 * batch of tasks, each with its own timeout, so that start-up is paid once.
 *
 * <pre>
 * java -jar dl-runner.jar batch MANIFEST OUT_TSV OUT_DIR [TIMEOUT_SECONDS]
 * </pre>
 *
 * A manifest line is {@code id<TAB>reasoner<TAB>task<TAB>premise[<TAB>conclusion]}:
 * reasoner {@code hermit}, {@code elk}, {@code openllet} or {@code konclude} (a process of
 * its own, see {@link Konclude}); task {@code consistency},
 * {@code entailment} (premise entails every logical axiom of the conclusion) or
 * {@code classify}; or {@code ntriples}, any reasoner: the premise converted to
 * {@code OUT_DIR/id.nt} for systems that read RDF. A result line is
 * {@code id<TAB>reasoner<TAB>task<TAB>status<TAB>millis<TAB>detail}, status one of
 * {@code consistent}, {@code inconsistent}, {@code entailed}, {@code not-entailed},
 * {@code classified} (detail: the taxonomy's SHA-256), {@code unsupported},
 * {@code timeout}, {@code parse-error}, {@code error}.
 *
 * <p>The canonical taxonomy ({@code OUT_DIR/id.reasoner.tax}) has one line per class,
 * {@code = rep member}, the representative being the smallest IRI of its equivalence
 * class ({@code owl:Nothing} for unsatisfiable classes, {@code owl:Thing} for those
 * equivalent to it), and one per direct subsumption between representatives,
 * {@code < sub super}; sorted, so that equal taxonomies have equal hashes.
 */
public final class Runner {
    private static final String THING = "http://www.w3.org/2002/07/owl#Thing";
    private static final String NOTHING = "http://www.w3.org/2002/07/owl#Nothing";

    public static void main(String[] args) throws Exception {
        if (args.length < 4 || !args[0].equals("batch")) {
            System.err.println("usage: batch MANIFEST OUT_TSV OUT_DIR [TIMEOUT_SECONDS]");
            System.exit(2);
        }
        Path manifest = Path.of(args[1]);
        Path out = Path.of(args[2]);
        Path dir = Path.of(args[3]);
        long timeout = args.length > 4 ? Long.parseLong(args[4]) : 60;
        Files.createDirectories(dir);
        ExecutorService pool = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r);
            t.setDaemon(true);
            return t;
        });
        try (BufferedWriter w = Files.newBufferedWriter(out, StandardCharsets.UTF_8)) {
            for (String line : Files.readAllLines(manifest, StandardCharsets.UTF_8)) {
                if (line.isBlank() || line.startsWith("#")) {
                    continue;
                }
                String[] f = line.split("\t");
                String id = f[0], reasoner = f[1], task = f[2];
                String premise = f[3], conclusion = f.length > 4 ? f[4] : null;
                AtomicReference<OWLReasoner> running = new AtomicReference<>();
                long start = System.nanoTime();
                String[] result;
                Future<String[]> future = pool.submit(
                    () -> run(id, reasoner, task, premise, conclusion, dir, timeout, running));
                try {
                    result = future.get(timeout, TimeUnit.SECONDS);
                } catch (TimeoutException e) {
                    OWLReasoner r = running.get();
                    if (r != null) {
                        try {
                            r.interrupt();
                        } catch (RuntimeException ignored) {
                            // Some reasoners can't be interrupted; the thread is a daemon.
                        }
                    }
                    future.cancel(true);
                    result = new String[] {"timeout", ""};
                } catch (java.util.concurrent.ExecutionException e) {
                    result = failure(e.getCause());
                }
                long ms = (System.nanoTime() - start) / 1_000_000;
                w.write(String.join("\t", id, reasoner, task, result[0], Long.toString(ms),
                    clean(result[1])));
                w.newLine();
                w.flush();
                if (result[0].equals("timeout")) {
                    // A reasoner that missed its timeout may still be running: a fresh JVM
                    // for the rest (the driver resumes; exit 3 asks for that).
                    System.exit(3);
                }
            }
        }
        pool.shutdownNow();
        System.exit(0);
    }

    private static String clean(String s) {
        String one = s.replace('\t', ' ').replace('\n', ' ').replace('\r', ' ');
        return one.length() > 300 ? one.substring(0, 300) : one;
    }

    private static String[] failure(Throwable e) {
        String name = e.getClass().getName();
        if (name.contains("Unsupported") || name.contains("UnsupportedEntailment")
            || name.contains("UnsupportedDatatype") || name.contains("OWLReasonerRuntime")
                && String.valueOf(e.getMessage()).toLowerCase().contains("unsupported")) {
            return new String[] {"unsupported", name + ": " + e.getMessage()};
        }
        if (name.contains("OWLOntologyCreation") || name.contains("Parser")) {
            return new String[] {"parse-error", name + ": " + e.getMessage()};
        }
        StringWriter trace = new StringWriter();
        e.printStackTrace(new PrintWriter(trace));
        return new String[] {"error", name + ": " + e.getMessage()};
    }

    private static OWLReasonerFactory factory(String name) {
        return switch (name) {
            case "hermit" -> new org.semanticweb.HermiT.ReasonerFactory();
            case "elk" -> new org.semanticweb.elk.owlapi.ElkReasonerFactory();
            case "openllet" -> openllet.owlapi.OpenlletReasonerFactory.getInstance();
            default -> throw new IllegalArgumentException("unknown reasoner " + name);
        };
    }

    private static OWLOntology load(String path) throws Exception {
        OWLOntologyManager m = OWLManager.createOWLOntologyManager();
        m.setOntologyLoaderConfiguration(new OWLOntologyLoaderConfiguration()
            .setMissingImportHandlingStrategy(MissingImportHandlingStrategy.SILENT));
        return m.loadOntologyFromOntologyDocument(new File(path));
    }

    private static String[] run(String id, String name, String task, String premise,
            String conclusion, Path dir, long timeout, AtomicReference<OWLReasoner> running)
            throws Exception {
        OWLOntology o;
        try {
            o = load(premise);
        } catch (Exception e) {
            return new String[] {"parse-error", e.getClass().getName() + ": " + e.getMessage()};
        }
        if (task.equals("ntriples")) {
            // For systems that read RDF (NRESE): the ontology in N-Triples, OUT_DIR/id.nt.
            try (var out = Files.newOutputStream(dir.resolve(id + ".nt"))) {
                o.getOWLOntologyManager().saveOntology(o,
                    new org.semanticweb.owlapi.formats.NTriplesDocumentFormat(), out);
            }
            return new String[] {"converted", id + ".nt"};
        }
        if (task.equals("functional")) {
            // The ontology in functional syntax, as Konclude gets it: OUT_DIR/id.ofn.
            try (var out = Files.newOutputStream(dir.resolve(id + ".ofn"))) {
                o.getOWLOntologyManager().saveOntology(o,
                    new org.semanticweb.owlapi.formats.FunctionalSyntaxDocumentFormat(), out);
            }
            return new String[] {"converted", id + ".ofn"};
        }
        if (name.equals("konclude")) {
            return Konclude.run(id, task, o, dir, timeout);
        }
        OWLReasoner r = factory(name).createReasoner(o, new SimpleConfiguration(timeout * 1000));
        running.set(r);
        try {
            switch (task) {
                case "consistency":
                    return new String[] {r.isConsistent() ? "consistent" : "inconsistent", ""};
                case "entailment": {
                    OWLOntology c;
                    try {
                        c = load(conclusion);
                    } catch (Exception e) {
                        return new String[] {"parse-error",
                            "conclusion: " + e.getClass().getName() + ": " + e.getMessage()};
                    }
                    if (!r.isConsistent()) {
                        return new String[] {"entailed", "the premise is inconsistent"};
                    }
                    Set<OWLAxiom> axioms = new java.util.HashSet<>(c.getLogicalAxioms());
                    if (axioms.isEmpty()) {
                        return new String[] {"entailed", "no logical axioms"};
                    }
                    for (OWLAxiom a : axioms) {
                        if (!r.isEntailmentCheckingSupported(a.getAxiomType())) {
                            return new String[] {"unsupported", "entailment of " + a.getAxiomType()};
                        }
                    }
                    return new String[] {r.isEntailed(axioms) ? "entailed" : "not-entailed", ""};
                }
                case "classify": {
                    if (!r.isConsistent()) {
                        return new String[] {"inconsistent", ""};
                    }
                    r.precomputeInferences(InferenceType.CLASS_HIERARCHY);
                    return written(dir, id, name, taxonomy(o, r));
                }
                default:
                    throw new IllegalArgumentException("unknown task " + task);
            }
        } finally {
            r.dispose();
        }
    }

    /** Writes a taxonomy and answers with its hash. */
    static String[] written(Path dir, String id, String name, String tax) throws Exception {
        Files.writeString(dir.resolve(id + "." + name + ".tax"), tax, StandardCharsets.UTF_8);
        MessageDigest sha = MessageDigest.getInstance("SHA-256");
        return new String[] {"classified",
            HexFormat.of().formatHex(sha.digest(tax.getBytes(StandardCharsets.UTF_8)))};
    }

    static OWLOntology loadFile(Path path) throws Exception {
        return load(path.toString());
    }

    private static String representative(Node<OWLClass> node) {
        if (node.isBottomNode()) {
            return NOTHING;
        }
        if (node.isTopNode()) {
            return THING;
        }
        String best = null;
        for (OWLClass c : node.getEntities()) {
            String iri = c.getIRI().toString();
            if (best == null || iri.compareTo(best) < 0) {
                best = iri;
            }
        }
        return best;
    }

    static String taxonomy(OWLOntology o, OWLReasoner r) {
        TreeSet<String> lines = new TreeSet<>();
        List<OWLClass> classes = new ArrayList<>(o.getClassesInSignature(Imports.INCLUDED));
        classes.add(o.getOWLOntologyManager().getOWLDataFactory().getOWLThing());
        classes.add(o.getOWLOntologyManager().getOWLDataFactory().getOWLNothing());
        Set<String> done = new java.util.HashSet<>();
        for (OWLClass c : classes) {
            Node<OWLClass> node = r.getEquivalentClasses(c);
            String rep = representative(node);
            lines.add("= " + rep + " " + c.getIRI());
            if (!done.add(rep) || node.isBottomNode()) {
                continue;
            }
            for (Node<OWLClass> sup : r.getSuperClasses(c, true)) {
                lines.add("< " + rep + " " + representative(sup));
            }
        }
        return String.join("\n", lines) + "\n";
    }
}
