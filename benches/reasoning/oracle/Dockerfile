# Reference OWL 2 RL materialisation (owlrl on rdflib): the correctness oracle for small
# inputs. Slow by design; never a performance comparison. Queries over the closure are
# answered by pyoxigraph (see owlrl_materialise.py).
FROM python:3.13-slim
RUN pip install --no-cache-dir "rdflib==7.1.4" "owlrl==7.1.3" "pyoxigraph==0.5.11"
COPY owlrl_materialise.py answer_queries.py /oracle/
ENTRYPOINT ["python", "/oracle/owlrl_materialise.py"]
