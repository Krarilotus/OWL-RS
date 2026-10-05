"""An independent oracle for the GeoSPARQL compliance benchmark's topology queries: the
answer GEOS (shapely) gives each query of the form `my:X geo:rel ?f` or `?f geo:rel my:X`,
from the relation's DE-9IM pattern (GeoSPARQL 1.1, tables 7-9), against the benchmark's
expected answer.

    python geo_oracle.py BENCHMARK_RESOURCES QUERY...
"""
import re
import sys
from pathlib import Path

import rdflib
from shapely import wkt

GEO = rdflib.Namespace("http://www.opengis.net/ont/geosparql#")
# GeoSPARQL 1.1: the DE-9IM pattern of each relation (a holds-for b).
PATTERNS = {
    "sfEquals": ["TFFFTFFFT"], "sfDisjoint": ["FF*FF****"],
    "sfIntersects": ["T********", "*T*******", "***T*****", "****T****"],
    "sfTouches": ["FT*******", "F**T*****", "F***T****"], "sfWithin": ["T*F**F***"],
    "sfContains": ["T*****FF*"], "sfCrosses": ["T*T***T**"], "sfOverlaps": ["T*T***T**"],
    "ehEquals": ["TFFFTFFFT"], "ehDisjoint": ["FF*FF****"],
    "ehMeet": ["FT*******", "F**T*****", "F***T****"], "ehOverlap": ["T*T***T**"],
    "ehCovers": ["T*TFT*FF*"], "ehCoveredBy": ["TFF*TFT**"], "ehInside": ["TFF*FFT**"],
    "ehContains": ["T*TFF*FF*"],
    "rcc8eq": ["TFFFTFFFT"], "rcc8dc": ["FFTFFTTTT"], "rcc8ec": ["FFTFTTTTT"],
    "rcc8po": ["TTTTTTTTT"], "rcc8tppi": ["TTTFTTFFT"], "rcc8tpp": ["TFFTTFTTT"],
    "rcc8ntpp": ["TFFTFFTTT"], "rcc8ntppi": ["TTTFFTFFT"],
}

resources = Path(sys.argv[1])
g = rdflib.Graph()
g.parse(resources / "gsb_dataset" / "dataset.rdf", format="xml")


def name(node):
    return str(node).rsplit("#", 1)[-1]


shapes = {}
for node, literal in g.subject_objects(GEO.asWKT):
    text = re.sub(r"^\s*<[^>]+>\s*", "", str(literal))
    try:
        shapes[name(node)] = wkt.loads(text)
    except Exception:  # noqa: BLE001 - unreadable test geometries are skipped
        pass
objects = dict(shapes)
for feature, geometry in g.subject_objects(GEO.hasDefaultGeometry):
    if name(geometry) in shapes:
        objects[name(feature)] = shapes[name(geometry)]


def holds(relation, a, b):
    if a.is_empty or b.is_empty:
        return False
    matrix = a.relate(b)
    return any(all(p == "*" or (p == "T" and m != "F") or p == m for p, m in zip(pattern, matrix))
               for pattern in PATTERNS[relation])


for query in sys.argv[2:]:
    text = (resources / "gsb_queries" / f"{query}.rq").read_text()
    m = re.search(r"my:(\w+)\s+geo:(\w+)\s+\?\w+", text)
    if m:
        fixed, relation, forward = m.group(1), m.group(2), True
    else:
        m = re.search(r"\?\w+\s+geo:(\w+)\s+my:(\w+)", text)
        relation, fixed, forward = m.group(1), m.group(2), False
    a = objects[fixed]
    geos = sorted(k for k, s in objects.items()
                  if (holds(relation, a, s) if forward else holds(relation, s, a)))
    srx = (resources / "gsb_answers" / f"{query}.srx").read_text()
    expected = sorted(re.findall(r"<uri>[^<]*#(\w+)</uri>", srx))
    print(f"{query}: {('my:' + fixed + ' ' + relation + ' ?f') if forward else ('?f ' + relation + ' my:' + fixed)}")
    print(f"  GEOS only: {sorted(set(geos) - set(expected))}")
    print(f"  expected only: {sorted(set(expected) - set(geos))}")
