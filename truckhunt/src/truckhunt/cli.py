"""truckhunt: watch KSL Cars and Facebook Marketplace for comma.ai-supported pickups."""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

from . import config as config_mod
from .analyze import compare
from .extract import Listing, merge_listings
from .models import fetch_cars_md, load_support, match_listing, parse_cars_md, save_support
from .notify import build_message, describe, send_email
from .store import Store, write_csv

SOURCES = {"ksl": "ksl", "fb": "facebook"}
ACTIVE_DAYS = 14  # a listing not seen for this long is treated as sold for price comparisons


def support_path(cfg: dict) -> Path:
    return cfg["data_dir"] / "supported_trucks.json"


def wanted(item: Listing, cfg: dict) -> bool:
    s = cfg["search"]
    if item.price is not None and not s["min_price"] <= item.price <= s["max_price"]:
        return False
    return item.miles is None or item.miles <= s["max_miles"]


def match(item: Listing, support):
    return match_listing(item.title, support, make=item.make, model=item.model, year=item.year,
                         extra_text=item.description)


def cmd_run(args, cfg) -> int:
    from .scrape import Scraper  # needs Playwright; the other commands don't

    support = load_support(support_path(cfg))
    families = cfg["search"].get("families") or list(support)
    unknown = [f for f in families if f not in support]
    if unknown:
        print(f"unknown truck families in config: {', '.join(unknown)} (known: {', '.join(support)})")
        return 2
    sources = [s for s in args.sources if cfg[SOURCES[s]]["enabled"]]
    store = Store(cfg["data_dir"] / "listings.db")
    started = time.time()
    known = {s: store.known_ids(s) for s in sources}

    with Scraper(cfg, dump_dir=args.dump) as sc:
        # Alternate sites, so neither sees a burst of back-to-back searches.
        found = merge_listings([sc.search(src, fam, support[fam]) for fam in families for src in sources])
        matched = [(it, m) for it in found if wanted(it, cfg) and (m := match(it, support))]
        print(f"{len(found)} listings seen, {len(matched)} are supported trucks in range")

        if cfg["browser"]["details"]:
            todo = [it for it, m in matched if m.package_status == "check" and it.id not in known[it.source]
                    and not it.description][: cfg["browser"]["max_details"]]
            if todo:
                print(f"reading {len(todo)} new listing(s) for package details")
            for it in todo:
                sc.details(it)
            matched = [(it, m) for it, _ in matched if wanted(it, cfg) and (m := match(it, support))]

    new = sum(store.upsert(it, m) for it, m in matched)
    print(f"{new} new")
    return report(cfg, store, since=started, alert=not args.no_alert)


def report(cfg, store: Store, since: float | None, alert: bool) -> int:
    active = store.all(active_since=time.time() - ACTIVE_DAYS * 86400)
    comparisons = compare(active, cfg["deals"]["min_samples"])
    deal_pct = cfg["deals"]["pct_below"]

    csv_path = cfg["data_dir"] / "listings.csv"
    rows = []
    for r in active:
        c = comparisons.get((r["source"], r["id"]))
        rows.append({**dict(r), "median_price": c.median if c else None,
                     "pct_below_median": round(c.pct_below, 3) if c and c.pct_below is not None else None,
                     "deal": bool(c and c.pct_below is not None and c.pct_below >= deal_pct)})
    write_csv(rows, csv_path)
    print(f"wrote {len(rows)} active listings to {csv_path}")

    if not alert:
        return 0
    new = store.unalerted()
    drops = store.price_drops(since) if since else []
    if not new and not drops:
        print("nothing new to report")
        return 0
    body = build_message(new, drops, comparisons, deal_pct)
    print("\n" + body)
    if cfg["email"]["enabled"]:
        deals = sum(1 for r in new if (c := comparisons.get((r["source"], r["id"]))) and
                    c.pct_below is not None and c.pct_below >= deal_pct)
        subject = f"truckhunt: {len(new)} new, {len(drops)} price drops" + (f", {deals} deals" if deals else "")
        send_email(cfg, subject, body)
        print(f"emailed {cfg['email']['to']}")
    store.mark_alerted(new)
    return 0


def cmd_deals(args, cfg) -> int:
    store = Store(cfg["data_dir"] / "listings.db")
    active = store.all(active_since=time.time() - ACTIVE_DAYS * 86400)
    comparisons = compare(active, cfg["deals"]["min_samples"])
    ranked = sorted((r for r in active if comparisons[(r["source"], r["id"])].pct_below is not None),
                    key=lambda r: comparisons[(r["source"], r["id"])].pct_below, reverse=True)
    for r in ranked[: args.top or None]:
        print(describe(r, comparisons[(r["source"], r["id"])], cfg["deals"]["pct_below"]) + "\n")
    if not ranked:
        print("not enough comparable listings yet; run `truckhunt run` a few times")
    return 0


def cmd_export(args, cfg) -> int:
    store = Store(cfg["data_dir"] / "listings.db")
    rows = [dict(r) for r in store.all()]
    write_csv(rows, args.out)
    print(f"wrote {len(rows)} listings (including ones no longer seen) to {args.out}")
    return 0


def cmd_update_models(args, cfg) -> int:
    support = parse_cars_md(fetch_cars_md())
    path = support_path(cfg)
    path.parent.mkdir(parents=True, exist_ok=True)
    save_support(support, path)
    for s in support.values():
        pkgs = "; ".join(f"{lo}-{hi}: {p}" if lo != hi else f"{lo}: {p}" for lo, hi, p in s.packages)
        print(f"{s.make} {s.model}: {pkgs}")
    print(f"saved to {path}")
    return 0


def cmd_login(args, cfg) -> int:
    from .scrape import open_login
    open_login(cfg)
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="truckhunt", description=__doc__)
    p.add_argument("--config", type=Path, help=f"config file (default {config_mod.DEFAULT_PATH})")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("login", help="open the browser profile to log in to Facebook and clear KSL checks")
    r = sub.add_parser("run", help="search both sites, record matches, alert on new ones, write the CSV")
    r.add_argument("--sources", nargs="+", choices=list(SOURCES), default=list(SOURCES))
    r.add_argument("--headless", action="store_true", help="no browser window (overrides config)")
    r.add_argument("--dump", type=Path, help="save each page's HTML and JSON here, for fixing the parsers")
    r.add_argument("--no-alert", action="store_true", help="record and export, but don't alert")
    d = sub.add_parser("deals", help="active listings ranked by how far under the median they are")
    d.add_argument("--top", type=int, default=15)
    e = sub.add_parser("export", help="every listing ever seen to a CSV")
    e.add_argument("out", type=Path)
    sub.add_parser("update-models", help="refresh the supported trucks from comma's CARS.md")

    args = p.parse_args(argv)
    cfg = config_mod.load(args.config)
    if getattr(args, "headless", False):
        cfg["browser"]["headless"] = True
    handler = {"login": cmd_login, "run": cmd_run, "deals": cmd_deals, "export": cmd_export,
               "update-models": cmd_update_models}[args.cmd]
    return handler(args, cfg)


if __name__ == "__main__":
    sys.exit(main())
