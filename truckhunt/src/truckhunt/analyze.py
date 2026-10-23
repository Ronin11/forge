"""Compare each listing's price with others of the same truck and year."""

from __future__ import annotations

import statistics
from dataclasses import dataclass
from typing import Mapping, Sequence


@dataclass
class Comparison:
    median: int | None
    samples: int
    basis: str          # 'year' (same family and year) or 'year±1' (too few same-year listings)
    pct_below: float | None  # 0.15 = 15% under the median; negative is over

    @property
    def label(self) -> str:
        if self.pct_below is None:
            return ""
        side = "below" if self.pct_below >= 0 else "above"
        return f"{abs(self.pct_below):.0%} {side} median ${self.median:,} ({self.samples} {self.basis})"


def compare(rows: Sequence[Mapping], min_samples: int = 3) -> dict[tuple[str, str], Comparison]:
    """Comparison for every row, keyed by (source, id).

    A listing isn't counted against itself, and the same truck posted on both
    sites counts twice; with a few dozen listings that skew is small next to
    the spread in trim and miles.
    """
    priced = [r for r in rows if r["price"]]
    out = {}
    for r in rows:
        if not r["price"]:
            out[(r["source"], r["id"])] = Comparison(None, 0, "", None)
            continue
        others = [o for o in priced if o["family"] == r["family"] and (o["source"], o["id"]) != (r["source"], r["id"])]
        same = [o["price"] for o in others if o["year"] == r["year"]]
        basis = "year"
        if len(same) < min_samples:
            same = [o["price"] for o in others if abs(o["year"] - r["year"]) <= 1]
            basis = "year±1"
        if len(same) < min_samples:
            out[(r["source"], r["id"])] = Comparison(None, len(same), basis, None)
            continue
        median = int(statistics.median(same))
        out[(r["source"], r["id"])] = Comparison(median, len(same), basis, 1 - r["price"] / median)
    return out
