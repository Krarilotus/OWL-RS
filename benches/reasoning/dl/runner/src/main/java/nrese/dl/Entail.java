package nrese.dl;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.*;

import org.semanticweb.owlapi.apibinding.OWLManager;
import org.semanticweb.owlapi.formats.TurtleDocumentFormat;
import org.semanticweb.owlapi.io.StringDocumentSource;
import org.semanticweb.owlapi.model.*;
import org.semanticweb.owlapi.reasoner.OWLReasoner;
import org.semanticweb.owlapi.vocab.OWL2Datatype;

/**
 * Entailment questions for {@code crates/nrese-store/tests/it/dl/outside.rs}, a request a
 * line on standard input ({@code java -cp dl-runner.jar nrese.dl.Entail TIMEOUT_SECONDS}):
 * {@code hermit} or {@code openllet}, the ontology as Turtle on one line, and the questions,
 * tab-separated. A question is {@code T C a} (a : C), {@code P p a b}, {@code D d a n} (n an
 * integer), {@code S a b} (a = b), {@code E p C a} (a : ∃p.C), {@code F d a} (a : ∃d.Literal)
 * or {@code U C D} (C ⊑ D). The answer is {@code i} (inconsistent), or {@code c} and a
 * {@code 1} or {@code 0} per question, or {@code u} (unsupported), {@code p} (parse error),
 * {@code e} (error) or {@code t} (timeout) and what happened. After a timeout the JVM halts
 * (status 3), since the reasoner may still run.
 */
public final class Entail {
    public static void main(String[] args) throws Exception {
        long timeout = Long.parseLong(args[0]);
        // Halted, never shut down: a timed-out reasoner may still run.
        ExecutorService pool = Executors.newSingleThreadExecutor();
        PrintStream out = new PrintStream(System.out, true, StandardCharsets.UTF_8);
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = in.readLine()) != null) {
            String[] request = line.split("\t");
            Future<String> answer = pool.submit(() -> answer(request));
            try {
                out.println(answer.get(timeout, TimeUnit.SECONDS));
            } catch (TimeoutException e) {
                out.println("t after " + timeout + " s");
                Runtime.getRuntime().halt(3);
            } catch (ExecutionException e) {
                String[] f = Runner.failure(e.getCause());
                out.println(f[0].charAt(0) + " " + Runner.clean(f[1]));
            }
        }
        Runtime.getRuntime().halt(0);
    }

    private static String answer(String[] request) throws Exception {
        OWLOntologyManager m = OWLManager.createOWLOntologyManager();
        OWLOntology o = m.loadOntologyFromOntologyDocument(new StringDocumentSource(
            request[1], IRI.create("urn:nrese:outside-check"), new TurtleDocumentFormat(), null));
        OWLDataFactory df = m.getOWLDataFactory();
        OWLReasoner r = Runner.factory(request[0]).createReasoner(o);
        try {
            if (!r.isConsistent()) {
                return "i";
            }
            StringBuilder b = new StringBuilder("c");
            for (int k = 2; k < request.length; k++) {
                OWLAxiom a = question(df, request[k].split(" "));
                if (!r.isEntailmentCheckingSupported(a.getAxiomType())) {
                    return "u entailment of " + a.getAxiomType();
                }
                b.append(r.isEntailed(a) ? '1' : '0');
            }
            return b.toString();
        } finally {
            r.dispose();
        }
    }

    private static OWLAxiom question(OWLDataFactory df, String[] t) {
        return switch (t[0]) {
            case "T" -> df.getOWLClassAssertionAxiom(df.getOWLClass(IRI.create(t[1])), ind(df, t[2]));
            case "P" -> df.getOWLObjectPropertyAssertionAxiom(
                df.getOWLObjectProperty(IRI.create(t[1])), ind(df, t[2]), ind(df, t[3]));
            case "D" -> df.getOWLDataPropertyAssertionAxiom(df.getOWLDataProperty(IRI.create(t[1])),
                ind(df, t[2]), df.getOWLLiteral(t[3], OWL2Datatype.XSD_INTEGER));
            case "S" -> df.getOWLSameIndividualAxiom(ind(df, t[1]), ind(df, t[2]));
            case "E" -> df.getOWLClassAssertionAxiom(df.getOWLObjectSomeValuesFrom(
                df.getOWLObjectProperty(IRI.create(t[1])), df.getOWLClass(IRI.create(t[2]))),
                ind(df, t[3]));
            case "F" -> df.getOWLClassAssertionAxiom(df.getOWLDataSomeValuesFrom(
                df.getOWLDataProperty(IRI.create(t[1])), df.getTopDatatype()), ind(df, t[2]));
            case "U" -> df.getOWLSubClassOfAxiom(df.getOWLClass(IRI.create(t[1])),
                df.getOWLClass(IRI.create(t[2])));
            default -> throw new IllegalArgumentException("unknown question " + t[0]);
        };
    }

    private static OWLIndividual ind(OWLDataFactory df, String iri) {
        return df.getOWLNamedIndividual(IRI.create(iri));
    }
}
