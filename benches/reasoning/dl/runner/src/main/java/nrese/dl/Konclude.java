package nrese.dl;

import java.io.FileOutputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.concurrent.TimeUnit;

import org.semanticweb.owlapi.formats.FunctionalSyntaxDocumentFormat;
import org.semanticweb.owlapi.model.AxiomType;
import org.semanticweb.owlapi.model.OWLAxiom;
import org.semanticweb.owlapi.model.OWLClass;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.parameters.Imports;
import org.semanticweb.owlapi.reasoner.structural.StructuralReasonerFactory;

/**
 * KoncludeCLI run directly, as a process of its own (docs/design/owl2-dl.md §11: not
 * through OWLLink): the premise in functional syntax, a hard kill at the timeout.
 * Classification reads Konclude's hierarchy back as told axioms and takes the canonical
 * taxonomy through OWL API's structural reasoner, over the premise's classes. Entailment
 * isn't offered by the command line: `unsupported`.
 */
final class Konclude {
    static final String BINARY = System.getenv().getOrDefault("KONCLUDE", "/opt/konclude/Binaries/Konclude");

    private Konclude() {}

    static String[] run(String id, String task, OWLOntology o, Path dir, long timeout) throws Exception {
        if (!task.equals("consistency") && !task.equals("classify")) {
            return new String[] {"unsupported", "Konclude's command line has no " + task};
        }
        Path work = Files.createTempDirectory("konclude");
        try {
            Path input = work.resolve("input.ofn");
            try (OutputStream out = new FileOutputStream(input.toFile())) {
                o.getOWLOntologyManager().saveOntology(o, new FunctionalSyntaxDocumentFormat(), out);
            }
            Path output = work.resolve("output.ofn");
            List<String> command = task.equals("consistency")
                ? List.of(BINARY, "consistency", "-i", input.toString())
                : List.of(BINARY, "classification", "-i", input.toString(), "-o", output.toString());
            Process p = new ProcessBuilder(command).redirectErrorStream(true)
                .redirectOutput(work.resolve("log.txt").toFile()).start();
            if (!p.waitFor(timeout, TimeUnit.SECONDS)) {
                p.destroyForcibly().waitFor();
                return new String[] {"timeout", ""};
            }
            String log = Files.readString(work.resolve("log.txt"), StandardCharsets.UTF_8);
            // Konclude reports an ontology it couldn't read (even a missing file) as
            // consistent, with an {error} line: such a run is no answer.
            String errors = log.lines().filter(l -> l.startsWith("{error}"))
                .reduce((a, b) -> a + " | " + b).orElse("");
            if (!errors.isEmpty()) {
                return new String[] {"error", errors};
            }
            String lower = log.toLowerCase();
            if (task.equals("consistency")) {
                if (lower.contains("is inconsistent")) {
                    return new String[] {"inconsistent", ""};
                }
                if (lower.contains("is consistent")) {
                    return new String[] {"consistent", ""};
                }
                return new String[] {"error", "exit " + p.exitValue() + ": " + tail(log)};
            }
            if (lower.contains("is inconsistent") || lower.contains("inconsistent ontology")) {
                return new String[] {"inconsistent", ""};
            }
            if (!Files.exists(output)) {
                return new String[] {"error", "exit " + p.exitValue() + ": " + tail(log)};
            }
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
            var structural = new StructuralReasonerFactory().createReasoner(hierarchy);
            try {
                return Runner.written(dir, id, "konclude", Runner.taxonomy(hierarchy, structural));
            } finally {
                structural.dispose();
            }
        } finally {
            try (var files = Files.walk(work)) {
                files.sorted(java.util.Comparator.reverseOrder()).forEach(f -> f.toFile().delete());
            }
        }
    }

    private static String tail(String log) {
        String t = log.strip();
        return t.length() > 200 ? t.substring(t.length() - 200) : t;
    }
}
