"""SQLite record of every matching listing seen, and its price over time."""

from __future__ import annotations

import csv
import sqlite3
import time
from pathlib import Path

from .extract import Listing
from .models import Match

SCHEMA = """
CREATE TABLE IF NOT EXISTS listings (
    source TEXT NOT NULL,
    id TEXT NOT NULL,
    url TEXT NOT NULL,
    family TEXT NOT NULL,
    year INTEGER NOT NULL,
    title TEXT,
    price INTEGER,
    miles INTEGER,
    location TEXT,
    package_status TEXT,
    packages TEXT,
    first_seen REAL NOT NULL,
    last_seen REAL NOT NULL,
    alerted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source, id)
);
CREATE TABLE IF NOT EXISTS prices (
    source TEXT NOT NULL,
    id TEXT NOT NULL,
    seen REAL NOT NULL,
    price INTEGER NOT NULL
);
"""

COLUMNS = ("source", "id", "url", "family", "year", "title", "price", "miles", "location",
           "package_status", "packages", "first_seen", "last_seen")


class Store:
    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(path)
        self.db.row_factory = sqlite3.Row
        self.db.executescript(SCHEMA)

    def upsert(self, item: Listing, match: Match, now: float | None = None) -> bool:
        """Record a listing; True when it's new. A known listing keeps its first
        price in `prices` and gains a row only when the price changes."""
        now = now or time.time()
        row = self.db.execute("SELECT price, package_status FROM listings WHERE source=? AND id=?",
                              (item.source, item.id)).fetchone()
        packages = " / ".join(match.packages)
        if row is None:
            self.db.execute(
                "INSERT INTO listings (source, id, url, family, year, title, price, miles, location,"
                " package_status, packages, first_seen, last_seen) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (item.source, item.id, item.url, match.family, match.year, item.title, item.price,
                 item.miles, item.location, match.package_status, packages, now, now))
        else:
            # Search cards rarely carry the description, so don't let a later
            # card-only sighting downgrade a 'mentioned' found on the detail page.
            status = row["package_status"] if row["package_status"] == "mentioned" else match.package_status
            self.db.execute(
                "UPDATE listings SET title=?, price=COALESCE(?, price), miles=COALESCE(?, miles),"
                " location=COALESCE(NULLIF(?, ''), location), package_status=?, last_seen=?"
                " WHERE source=? AND id=?",
                (item.title, item.price, item.miles, item.location, status, now, item.source, item.id))
        if item.price is not None and (row is None or row["price"] != item.price):
            self.db.execute("INSERT INTO prices VALUES (?,?,?,?)", (item.source, item.id, now, item.price))
        self.db.commit()
        return row is None

    def set_package_status(self, source: str, listing_id: str, status: str) -> None:
        self.db.execute("UPDATE listings SET package_status=? WHERE source=? AND id=?",
                        (status, source, listing_id))
        self.db.commit()

    def known_ids(self, source: str) -> set[str]:
        return {r[0] for r in self.db.execute("SELECT id FROM listings WHERE source=?", (source,))}

    def all(self, active_since: float | None = None) -> list[sqlite3.Row]:
        if active_since is None:
            return list(self.db.execute("SELECT * FROM listings ORDER BY family, year, price"))
        return list(self.db.execute("SELECT * FROM listings WHERE last_seen >= ? ORDER BY family, year, price",
                                    (active_since,)))

    def unalerted(self) -> list[sqlite3.Row]:
        return list(self.db.execute("SELECT * FROM listings WHERE alerted=0 ORDER BY family, year, price"))

    def mark_alerted(self, rows: list[sqlite3.Row]) -> None:
        self.db.executemany("UPDATE listings SET alerted=1 WHERE source=? AND id=?",
                            [(r["source"], r["id"]) for r in rows])
        self.db.commit()

    def price_drops(self, since: float) -> list[tuple[sqlite3.Row, int]]:
        """Listings whose price fell in a change recorded since `since`: (row, old price)."""
        out = []
        for r in self.db.execute(
                "SELECT p.source, p.id, p.price, p.seen FROM prices p WHERE p.seen >= ?", (since,)):
            prev = self.db.execute(
                "SELECT price FROM prices WHERE source=? AND id=? AND seen < ? ORDER BY seen DESC LIMIT 1",
                (r["source"], r["id"], r["seen"])).fetchone()
            if prev and prev["price"] > r["price"]:
                row = self.db.execute("SELECT * FROM listings WHERE source=? AND id=?",
                                      (r["source"], r["id"])).fetchone()
                out.append((row, prev["price"]))
        return out


def write_csv(rows: list[dict], path: Path) -> None:
    rows = [{k: time.strftime("%Y-%m-%d %H:%M", time.localtime(v)) if k in ("first_seen", "last_seen") and v else v
             for k, v in r.items()} for r in rows]
    fields = list(rows[0].keys()) if rows else list(COLUMNS)
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        w.writerows(rows)
