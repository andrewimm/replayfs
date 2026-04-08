#!/usr/bin/env python3
"""Insert 'install' events after every package.json write in a replayfs log."""

import json
import sys


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <log.ndjson>", file=sys.stderr)
        sys.exit(1)

    log_path = sys.argv[1]

    with open(log_path, "r") as f:
        lines = f.readlines()

    output = []
    seq = 0

    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue

        row = json.loads(stripped)

        if row.get("type") == "header":
            output.append(row)
            continue

        # Renumber seq
        seq += 1
        row["seq"] = seq
        output.append(row)

        # Check if this event wrote to a package.json (not a .tmp intermediate)
        path = row.get("path", "")
        op = row.get("op", "")
        if op in ("create", "modify") and path.endswith("package.json"):
            seq += 1
            install_event = {
                "type": "event",
                "seq": seq,
                "elapsed_ms": row["elapsed_ms"],
                "op": "install",
                "path": path,
            }
            output.append(install_event)

    with open(log_path, "w") as f:
        for row in output:
            f.write(json.dumps(row, separators=(",", ":")) + "\n")

    print(f"Wrote {len(output)} lines ({seq} events) to {log_path}")


if __name__ == "__main__":
    main()
