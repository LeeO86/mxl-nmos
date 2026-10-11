#!/usr/bin/env python3
"""Compare nmos-testing results with the expected non-pass results.

    check_amwa.py <results-dir> <expected.json>

Every test must pass, or end in exactly the state the expected file lists for it
(with the reason). A listed test that now ends differently fails too, so the
list never hides a change. Writes <results-dir>/summary.md.
"""
import json
import pathlib
import sys
from collections import Counter


def main():
    out = pathlib.Path(sys.argv[1])
    expected = json.loads(pathlib.Path(sys.argv[2]).read_text())
    problems = []
    lines = ["| Suite | Results |", "| --- | --- |"]
    for suite, listed in expected.items():
        if suite.startswith("_"):
            continue
        path = out / f"{suite}.json"
        if not path.exists():
            problems.append(f"{suite}: no results ({path})")
            continue
        results = json.loads(path.read_text())["results"]
        counts = Counter(r["state"] for r in results)
        lines.append(f"| {suite} | {', '.join(f'{k} {v}' for k, v in sorted(counts.items()))} |")
        seen = set()
        for r in results:
            name, state = r["name"], r["state"]
            seen.add(name)
            want = listed.get(name)
            if want is None:
                if state != "Pass":
                    problems.append(f"{suite} {name}: {state}: {r['description']} | {r['detail']}")
            elif want["state"] != state:
                problems.append(f"{suite} {name}: expected {want['state']} ({want['reason']}), got {state}: {r['detail']}")
        for name in listed:
            if name not in seen:
                problems.append(f"{suite} {name}: listed but not run")
    summary = "\n".join(lines)
    (out / "summary.md").write_text(summary + "\n")
    print(summary)
    if problems:
        print("\nUnexpected results:")
        for p in problems:
            print("  " + p)
        sys.exit(1)
    print("\nAll results are Pass or expected.")


if __name__ == "__main__":
    main()
