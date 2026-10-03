// Apache Jena's own HTTP client against an NRESE server: what Jena applications, Fuseki
// tooling and the many libraries built on RDFConnection do.
//
//   java -cp "lib/*" JenaClientTest.java http://127.0.0.1:7878
//
// Exits 0 when every check passes, prints the failing check otherwise.

import java.util.List;

import org.apache.jena.query.Query;
import org.apache.jena.query.QueryFactory;
import org.apache.jena.query.QuerySolution;
import org.apache.jena.query.ResultSet;
import org.apache.jena.rdf.model.Literal;
import org.apache.jena.rdf.model.Model;
import org.apache.jena.rdf.model.ModelFactory;
import org.apache.jena.rdf.model.Property;
import org.apache.jena.rdf.model.Resource;
import org.apache.jena.rdfconnection.RDFConnection;
import org.apache.jena.rdfconnection.RDFConnectionRemote;
import org.apache.jena.riot.WebContent;
import org.apache.jena.sparql.exec.http.QueryExecutionHTTP;
import org.apache.jena.vocabulary.RDF;
import org.apache.jena.vocabulary.RDFS;
import org.apache.jena.vocabulary.XSD;

public class JenaClientTest {
    static int checks = 0;

    static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            System.err.println("FAILED: " + what);
            System.exit(1);
        }
        System.out.println("ok " + what);
    }

    static final String EX = "http://example.com/jena/";

    static long count(RDFConnection conn, String where) {
        long[] n = {0};
        conn.querySelect("SELECT (COUNT(*) AS ?n) WHERE { " + where + " }",
            row -> n[0] = row.getLiteral("n").getLong());
        return n[0];
    }

    public static void main(String[] args) throws Exception {
        String server = args.length > 0 ? args[0] : "http://127.0.0.1:7878";
        try (RDFConnection conn = RDFConnectionRemote.newBuilder()
                .destination(server)
                .queryEndpoint("dataset/query")
                .updateEndpoint("dataset/update")
                .gspEndpoint("dataset/data")
                .build()) {
            // SPARQL Update, then queries of every form.
            conn.update("PREFIX ex: <" + EX + "> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> "
                + "INSERT DATA { ex:a a ex:C ; rdfs:label \"Ä \\u00e9t\\u00e9\"@fr , \"plain\" ; ex:age 42 ; "
                + "ex:when \"2026-10-03T06:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> ; ex:p ex:b . "
                + "ex:b ex:p ex:c . }");
            check(count(conn, "?s ?p ?o FILTER(STRSTARTS(STR(?s), \"" + EX + "\"))") >= 7, "update then count");
            check(conn.queryAsk("PREFIX ex: <" + EX + "> ASK { ex:a ex:p/ex:p ex:c }"), "ASK with a property path");
            Model constructed = conn.queryConstruct("PREFIX ex: <" + EX + "> CONSTRUCT { ?s ex:q ?o } WHERE { ?s ex:p ?o }");
            check(constructed.size() == 2, "CONSTRUCT");
            Model described = conn.queryDescribe("DESCRIBE <" + EX + "a>");
            check(described.size() >= 6, "DESCRIBE");
            // Terms come back as written: language tag, datatype, unicode.
            String[] seen = {null, null, null};
            conn.querySelect("PREFIX ex: <" + EX + "> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> "
                + "SELECT ?l ?age ?when WHERE { ex:a rdfs:label ?l ; ex:age ?age ; ex:when ?when FILTER(LANG(?l) = \"fr\") }",
                row -> {
                    Literal l = row.getLiteral("l");
                    seen[0] = l.getLexicalForm() + "@" + l.getLanguage();
                    seen[1] = row.getLiteral("age").getDatatypeURI();
                    seen[2] = row.getLiteral("when").getDatatypeURI();
                });
            check("Ä été@fr".equals(seen[0]), "a language-tagged literal with non-ASCII text: " + seen[0]);
            check(XSD.integer.getURI().equals(seen[1]), "an integer's datatype: " + seen[1]);
            check(XSD.dateTime.getURI().equals(seen[2]), "a dateTime's datatype: " + seen[2]);

            // Results in XML and in JSON, as Jena asks for them.
            for (String accept : List.of(WebContent.contentTypeResultsXML, WebContent.contentTypeResultsJSON,
                    WebContent.contentTypeTextTSV)) {
                Query query = QueryFactory.create("SELECT ?o WHERE { <" + EX + "a> <" + EX + "p> ?o }");
                try (QueryExecutionHTTP qexec = QueryExecutionHTTP.service(server + "/dataset/query")
                        .query(query).acceptHeader(accept).build()) {
                    ResultSet rs = qexec.execSelect();
                    QuerySolution first = rs.next();
                    check(first.getResource("o").getURI().equals(EX + "b") && !rs.hasNext(),
                        "SELECT results as " + accept);
                }
            }

            // The Graph Store Protocol: put, get, post, delete a named graph.
            String graph = EX + "graph";
            Model model = ModelFactory.createDefaultModel();
            Resource x = model.createResource(EX + "x");
            Property name = model.createProperty(EX, "name");
            x.addProperty(RDF.type, model.createResource(EX + "Thing")).addProperty(name, "x");
            conn.put(graph, model);
            check(conn.fetch(graph).isIsomorphicWith(model), "GSP PUT then GET");
            Model more = ModelFactory.createDefaultModel();
            more.createResource(EX + "y").addProperty(RDFS.label, "y");
            conn.load(graph, more);
            check(conn.fetch(graph).size() == 3, "GSP POST adds");
            check(count(conn, "GRAPH <" + graph + "> { ?s ?p ?o }") == 3, "the named graph through SPARQL");
            conn.delete(graph);
            check(count(conn, "GRAPH <" + graph + "> { ?s ?p ?o }") == 0, "GSP DELETE");

            // Clean up.
            conn.update("DELETE { ?s ?p ?o } WHERE { ?s ?p ?o FILTER(STRSTARTS(STR(?s), \"" + EX + "\")) }");
            check(count(conn, "?s ?p ?o FILTER(STRSTARTS(STR(?s), \"" + EX + "\"))") == 0, "cleaned up");
        }
        System.out.println(checks + " checks passed");
    }
}
