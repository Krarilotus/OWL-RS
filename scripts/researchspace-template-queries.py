"""Every SPARQL query in ResearchSpace's templates, sent to NRESE: which don't parse or fail.

Usage: python3 scripts/researchspace-template-queries.py <unpacked ROOT.war> <NRESE base URL>

(The war is ResearchSpace's /var/lib/jetty/webapps/ROOT.war, copied out of its container
and unzipped; NRESE's base URL is the one ResearchSpace uses, e.g. http://127.0.0.1:10215.)
"""
import html
import json
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

root = pathlib.Path(sys.argv[1])
base = sys.argv[2].rstrip('/')

ATTRIBUTE = re.compile(r"""\b(?:query|count-query|select-query|construct-query|ask-query|insert-query|delete-query|filter-query|default-query|update)\s*=\s*(?:'([^']*)'|"([^"]*)")""", re.S)
START = re.compile(r'^\s*(?:(?:PREFIX|BASE)\b[^\n]*\n\s*)*(SELECT|ASK|CONSTRUCT|DESCRIBE)\b', re.I | re.S)

found = []
for path in root.rglob('*.html'):
    text = path.read_text(encoding='utf-8', errors='replace')
    for match in ATTRIBUTE.finditer(text):
        query = html.unescape(match.group(1) or match.group(2) or '')
        if not START.match(query):
            continue
        if '{{' in query or '[[' in query:
            continue
        found.append((path.name, query))

# ResearchSpace binds `?__token__`-style parameters before it sends a query; a value that
# keeps the query well formed.
def bind(query):
    return re.sub(r'\?(__\w+__)', '<http://example.com/value>', query)

results = {'ok': 0, 'syntax': [], 'error': []}
for name, query in found:
    data = urllib.parse.urlencode({'query': bind(query)}).encode()
    request = urllib.request.Request(base + '/dataset/query', data=data, headers={
        'Content-Type': 'application/x-www-form-urlencoded',
        'Accept': 'application/sparql-results+json, text/turtle;q=0.9',
    })
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            response.read()
            results['ok'] += 1
    except urllib.error.HTTPError as error:
        body = error.read().decode('utf-8', errors='replace')[:300]
        kind = 'syntax' if error.code == 400 else 'error'
        results[kind].append({'template': urllib.parse.unquote(name)[-80:], 'status': error.code,
                              'detail': body, 'query': query[:400]})
    except Exception as error:  # timeouts, resets
        results['error'].append({'template': urllib.parse.unquote(name)[-80:], 'status': 0,
                                 'detail': str(error), 'query': query[:400]})

print(f"queries: {len(found)}, ok: {results['ok']}, rejected (400): {len(results['syntax'])}, failed: {len(results['error'])}")
kinds = {}
others = []
for item in results['syntax'] + results['error']:
    detail = item['detail']
    if "found '??'" in detail:
        kind = 'placeholder ??'
    elif 'is not declared' in detail:
        kind = 'undeclared prefix ' + re.search(r"prefix '([^']*)'", detail).group(1)
    else:
        kind = 'other'
        others.append(item)
    kinds[kind] = kinds.get(kind, 0) + 1
print(json.dumps(kinds, indent=1))
print(json.dumps(others, indent=1)[:15000])
