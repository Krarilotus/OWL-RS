// Jena (ARQ) as a second oracle for NRESE's differential tests (benches/oracle/README.md).
//
//   docker run -v <dump>:/cases nrese-bench/jena-oracle /cases
//
// For every <test>/d<N>/ of a dump, twice: the dataset as NRESE loaded it (data.nq, answers
// in q<M>.jena) and with numbers in canonical lexical forms (data.canonical.nq, answers in
// q<M>.jena-canonical). An answer file is a line of the variables (ASK for an ASK), then
// one line per solution, the variables' terms in N-Triples syntax separated by tabs (UNDEF
// for unbound), or true/false for an ASK; or one line "ERROR <message>".
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;
import java.util.stream.*;
import org.apache.jena.graph.Node;
import org.apache.jena.query.*;
import org.apache.jena.riot.Lang;
import org.apache.jena.riot.RDFDataMgr;
import org.apache.jena.riot.out.NodeFmtLib;
import org.apache.jena.sparql.core.DatasetGraph;
import org.apache.jena.sparql.core.DatasetGraphFactory;
import org.apache.jena.graph.GraphMemFactory;
import org.apache.jena.sparql.engine.binding.Binding;
import org.apache.jena.sys.JenaSystem;

public class Oracle {
    public static void main(String[] args) throws Exception {
        JenaSystem.init();
        // The algebra as the standard defines it, not as ARQ's optimiser rewrites it: its
        // filter-disjunction rewrite turns FILTER(?d = <c> || ...) into a substitution of ?d,
        // also where ?d is unbound (a UNION branch), and invents solutions (README.md).
        ARQ.getContext().set(ARQ.optimization, false);
        List<Path> datasets;
        try (Stream<Path> walk = Files.walk(Path.of(args[0]))) {
            datasets = walk.filter(p -> p.getFileName().toString().equals("data.nq"))
                .map(Path::getParent).sorted().collect(Collectors.toList());
        }
        int answered = 0, errors = 0;
        for (Path dir : datasets) {
            // The dataset as NRESE loaded it, and with numbers in canonical lexical forms.
            for (String[] variant : new String[][] {{"data.nq", ".jena"}, {"data.canonical.nq", ".jena-canonical"}}) {
                Path source = dir.resolve(variant[0]);
                if (!Files.exists(source)) continue;
                // A graph that matches literals as RDF terms. The transactional in-memory dataset
                // finds "1"^^xsd:int for a bound "01"^^xsd:integer in property paths (value
                // matching), which the standard's term equality doesn't allow (README.md).
                DatasetGraph data = DatasetGraphFactory.wrap(GraphMemFactory.createDefaultGraphSameTerm());
                RDFDataMgr.read(data, source.toString(), Lang.NQUADS);
                List<Path> queries;
                try (Stream<Path> list = Files.list(dir)) {
                    queries = list.filter(p -> p.toString().endsWith(".rq")).sorted().collect(Collectors.toList());
                }
                for (Path file : queries) {
                    String name = file.getFileName().toString().replace(".rq", variant[1]);
                    List<String> lines = answer(data, file);
                    if (lines.get(0).startsWith("ERROR ")) errors++; else answered++;
                    Files.write(dir.resolve(name), lines, StandardCharsets.UTF_8);
                }
            }
        }
        System.err.printf("%d datasets, %d answers, %d errors%n", datasets.size(), answered, errors);
    }

    static List<String> answer(DatasetGraph data, Path file) {
        List<String> lines = new ArrayList<>();
        try {
            Query query = QueryFactory.create(Files.readString(file, StandardCharsets.UTF_8));
            try (QueryExecution execution = QueryExecution.dataset(DatasetFactory.wrap(data)).query(query).build()) {
                if (query.isAskType()) {
                    lines.add("ASK");
                    lines.add(Boolean.toString(execution.execAsk()));
                } else {
                    ResultSet results = execution.execSelect();
                    List<String> vars = results.getResultVars();
                    lines.add(String.join("\t", vars));
                    while (results.hasNext()) {
                        Binding binding = results.nextBinding();
                        StringJoiner row = new StringJoiner("\t");
                        for (String var : vars) {
                            Node node = binding.get(var);
                            row.add(node == null ? "UNDEF" : NodeFmtLib.strNT(node));
                        }
                        lines.add(row.toString());
                    }
                }
            }
        } catch (Exception e) {
            lines = List.of("ERROR " + String.valueOf(e.getMessage()).replace('\n', ' '));
        }
        return lines;
    }
}
