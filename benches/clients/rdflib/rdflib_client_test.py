#!/usr/bin/env python3
"""rdflib's own SPARQL store against an NRESE server: what Python applications built on
rdflib do (SPARQLUpdateStore as a Graph's store, named graphs through a Dataset), and the
Graph Store Protocol with plain HTTP.

    python rdflib_client_test.py http://127.0.0.1:7878

Exits 0 when every check passes, prints the failing check otherwise.
"""

import sys
import urllib.request

from rdflib import RDF, RDFS, XSD, Dataset, Graph, Literal, Namespace, URIRef
from rdflib.plugins.stores.sparqlstore import SPARQLUpdateStore

EX = Namespace("http://example.com/rdflib/")
checks = 0


def check(ok, what):
    global checks
    checks += 1
    if not ok:
        print(f"FAILED: {what}", file=sys.stderr)
        sys.exit(1)
    print(f"ok {what}")


def main():
    server = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:7878"
    store = SPARQLUpdateStore(f"{server}/dataset/query", f"{server}/dataset/update")
    graph = Graph(store, identifier=URIRef("urn:x-arq:DefaultGraph"))

    # Writes through the store (rdflib sends INSERT DATA), then queries of every form.
    graph.add((EX.a, RDF.type, EX.C))
    graph.add((EX.a, RDFS.label, Literal("Grüße", lang="de")))
    graph.add((EX.a, EX.age, Literal(42)))
    graph.add((EX.a, EX.when, Literal("2026-10-03T06:00:00Z", datatype=XSD.dateTime)))
    graph.add((EX.a, EX.p, EX.b))
    graph.add((EX.b, EX.p, EX.c))
    rows = list(graph.query(f"SELECT ?o WHERE {{ <{EX.a}> <{EX.p}> ?o }}"))
    check([r[0] for r in rows] == [EX.b], "SELECT through the store")
    check(bool(graph.query(f"ASK {{ <{EX.a}> <{EX.p}>/<{EX.p}> <{EX.c}> }}").askAnswer), "ASK with a path")
    built = graph.query(f"CONSTRUCT {{ ?s <{EX.q}> ?o }} WHERE {{ ?s <{EX.p}> ?o }}").graph
    check(len(built) == 2, "CONSTRUCT")
    label = next(iter(graph.objects(EX.a, RDFS.label)))
    check(label == Literal("Grüße", lang="de"), f"a language-tagged literal with non-ASCII text: {label!r}")
    age = next(iter(graph.objects(EX.a, EX.age)))
    check(age.datatype == XSD.integer and age.toPython() == 42, f"an integer: {age!r}")
    when = next(iter(graph.objects(EX.a, EX.when)))
    check(when.datatype == XSD.dateTime, f"a dateTime: {when!r}")
    check((EX.a, RDF.type, EX.C) in graph, "a triple's presence")
    graph.remove((EX.b, EX.p, EX.c))
    check((EX.b, EX.p, EX.c) not in graph, "remove (DELETE DATA)")

    # A Dataset's default graph: rdflib writes it into the default graph and asks for it
    # by its own name (urn:x-rdflib:default), which NRESE reads as the default graph.
    plain = Dataset(store=store)
    plain.add((EX.d, RDF.type, EX.Default))
    rows = list(plain.query(f"SELECT ?s WHERE {{ ?s a <{EX.Default}> }}"))
    check([r[0] for r in rows] == [EX.d], "a Dataset's default graph")

    # Named graphs through a Dataset over the same store (default_union: the queries name
    # no graph, so GRAPH sees every named graph).
    dataset = Dataset(store=store, default_union=True)
    named = dataset.graph(URIRef(EX.g))
    named.add((EX.x, RDF.type, EX.Thing))
    named.add((EX.x, RDFS.label, Literal("x")))
    rows = list(dataset.query(f"SELECT ?s WHERE {{ GRAPH <{EX.g}> {{ ?s a <{EX.Thing}> }} }}"))
    check([r[0] for r in rows] == [EX.x], "a named graph written through a Dataset")

    # The Graph Store Protocol with plain HTTP: put, get, delete.
    turtle = f"<{EX.y}> <{RDFS.label}> \"y\" .\n".encode()
    target = f"{server}/dataset/data?graph={EX.h}"
    put = urllib.request.Request(target, data=turtle, method="PUT", headers={"Content-Type": "text/turtle"})
    with urllib.request.urlopen(put) as response:
        check(response.status in (200, 201, 204), f"GSP PUT ({response.status})")
    get = urllib.request.Request(target, headers={"Accept": "text/turtle"})
    with urllib.request.urlopen(get) as response:
        fetched = Graph().parse(data=response.read(), format="turtle")
    check(len(fetched) == 1 and (EX.y, RDFS.label, Literal("y")) in fetched, "GSP GET")
    delete = urllib.request.Request(target, method="DELETE")
    with urllib.request.urlopen(delete) as response:
        check(response.status in (200, 204), f"GSP DELETE ({response.status})")

    # Clean up.
    store.update(f"DELETE {{ ?s ?p ?o }} WHERE {{ ?s ?p ?o FILTER(STRSTARTS(STR(?s), \"{EX}\")) }}")
    store.update(f"DROP SILENT GRAPH <{EX.g}>")
    rows = list(graph.query(f"SELECT (COUNT(*) AS ?n) WHERE {{ ?s ?p ?o FILTER(STRSTARTS(STR(?s), \"{EX}\")) }}"))
    check(int(rows[0][0]) == 0, "cleaned up")
    print(f"{checks} checks passed")


if __name__ == "__main__":
    main()
