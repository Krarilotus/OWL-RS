"""Pre-invocation host capability coverage, separate from statistical observations."""


def unsupported_cold(cases, series, modes_of):
    """Keep declared cold dimensions, with the existing host adapter's capability reason."""
    out = {}
    for case in cases:
        modes, why = modes_of(case)
        if why and "cold" not in modes:
            missing = {f"time/{s}": {"kind": "unsupported", "capability": "cold_cache", "reason": why}
                       for s in series.get(case["name"], []) if s.startswith("cold/")}
            if missing:
                out[case["name"]] = missing
    return out


def common_unsupported(cand, base, observed):
    """Only consistent predeclarations on every pass of both sides explain absent cold rows."""
    support = cand.get("unsupported", []) + base.get("unsupported", [])
    if not support or len(support) != len(cand["blocks"]) + len(base["blocks"]):
        return {}
    out = {}
    for name in set.intersection(*(set(s) for s in support)):
        declaration = support[0][name]
        if (name.startswith("time/cold/") and name[5:] not in observed
                and isinstance(declaration, dict) and declaration.get("kind") == "unsupported"
                and declaration.get("capability") == "cold_cache"
                and isinstance(declaration.get("reason"), str) and declaration["reason"]
                and all(s[name] == declaration for s in support)):
            out[name] = f"unsupported before execution on both sides: {declaration['reason']}"
    return out
