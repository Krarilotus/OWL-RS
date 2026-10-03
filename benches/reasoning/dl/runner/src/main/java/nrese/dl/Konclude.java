package nrese.dl;

import java.io.FileOutputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.concurrent.TimeUnit;

import org.semanticweb.owlapi.formats.FunctionalSyntaxDocumentFormat;
import org.semanticweb.owlapi.model.AxiomType;
import org.semanticweb.owlapi.model.OWLAxiom;
import org.semanticweb.owlapi.model.OWLClass;
import org.semanticweb.owlapi.model.OWLClassAssertionAxiom;
import org.semanticweb.owlapi.model.OWLNamedIndividual;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.parameters.Imports;
import org.semanticweb.owlapi.reasoner.Node;
import org.semanticweb.owlapi.reasoner.OWLReasoner;
import org.semanticweb.owlapi.reasoner.structural.StructuralReasonerFactory;

/**
 * KoncludeCLI run directly, as a process of its own (docs/design/owl2-dl.md §11: not
 * through OWLLink): the premise in functional syntax, a hard kill at the timeout.
 * Classification reads Konclude's hierarchy back as told axioms and takes the canonical
 * taxonomy through OWL API's structural reasoner, over the premise's classes. Realisation
 * runs Konclude's classification and realisation (two processes, each with the timeout)
 * and reduces the class assertions it returns to direct types over that hierarchy.
 * Entailment isn't offered by the command line: `unsupported`.
 */
final class Konclude {
    static final String BINARY = System.getenv().getOrDefault("KONCLUDE", "/opt/konclude/Binaries/Konclude");

    private Konclude() {}

    /** A Konclude process's outcome: an answer (status, detail), or its output file. */
    private record Outcome(String[] answer, Path output) {}

    static String[] run(String id, String task, OWLOntology o, Path dir, long timeout) throws Exception {
        if (!task.equals("consistency") && !task.equals("classify") && !task.equals("realise")) {
            return new String[] {"unsupported", "Konclude's command line has no " + task};
        }
        Path work = Files.createTempDirectory("konclude");
        try {
            Path input = work.resolve("input.ofn");
            try (OutputStream out = new FileOutputStream(input.toFile())) {
                o.getOWLOntologyManager().saveOntology(o, new FunctionalSyntaxDocumentFormat(), out);
            }
            if (task.equals("consistency")) {
                return process(work, "consistency", input, timeout).answer();
            }
            Outcome classified = process(work, "classification", input, timeout);
            if (classified.answer() != null) {
                return classified.answer();
            }
            OWLOntology hierarchy = hierarchy(o, classified.output());
            var structural = new StructuralReasonerFactory().createReasoner(hierarchy);
            try {
                if (task.equals("classify")) {
                    return Runner.written(dir, id, "konclude", Runner.taxonomy(hierarchy, structural));
                }
                Outcome realised = process(work, "realization", input, timeout);
                if (realised.answer() != null) {
                    return realised.answer();
                }
                OWLOntology assertions = Runner.loadFile(realised.output());
                return Runner.realised(dir, id, "konclude", realisation(o, assertions, structural));
            } finally {
                structural.dispose();
            }
        } finally {
            try (var files = Files.walk(work)) {
                files.sorted(java.util.Comparator.reverseOrder()).forEach(f -> f.toFile().delete());
            }
        }
    }

    /** One Konclude command; an answer when it ends without an output to read. */
    private static Outcome process(Path work, String command, Path input, long timeout) throws Exception {
        Path output = work.resolve(command + ".ofn");
        Path logFile = work.resolve(command + ".log");
        List<String> line = command.equals("consistency")
            ? List.of(BINARY, "consistency", "-i", input.toString())
            : List.of(BINARY, command, "-i", input.toString(), "-o", output.toString());
        Process p = new ProcessBuilder(line).redirectErrorStream(true).redirectOutput(logFile.toFile()).start();
        if (!p.waitFor(timeout, TimeUnit.SECONDS)) {
            p.destroyForcibly().waitFor();
            return new Outcome(new String[] {"timeout", ""}, null);
        }
        String log = Files.readString(logFile, StandardCharsets.UTF_8);
        // Konclude reports an ontology it couldn't read (even a missing file) as
        // consistent, with an {error} line: such a run is no answer.
        String errors = log.lines().filter(l -> l.startsWith("{error}"))
            .reduce((a, b) -> a + " | " + b).orElse("");
        if (!errors.isEmpty()) {
            return new Outcome(new String[] {"error", errors}, null);
        }
        String lower = log.toLowerCase();
        if (command.equals("consistency")) {
            if (lower.contains("is inconsistent")) {
                return new Outcome(new String[] {"inconsistent", ""}, null);
            }
            if (lower.contains("is consistent")) {
                return new Outcome(new String[] {"consistent", ""}, null);
            }
            return new Outcome(new String[] {"error", "exit " + p.exitValue() + ": " + tail(log)}, null);
        }
        if (lower.contains("is inconsistent") || lower.contains("inconsistent ontology")) {
            return new Outcome(new String[] {"inconsistent", ""}, null);
        }
        if (!Files.exists(output)) {
            return new Outcome(new String[] {"error", "exit " + p.exitValue() + ": " + tail(log)}, null);
        }
        return new Outcome(null, output);
    }

    /** Konclude's hierarchy as told axioms, over the premise's classes. */
    private static OWLOntology hierarchy(OWLOntology o, Path output) throws Exception {
        OWLOntology hierarchy = Runner.loadFile(output);
        // The premise's classes, so that classes only under owl:Thing are there too.
        for (OWLClass c : o.getClassesInSignature(Imports.INCLUDED)) {
            OWLAxiom d = hierarchy.getOWLOntologyManager().getOWLDataFactory().getOWLDeclarationAxiom(c);
            hierarchy.getOWLOntologyManager().addAxiom(hierarchy, d);
        }
        for (OWLAxiom a : hierarchy.getAxioms()) {
            if (!a.isOfType(AxiomType.SUBCLASS_OF, AxiomType.EQUIVALENT_CLASSES, AxiomType.DECLARATION)) {
                hierarchy.getOWLOntologyManager().removeAxiom(hierarchy, a);
            }
        }
        return hierarchy;
    }

    /**
     * The canonical realisation from Konclude's class assertions: per named individual of the
     * premise, its asserted types as representatives, without any type that is a strict
     * superclass of another (the direct types); owl:Thing when none is left.
     */
    private static String realisation(OWLOntology o, OWLOntology assertions, OWLReasoner hierarchy) {
        Map<String, List<Node<OWLClass>>> types = new TreeMap<>();
        for (OWLClassAssertionAxiom a : assertions.getAxioms(AxiomType.CLASS_ASSERTION)) {
            if (a.getIndividual().isNamed() && !a.getClassExpression().isAnonymous()) {
                OWLClass c = a.getClassExpression().asOWLClass();
                types.computeIfAbsent(a.getIndividual().asOWLNamedIndividual().getIRI().toString(),
                    k -> new ArrayList<>()).add(hierarchy.getEquivalentClasses(c));
            }
        }
        TreeSet<String> lines = new TreeSet<>();
        for (OWLNamedIndividual i : o.getIndividualsInSignature(Imports.INCLUDED)) {
            String iri = i.getIRI().toString();
            List<Node<OWLClass>> nodes = types.getOrDefault(iri, List.of());
            Set<String> direct = new TreeSet<>();
            for (Node<OWLClass> n : nodes) {
                if (n.isTopNode()) {
                    continue;
                }
                boolean subsumed = false;
                for (Node<OWLClass> m : nodes) {
                    if (m != n && !m.equals(n) && hierarchy.getSuperClasses(m.getRepresentativeElement(), false)
                            .containsEntity(n.getRepresentativeElement())) {
                        subsumed = true;
                        break;
                    }
                }
                if (!subsumed) {
                    direct.add(Runner.representative(n));
                }
            }
            if (direct.isEmpty()) {
                direct.add(Runner.representative(hierarchy.getTopClassNode()));
            }
            for (String t : direct) {
                lines.add("a " + iri + " " + t);
            }
        }
        return lines.isEmpty() ? "" : String.join("\n", lines) + "\n";
    }

    private static String tail(String log) {
        String t = log.strip();
        return t.length() > 200 ? t.substring(t.length() - 200) : t;
    }
}
