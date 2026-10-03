package nrese.dl;

import java.io.FileOutputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import org.semanticweb.owlapi.formats.FunctionalSyntaxDocumentFormat;
import org.semanticweb.owlapi.model.AxiomType;
import org.semanticweb.owlapi.model.OWLEquivalentClassesAxiom;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.OWLSubClassOfAxiom;
import org.semanticweb.owlapi.reasoner.structural.StructuralReasonerFactory;

/**
 * ELK 0.4.3, through its standalone command line as a process of its own: it is built on
 * OWL API 3, which can't share the runner's JVM with OWL API 5. The Whelk paper (Balhoff et
 * al., TGDK 2024) measured it far faster than 0.6.0 on large ontologies, so every EL claim
 * is made against the faster of the two (docs/design/owl2-dl-performance.md §1).
 *
 * <p>The premise goes in as functional syntax; {@code -c -o} writes the taxonomy, which is
 * read back as told axioms and canonicalised like Konclude's. Classification only: the
 * command line doesn't print its consistency verdict. An inconsistent ontology comes back
 * as {@code owl:Thing} equivalent to {@code owl:Nothing}, reported {@code inconsistent}.
 * The detail column carries ELK's own stage times (loading, taxonomy), since the task's
 * time includes starting a JVM. {@code ELK043_WORKERS} sets its worker threads (default:
 * all cores, as ELK chooses).
 */
final class Elk043 {
    static final String JAR = System.getenv().getOrDefault("ELK043", "/opt/elk-0.4.3/elk-standalone-0.4.3.jar");
    static final String WORKERS = System.getenv().getOrDefault("ELK043_WORKERS", "");
    private static final Pattern STAGE = Pattern.compile(
        "- (Loading of Axioms|Class Taxonomy Computation|Consistency Checking) took (\\d+) ms");

    private Elk043() {}

    static String[] run(String id, String task, OWLOntology o, Path dir, long timeout) throws Exception {
        if (!task.equals("classify")) {
            return new String[] {"unsupported", "ELK 0.4.3's command line: classification only here"};
        }
        Path work = Files.createTempDirectory("elk043");
        try {
            Path input = work.resolve("input.ofn");
            try (OutputStream out = new FileOutputStream(input.toFile())) {
                o.getOWLOntologyManager().saveOntology(o, new FunctionalSyntaxDocumentFormat(), out);
            }
            Path output = work.resolve("taxonomy.ofn");
            Path logFile = work.resolve("elk.log");
            List<String> line = new ArrayList<>(List.of("java", "-Xmx8g", "-jar", JAR,
                "-i", input.toString(), "-c", "-o", output.toString()));
            if (!WORKERS.isEmpty()) {
                line.addAll(List.of("-w", WORKERS));
            }
            Process p = new ProcessBuilder(line).redirectErrorStream(true).redirectOutput(logFile.toFile()).start();
            if (!p.waitFor(timeout, TimeUnit.SECONDS)) {
                p.destroyForcibly().waitFor();
                return new String[] {"timeout", ""};
            }
            String log = Files.readString(logFile, StandardCharsets.UTF_8);
            if (p.exitValue() != 0 || !Files.exists(output)) {
                String t = log.strip();
                return new String[] {"error", "exit " + p.exitValue() + ": "
                    + (t.length() > 200 ? t.substring(t.length() - 200) : t)};
            }
            OWLOntology hierarchy = Konclude.hierarchy(o, output);
            if (inconsistent(hierarchy)) {
                return new String[] {"inconsistent", stages(log)};
            }
            var structural = new StructuralReasonerFactory().createReasoner(hierarchy);
            try {
                String[] answer = Runner.written(dir, id, "elk-0.4.3", Runner.taxonomy(hierarchy, structural));
                // The hash stays first in the detail: comparisons read it.
                return new String[] {answer[0], answer[1] + " " + stages(log)};
            } finally {
                structural.dispose();
            }
        } finally {
            try (var files = Files.walk(work)) {
                files.sorted(java.util.Comparator.reverseOrder()).forEach(f -> f.toFile().delete());
            }
        }
    }

    /** Whether the taxonomy makes owl:Thing equal to (or a subclass of) owl:Nothing. */
    private static boolean inconsistent(OWLOntology hierarchy) {
        for (OWLEquivalentClassesAxiom a : hierarchy.getAxioms(AxiomType.EQUIVALENT_CLASSES)) {
            if (a.containsOWLThing() && a.containsOWLNothing()) {
                return true;
            }
        }
        for (OWLSubClassOfAxiom a : hierarchy.getAxioms(AxiomType.SUBCLASS_OF)) {
            if (a.getSubClass().isOWLThing() && a.getSuperClass().isOWLNothing()) {
                return true;
            }
        }
        return false;
    }

    /** ELK's own stage times from its log: {@code load=…ms taxonomy=…ms}. */
    private static String stages(String log) {
        StringBuilder out = new StringBuilder();
        Matcher m = STAGE.matcher(log);
        while (m.find()) {
            String name = switch (m.group(1)) {
                case "Loading of Axioms" -> "load";
                case "Class Taxonomy Computation" -> "taxonomy";
                default -> "consistency";
            };
            out.append(out.length() == 0 ? "" : " ").append(name).append('=').append(m.group(2)).append("ms");
        }
        return out.toString();
    }
}
