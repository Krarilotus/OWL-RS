# Reference OWL 2 RL materialisation (owlrl on rdflib): the correctness oracle for small
# inputs. Slow by design; never a performance comparison.
FROM python:3.13-slim
RUN pip install --no-cache-dir "rdflib==7.1.4" "owlrl==7.1.3"
COPY owlrl_materialise.py /oracle/owlrl_materialise.py
ENTRYPOINT ["python", "/oracle/owlrl_materialise.py"]
