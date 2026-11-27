"""Measure candidate query plans against a live, stationary Linux application."""
import argparse
import asyncio
import csv
import json
import sys
from dataclasses import dataclass
from statistics import median
from time import perf_counter_ns

import xa11y
from dbus_fast import Message, MessageType
from dbus_fast.aio import MessageBus

ROOT = "/org/a11y/atspi/accessible/root"
ACCESSIBLE = "org.a11y.atspi.Accessible"
COLLECTION = "org.a11y.atspi.Collection"
PROPERTIES = "org.freedesktop.DBus.Properties"
REGISTRY = ("org.a11y.atspi.Registry", ROOT)


async def call(bus, node, interface, method, signature="", body=()):
    reply = await bus.call(Message(destination=node[0], path=node[1],
                                   interface=interface, member=method,
                                   signature=signature, body=list(body)))
    if reply.message_type == MessageType.ERROR:
        raise RuntimeError(f"{interface}.{method} on {node}: {reply.error_name} {reply.body}")
    return reply.body[0]


def mask(values, words):
    bits = [0] * max([words] + [n // 32 + 1 for n in values])
    for n in values:
        bits[n // 32] |= 1 << (n % 32)
    return [n if n < 2**31 else n - 2**32 for n in bits]


@dataclass(frozen=True)
class Case:
    selector: str
    roles: tuple = ()
    name: str | None = None
    enabled: bool = False
    exists: bool = False

    async def candidates(self, bus, root):
        states = (8, 24) if self.enabled else ()
        rule = [mask(states, 2), 2 if states else 1, {}, 1,
                mask(self.roles, 4), 2 if self.roles else 1, [], 1, False]
        cap = int(self.exists and self.name is None)
        refs = await call(bus, root, COLLECTION, "GetMatches",
                          "(aiia{ss}iaiiasib)uib", [rule, 1, cap, True])
        return [tuple(ref) for ref in refs]


async def properties(bus, refs, name, batch):
    async def read(ref):
        field = await call(bus, ref, PROPERTIES, "Get", "ss", [ACCESSIBLE, name])
        return field.value
    result = []
    for start in range(0, len(refs), batch):
        result.extend(await asyncio.gather(*(read(r) for r in refs[start:start + batch])))
    return result


async def within_depth(bus, refs, root, batch):
    # A fixture-level depth check. Share ancestry reads across all candidates in
    # this query. Raw parents alone cannot reconstruct every xa11y logical tree.
    parents = {}
    frontier = set(refs) - {root}
    for _ in range(51):
        if not frontier:
            break
        nodes = list(frontier)
        values = await properties(bus, nodes, "Parent", batch)
        parents.update((n, tuple(v)) for n, v in zip(nodes, values))
        frontier = {tuple(v) for v in values if v[1] != "/org/a11y/atspi/null"}
        frontier.difference_update(parents, {root})

    def included(ref):
        for _ in range(51):
            ref = parents.get(ref)
            if ref == root:
                return True
            if ref is None:
                return False
        return False
    return [r for r in refs if included(r)]


async def resolve(bus, root, case, batch, depth):
    refs = await case.candidates(bus, root)
    if case.name is not None:
        names = await properties(bus, refs, "Name", batch)
        refs = [r for r, name in zip(refs, names) if name == case.name]
    if depth:
        refs = await within_depth(bus, refs, root, batch)
    return refs


async def prefetched_walk(bus, root, case, batch, collection):
    """Prefetch records in batches, then walk actual child links from the scope."""
    # GetMatches can omit a remote child or include extra-deep descendants.
    # Treat the packet only as a list of addresses to prefetch. On a provider
    # without Collection the same loop discovers each successive tree level.
    hints = await Case("*").candidates(bus, root) if collection else []
    pending = {root, *hints}
    records = {}

    async def read(ref):
        fields = [call(bus, ref, ACCESSIBLE, "GetChildren"), call(bus, ref, ACCESSIBLE, "GetRole")]
        if case.name is not None:
            fields.append(call(bus, ref, PROPERTIES, "Get", "ss", [ACCESSIBLE, "Name"]))
        if case.enabled:
            fields.append(call(bus, ref, ACCESSIBLE, "GetState"))
        children, role, *extra = await asyncio.gather(*fields)
        name = extra[0].value if case.name is not None else None
        enabled = bool(extra[-1][0] & ((1 << 8) | (1 << 24))) if case.enabled else True
        return [tuple(c) for c in children], role, name, enabled

    null = "/org/a11y/atspi/null"
    while pending:
        nodes = list(pending)
        pending = set()
        for start in range(0, len(nodes), batch):
            refs = nodes[start:start + batch]
            rows = await asyncio.gather(*(read(ref) for ref in refs))
            records.update(zip(refs, rows))
            pending.update(c for row in rows for c in row[0] if c[0] and c[1] != null)
        pending.difference_update(records)

    found = []
    def visit(parent, depth):
        if depth > 50:
            return
        for node in records[parent][0]:
            if node not in records:
                continue
            if records[node][1] == 75:  # Match xa11y's nested application flattening.
                children = records[node][0]
            else:
                children = (node,)
            for child in children:
                if child not in records:
                    continue
                _, role, name, enabled = records[child]
                if role == 75:
                    continue
                if ((not case.roles or role in case.roles) and enabled and
                        (case.name is None or name == case.name)):
                    found.append(child)
                    if case.exists:
                        return
                visit(child, depth + 1)
                if case.exists and found:
                    return
    visit(root, 0)
    return found


async def measure(args, root, bus):
    daemon = ("org.freedesktop.DBus", "/org/freedesktop/DBus")
    pid = await call(bus, daemon, daemon[0], "GetConnectionUnixProcessID", "s", [root[0]])
    app = xa11y.App.by_pid(pid)
    collection = COLLECTION in await call(bus, root, ACCESSIBLE, "GetInterfaces")
    button = (43, 129)
    check_box = (7, 8)
    # These are explicit test translations, not a general selector parser.
    named = f"button[name={json.dumps(args.name)}]"
    cases = [
        Case("button", button), Case("check_box", check_box),
        Case("button, check_box", button + check_box),
        Case('button[enabled="true"]', button, enabled=True),
        Case("button", button, exists=True),
        Case(named, button, args.name),
        Case('button[name="missing benchmark button"]', button, "missing benchmark button"),
        Case(f"[name={json.dumps(args.name)}]", name=args.name),
    ]
    writer = csv.writer(sys.stdout)
    writer.writerow(["fixture", "selector", "operation", "matches", "strategy", "sample", "ms"])
    for case in cases:
        loc = app.locator(case.selector)
        expected = [(el.raw.get("bus_name", root[0]), el.stable_id) for el in loc.elements()]
        if case.exists:
            expected = expected[:1]
        strategies = ["current", "prefetch_walk"]
        if collection and not args.walk_only:
            strategies.extend(["native", "native_depth"])

        async def native_plan(strategy):
            if strategy == "prefetch_walk":
                return await prefetched_walk(bus, root, case, args.batch, collection)
            return await resolve(bus, root, case, args.batch, strategy == "native_depth")

        for strategy in strategies[1:]:
            actual = await native_plan(strategy)
            if actual != expected:
                raise ValueError(f"Ordered identities differ for {case.selector} ({strategy}): "
                                 f"{len(actual)} native, {len(expected)} xa11y")
        operation = "exists" if case.exists else "count"
        execute = getattr(loc, operation)
        timings = {s: [] for s in strategies}
        for i in range(args.samples + 1):
            offset = i % len(strategies)
            for strategy in strategies[offset:] + strategies[:offset]:
                start = perf_counter_ns()
                if strategy == "current":
                    count = int(execute())
                else:
                    refs = await native_plan(strategy)
                    count = len(refs)
                ms = (perf_counter_ns() - start) / 1e6
                if count != len(expected):
                    raise ValueError(f"Fixture changed during {case.selector}")
                if i:
                    timings[strategy].append(ms)
                    writer.writerow([args.fixture, case.selector, operation, count, strategy, i,
                                     round(ms, 4)])
        print(args.fixture, operation, case.selector, len(expected),
              *(round(median(v), 2) for v in timings.values()), file=sys.stderr)


async def main(args):
    session = await MessageBus().connect()
    try:
        addr = await call(session, ("org.a11y.Bus", "/org/a11y/bus"), "org.a11y.Bus", "GetAddress")
    finally:
        session.disconnect()
    bus = await MessageBus(bus_address=addr).connect()
    try:
        roots = await call(bus, REGISTRY, ACCESSIBLE, "GetChildren")
        for root in map(tuple, roots):
            if (await properties(bus, [root], "Name", 1))[0] == args.fixture:
                await measure(args, root, bus)
                return
        raise ValueError(f"No registered app named {args.fixture!r}")
    finally:
        bus.disconnect()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", help="registered AT-SPI application name")
    parser.add_argument("--name", default="OK", help="accessible name for residual-filter probes")
    parser.add_argument("--batch", type=int, default=64, help="maximum outstanding property reads")
    parser.add_argument("--walk-only", action="store_true", help="compare the child-link walk on embedded or deep trees")
    parser.add_argument("--samples", type=int, default=9)
    args = parser.parse_args()
    if min(args.batch, args.samples) < 1:
        parser.error("batch and samples must be positive")
    asyncio.run(main(args))
