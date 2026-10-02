"""Usage: bsbm-bi-queries.py [BSBM dir]: fills the BSBM BI templates with fixed parameters
into <dir>/biq/qN.rq (used by bsbm-bi-lab.sh)."""
# Fills the BSBM BI templates with fixed parameters into scratch/bsbm/biq/qN.rq.
import os, re, sys
base = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser('~/nrese-bench/scratch/bsbm')
tpl = base + '/bsbmtools-0.2/queries/bi'
out = base + '/biq'
os.makedirs(out, exist_ok=True)
inst = 'http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/'
values = {
    'Country1': '<http://downlode.org/rdf/iso-3166/countries#US>',
    'Country2': '<http://downlode.org/rdf/iso-3166/countries#DE>',
    'Country': '<http://downlode.org/rdf/iso-3166/countries#AT>',
    'Product': f'<{inst}dataFromProducer1/Product1>',
    'ConsecutiveMonth_0': '2008-01-01',
    'ConsecutiveMonth_1': '2008-02-01',
    'ConsecutiveMonth_2': '2008-03-01',
    'ProductType': f'<{inst}ProductType6>',
    'Producer': f'<{inst}dataFromProducer1/Producer1>',
}
for n in range(1, 9):
    text = open(f'{tpl}/query{n}.txt').read()
    text = re.sub(r'%(\w+)%', lambda m: values[m.group(1)], text)
    open(f'{out}/q{n}.rq', 'w').write(text)
print('ok', sorted(os.listdir(out)))
