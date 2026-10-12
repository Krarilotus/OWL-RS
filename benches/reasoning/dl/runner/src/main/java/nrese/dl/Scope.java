package nrese.dl;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicReference;

import org.semanticweb.owlapi.apibinding.OWLManager;
import org.semanticweb.owlapi.io.StringDocumentSource;
import org.semanticweb.owlapi.model.IRI;
import org.semanticweb.owlapi.model.OWLClass;
import org.semanticweb.owlapi.model.OWLDataFactory;
import org.semanticweb.owlapi.model.OWLNamedIndividual;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.OWLOntologyManager;
import org.semanticweb.owlapi.reasoner.OWLReasoner;
import org.semanticweb.owlapi.reasoner.OWLReasonerFactory;
import org.semanticweb.owlapi.reasoner.SimpleConfiguration;

/**
 * The reference answers for the small-scope cases ({@code nrese_owl::scope}, docs/design/owl2-dl.md
 * §11): one JVM answers many small ontologies, one functional-syntax document per line.
 *
 * <pre>
 * java -cp dl-runner.jar nrese.dl.Scope CASES OUT_TSV REASONERS TIMEOUT_SECONDS [PART PARTS]
 * </pre>
 *
 * {@code CASES} has lines {@code index<TAB>document}; {@code REASONERS} is a comma-separated list
 * ({@code hermit}, {@code openllet}); {@code PART PARTS}: only the cases whose index is
 * {@code PART} modulo {@code PARTS}. Cases already in {@code OUT_TSV} are skipped (resumable). A
 * result line is {@code index<TAB>reasoner<TAB>code<TAB>millis}, the code {@code i} for an
 * inconsistent ontology, else {@code c} and ten bits ({@code 0}, {@code 1}) for, in order: A
 * satisfiable, B satisfiable, A ⊑ B, B ⊑ A, ⊤ ⊑ A, ⊤ ⊑ B, a : A, a : B, b : A, b : B (the
 * order of {@code nrese_owl::scope::QUESTIONS}); or {@code t} (timeout), {@code u}
 * (unsupported), {@code p} (parse error), {@code e} (error), each with what happened after a
 * space. After a timeout the JVM exits with status 3, since the reasoner may still be running;
 * the driver starts a fresh one.
 */
public final class Scope {
    private static final String NS = "http://example.org/scope#";

    public static void main(String[] args) throws Exception {
        if (args.length < 4) {
            System.err.println("usage: CASES OUT_TSV REASONERS TIMEOUT_SECONDS [PART PARTS]");
            System.exit(2);
        }
        Path cases = Path.of(args[0]);
        Path out = Path.of(args[1]);
        String[] reasoners = args[2].split(",");
        long timeout = Long.parseLong(args[3]);
        long part = args.length > 5 ? Long.parseLong(args[4]) : 0;
        long parts = args.length > 5 ? Long.parseLong(args[5]) : 1;
        Set<String> done = new HashSet<>();
        if (Files.exists(out)) {
            for (String line : Files.readAllLines(out, StandardCharsets.UTF_8)) {
                String[] f = line.split("\t");
                if (f.length >= 3) {
                    done.add(f[0] + "\t" + f[1]);
                }
            }
        }
        ExecutorService pool = Executors.newCachedThreadPool(r -> {
            Thread t = new Thread(r);
            t.setDaemon(true);
            return t;
        });
        try (BufferedReader in = Files.newBufferedReader(cases, StandardCharsets.UTF_8);
             BufferedWriter w = Files.newBufferedWriter(out, StandardCharsets.UTF_8,
                 StandardOpenOption.CREATE, StandardOpenOption.APPEND)) {
            String line;
            while ((line = in.readLine()) != null) {
                int tab = line.indexOf('\t');
                if (tab < 0) {
                    continue;
                }
                String index = line.substring(0, tab);
                if (Long.parseLong(index) % parts != part) {
                    continue;
                }
                String document = line.substring(tab + 1);
                // Parsed once for every reasoner (none changes it).
                OWLOntology ontology = null;
                String parseError = null;
                for (String name : reasoners) {
                    if (done.contains(index + "\t" + name)) {
                        continue;
                    }
                    if (ontology == null && parseError == null) {
                        try {
                            ontology = OWLManager.createOWLOntologyManager()
                                .loadOntologyFromOntologyDocument(new StringDocumentSource(document));
                        } catch (Exception e) {
                            parseError = "p " + e.getClass().getName();
                        }
                    }
                    AtomicReference<OWLReasoner> running = new AtomicReference<>();
                    long start = System.nanoTime();
                    OWLOntology o = ontology;
                    String failed = parseError;
                    Future<String> future = pool.submit(
                        () -> failed != null ? failed : answer(o, name, timeout, running));
                    String code;
                    boolean timedOut = false;
                    try {
                        code = future.get(timeout, TimeUnit.SECONDS);
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
                        code = "t after " + timeout + " s";
                        timedOut = true;
                    } catch (java.util.concurrent.ExecutionException e) {
                        code = failure(e.getCause());
                    }
                    long ms = (System.nanoTime() - start) / 1_000_000;
                    w.write(index + "\t" + name + "\t" + code.replace('\t', ' ').replace('\n', ' ')
                        + "\t" + ms);
                    w.newLine();
                    w.flush();
                    if (timedOut) {
                        System.exit(3);
                    }
                }
            }
        }
        pool.shutdownNow();
        System.exit(0);
    }

    private static String failure(Throwable e) {
        String name = e.getClass().getName();
        String what = name + ": " + e.getMessage();
        if (what.length() > 200) {
            what = what.substring(0, 200);
        }
        if (name.contains("Unsupported") || String.valueOf(e.getMessage()).toLowerCase().contains("unsupported")) {
            return "u " + what;
        }
        if (name.contains("OWLOntologyCreation") || name.contains("Parser")) {
            return "p " + what;
        }
        return "e " + what;
    }

    private static OWLReasonerFactory factory(String name) {
        return switch (name) {
            case "hermit" -> new org.semanticweb.HermiT.ReasonerFactory();
            case "openllet" -> openllet.owlapi.OpenlletReasonerFactory.getInstance();
            default -> throw new IllegalArgumentException("unknown reasoner " + name);
        };
    }

    private static String answer(OWLOntology o, String name, long timeout,
            AtomicReference<OWLReasoner> running) throws Exception {
        OWLOntologyManager m = o.getOWLOntologyManager();
        OWLReasoner r = factory(name).createReasoner(o, new SimpleConfiguration(timeout * 1000));
        running.set(r);
        try {
            if (!r.isConsistent()) {
                return "i";
            }
            OWLDataFactory f = m.getOWLDataFactory();
            OWLClass a = f.getOWLClass(IRI.create(NS + "A"));
            OWLClass b = f.getOWLClass(IRI.create(NS + "B"));
            OWLNamedIndividual ia = f.getOWLNamedIndividual(IRI.create(NS + "a"));
            OWLNamedIndividual ib = f.getOWLNamedIndividual(IRI.create(NS + "b"));
            OWLClass thing = f.getOWLThing();
            List<Boolean> bits = List.of(
                r.isSatisfiable(a),
                r.isSatisfiable(b),
                r.isEntailed(f.getOWLSubClassOfAxiom(a, b)),
                r.isEntailed(f.getOWLSubClassOfAxiom(b, a)),
                r.isEntailed(f.getOWLSubClassOfAxiom(thing, a)),
                r.isEntailed(f.getOWLSubClassOfAxiom(thing, b)),
                r.isEntailed(f.getOWLClassAssertionAxiom(a, ia)),
                r.isEntailed(f.getOWLClassAssertionAxiom(b, ia)),
                r.isEntailed(f.getOWLClassAssertionAxiom(a, ib)),
                r.isEntailed(f.getOWLClassAssertionAxiom(b, ib)));
            StringBuilder code = new StringBuilder("c");
            for (boolean bit : bits) {
                code.append(bit ? '1' : '0');
            }
            return code.toString();
        } finally {
            r.dispose();
        }
    }
}
