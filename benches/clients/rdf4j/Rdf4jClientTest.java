// RDF4J's own Java client against an NRESE server: what GraphDB tooling, ResearchSpace and
// other RDF4J applications do through HTTPRepository and RemoteRepositoryManager.
//
//   java -cp "lib/*" Rdf4jClientTest.java http://127.0.0.1:7878
//
// Exits 0 when every check passes, prints the failing check otherwise.

import java.io.StringReader;
import java.util.List;
import java.util.Set;
import java.util.stream.Collectors;

import org.eclipse.rdf4j.model.IRI;
import org.eclipse.rdf4j.model.Model;
import org.eclipse.rdf4j.model.Statement;
import org.eclipse.rdf4j.model.ValueFactory;
import org.eclipse.rdf4j.model.impl.LinkedHashModel;
import org.eclipse.rdf4j.model.util.Values;
import org.eclipse.rdf4j.model.vocabulary.RDF;
import org.eclipse.rdf4j.model.vocabulary.RDFS;
import org.eclipse.rdf4j.query.BindingSet;
import org.eclipse.rdf4j.query.QueryLanguage;
import org.eclipse.rdf4j.query.TupleQueryResult;
import org.eclipse.rdf4j.repository.RepositoryConnection;
import org.eclipse.rdf4j.repository.RepositoryResult;
import org.eclipse.rdf4j.repository.config.RepositoryConfig;
import org.eclipse.rdf4j.repository.http.HTTPRepository;
import org.eclipse.rdf4j.repository.manager.RemoteRepositoryManager;
import org.eclipse.rdf4j.repository.sail.config.SailRepositoryConfig;
import org.eclipse.rdf4j.rio.RDFFormat;
import org.eclipse.rdf4j.sail.inferencer.fc.config.SchemaCachingRDFSInferencerConfig;
import org.eclipse.rdf4j.sail.memory.config.MemoryStoreConfig;

public class Rdf4jClientTest {
    static int checks = 0;

    static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            System.err.println("FAILED: " + what);
            System.exit(1);
        }
        System.out.println("ok " + what);
    }

    static final String EX = "http://example.com/";
    static final String DATA = "@prefix ex: <" + EX + "> .\n"
            + "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n"
            + "ex:a a ex:C ; rdfs:label \"A\"@en ; ex:p ex:b .\n"
            + "ex:C rdfs:subClassOf ex:D .\n";

    public static void main(String[] args) throws Exception {
        String server = args.length > 0 ? args[0] : "http://127.0.0.1:7878";
        boolean union = args.length > 1 && args[1].equals("union");
        ValueFactory vf = Values.getValueFactory();
        IRI a = vf.createIRI(EX, "a"), b = vf.createIRI(EX, "b"), p = vf.createIRI(EX, "p");
        IRI c = vf.createIRI(EX, "C"), d = vf.createIRI(EX, "D"), g = vf.createIRI(EX, "g");

        // The default repository through HTTPRepository.
        HTTPRepository repository = new HTTPRepository(server, "nrese");
        try (RepositoryConnection connection = repository.getConnection()) {
            connection.clear();
            connection.add(new StringReader(DATA), EX, RDFFormat.TURTLE);
            check(connection.size() == 4, "size after adding Turtle: " + connection.size());
            connection.add(a, p, c, g);
            check(connection.size(g) == 1, "size of a named graph");
            Set<String> contexts = connection.getContextIDs().stream()
                    .map(Object::toString).collect(Collectors.toSet());
            check(contexts.contains(EX + "g"), "context ids: " + contexts);
            try (RepositoryResult<Statement> found = connection.getStatements(a, p, null, false)) {
                List<Statement> list = found.stream().collect(Collectors.toList());
                check(list.size() == 2, "statements by pattern: " + list.size());
            }
            check(connection.hasStatement(a, RDFS.LABEL, vf.createLiteral("A", "en"), false),
                    "hasStatement with a language-tagged literal");
            String query = "SELECT ?x ?l WHERE { ?x <http://www.w3.org/2000/01/rdf-schema#label> ?l }";
            try (TupleQueryResult result = connection.prepareTupleQuery(QueryLanguage.SPARQL, query).evaluate()) {
                List<BindingSet> rows = result.stream().collect(Collectors.toList());
                check(rows.size() == 1 && rows.get(0).getValue("x").equals(a), "tuple query: " + rows);
            }
            check(connection.prepareBooleanQuery("ASK { <" + EX + "a> ?p ?o }").evaluate(), "ask query");
            Model graph = new LinkedHashModel();
            connection.prepareGraphQuery("CONSTRUCT WHERE { ?s ?p ?o }").evaluate().forEach(graph::add);
            // The default graph: the store's own (4), or the union of all graphs (5) with
            // store.default_graph = union, as RDF4J's and GraphDB's stores have it.
            int expected = union ? 5 : 4;
            check(graph.size() == expected, "graph query over the default graph: " + graph.size());
            connection.prepareUpdate("INSERT DATA { <" + EX + "b> <" + EX + "p> <" + EX + "a> }").execute();
            check(connection.hasStatement(b, p, a, false), "SPARQL update");

            // A transaction: its changes together, and gone after a rollback.
            connection.begin();
            connection.add(b, RDF.TYPE, c);
            connection.remove(a, p, b);
            check(connection.hasStatement(b, RDF.TYPE, c, false), "a transaction reads its own add");
            connection.commit();
            check(connection.hasStatement(b, RDF.TYPE, c, false) && !connection.hasStatement(a, p, b, false),
                    "a committed transaction");
            connection.begin();
            connection.add(a, RDF.TYPE, d);
            connection.rollback();
            check(!connection.hasStatement(a, RDF.TYPE, d, false), "a rolled-back transaction");

            connection.setNamespace("ex", EX);
            check(EX.equals(connection.getNamespace("ex")), "namespaces");
            connection.removeNamespace("ex");
            check(connection.getNamespace("ex") == null, "a removed namespace");

            connection.remove((IRI) null, null, null, g);
            check(connection.size(g) == 0, "removing a context");
        }

        // A repository created through RemoteRepositoryManager with RDF4J's RDFS inferencer:
        // it reasons, the default one doesn't.
        RemoteRepositoryManager manager = new RemoteRepositoryManager(server);
        manager.init();
        if (manager.hasRepositoryConfig("clienttest")) {
            manager.removeRepository("clienttest");
        }
        manager.addRepositoryConfig(new RepositoryConfig("clienttest", "Client test",
                new SailRepositoryConfig(new SchemaCachingRDFSInferencerConfig(new MemoryStoreConfig()))));
        check(manager.getRepositoryIDs().contains("clienttest"), "a repository created by the manager");
        HTTPRepository second = new HTTPRepository(server, "clienttest");
        try (RepositoryConnection connection = second.getConnection()) {
            connection.add(new StringReader(DATA), EX, RDFFormat.TURTLE);
            check(connection.hasStatement(a, RDF.TYPE, d, true), "the new repository reasons with RDFS");
            check(!connection.hasStatement(a, RDF.TYPE, d, false), "infer=false reads asserted statements");
        }
        manager.removeRepository("clienttest");
        check(!manager.getRepositoryIDs().contains("clienttest"), "a repository removed by the manager");
        manager.shutDown();
        System.out.println("ALL " + checks + " CHECKS PASSED");
    }
}
