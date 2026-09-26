// Jena rule-reasoner baseline for the reasoning benchmark (RT1 materialisation, RT3 queries).
//
//   docker run nrese-bench/jena-reasoner <rdfs|owl-micro|owl-mini|owl> <out.nt> <queries-dir|-> input.nt...
//
// Loads the inputs into one in-memory model, binds Jena's rule reasoner, forces the full
// closure (prepare() plus listing every statement), writes the inferred triples and prints
// timings and each query's answer count over the inference model.
import java.io.FileOutputStream;
import java.nio.file.*;
import java.util.*;
import org.apache.jena.query.*;
import org.apache.jena.rdf.model.*;
import org.apache.jena.reasoner.Reasoner;
import org.apache.jena.reasoner.ReasonerRegistry;
import org.apache.jena.riot.*;

public class Materialise {
    public static void main(String[] args) throws Exception {
        Reasoner reasoner = switch (args[0]) {
            case "rdfs" -> ReasonerRegistry.getRDFSReasoner();
            case "owl-micro" -> ReasonerRegistry.getOWLMicroReasoner();
            case "owl-mini" -> ReasonerRegistry.getOWLMiniReasoner();
            case "owl" -> ReasonerRegistry.getOWLReasoner();
            default -> throw new IllegalArgumentException(args[0]);
        };
        Model data = ModelFactory.createDefaultModel();
        long t0 = System.nanoTime();
        for (int i = 3; i < args.length; i++) RDFDataMgr.read(data, args[i], Lang.NTRIPLES);
        long t1 = System.nanoTime();
        InfModel inf = ModelFactory.createInfModel(reasoner, data);
        inf.prepare();
        Model inferred = ModelFactory.createDefaultModel();
        inf.listStatements().forEachRemaining(s -> { if (!data.contains(s)) inferred.add(s); });
        long t2 = System.nanoTime();
        System.err.printf("load %.1f s, closure %.1f s, asserted %d, inferred %d%n",
            (t1 - t0) / 1e9, (t2 - t1) / 1e9, data.size(), inferred.size());
        try (var out = new FileOutputStream(args[1])) { RDFDataMgr.write(out, inferred, Lang.NTRIPLES); }
        if (!args[2].equals("-")) {
            List<Path> queries = new ArrayList<>();
            try (var files = Files.list(Paths.get(args[2]))) {
                files.filter(p -> p.toString().endsWith(".rq")).sorted().forEach(queries::add);
            }
            for (Path q : queries) {
                long s = System.nanoTime();
                int rows = 0;
                try (QueryExecution exec = QueryExecutionFactory.create(Files.readString(q), inf)) {
                    ResultSet results = exec.execSelect();
                    while (results.hasNext()) { results.next(); rows++; }
                }
                System.out.printf("%s\t%d\t%.1f ms%n", q.getFileName().toString().replace(".rq", ""),
                    rows, (System.nanoTime() - s) / 1e6);
            }
        }
    }
}
