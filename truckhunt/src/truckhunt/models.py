"""Which pickups comma.ai supports, and whether a listing is one of them.

Years and required packages come from openpilot's docs/CARS.md (snapshot in
supported_trucks.json, refreshed by `truckhunt update-models`). The families
below only say how a truck is named in a listing title, which CARS.md can't.
"""

from __future__ import annotations

import json
import re
import urllib.request
from dataclasses import dataclass, field
from importlib import resources
from pathlib import Path

CARS_MD_URL = "https://raw.githubusercontent.com/commaai/openpilot/master/docs/CARS.md"


@dataclass(frozen=True)
class Family:
    key: str
    make: str               # make as CARS.md spells it
    cars_md_model: str      # CARS.md model rows starting with this belong here
    aliases: tuple[str, ...]        # any of these in the normalized title => this model
    make_aliases: tuple[str, ...]   # make words that may appear in a title
    needs_make: bool = False        # alias too generic to trust without the make
    exclude: tuple[str, ...] = ()   # words that mean a different, unsupported truck
    exclude_rows: tuple[str, ...] = ()  # CARS.md rows under the prefix that aren't this truck
    query: str = ""                 # search text for sites without structured filters


FAMILIES: tuple[Family, ...] = (
    Family("silverado", "Chevrolet", "Silverado 1500", ("silverado",), ("chevrolet", "chevy"),
           exclude=("2500", "3500", "2500hd", "3500hd", "hd"), query="chevrolet silverado 1500"),
    Family("sierra", "GMC", "Sierra 1500", ("sierra",), ("gmc",), needs_make=True,
           exclude=("2500", "3500", "2500hd", "3500hd", "hd"), query="gmc sierra 1500"),
    Family("f150", "Ford", "F-150", ("f150", "f 150"), ("ford",),
           exclude=("lightning",), exclude_rows=("F-150 Lightning",), query="ford f-150"),
    Family("maverick", "Ford", "Maverick", ("maverick",), ("ford",), query="ford maverick"),
    Family("ranger", "Ford", "Ranger", ("ranger",), ("ford",), needs_make=True, query="ford ranger"),
    Family("ridgeline", "Honda", "Ridgeline", ("ridgeline",), ("honda",), query="honda ridgeline"),
    Family("santacruz", "Hyundai", "Santa Cruz", ("santa cruz", "santacruz"), ("hyundai",),
           query="hyundai santa cruz"),
    # Ram 1500 Classic is the previous body kept on sale beside the new one; openpilot's
    # Ram 1500 support is the 2019+ new body only.
    Family("ram1500", "Ram", "1500", ("1500",), ("ram", "dodge"), needs_make=True,
           exclude=("classic", "2500", "3500"), query="ram 1500"),
    Family("r1t", "Rivian", "R1T", ("r1t",), ("rivian",), query="rivian r1t"),
)

FAMILY_BY_KEY = {f.key: f for f in FAMILIES}

# How sellers write each CARS.md package. A listing naming one of these (or the
# package itself) gets package_status 'mentioned'. Keyed by the package as CARS.md
# writes it; BlueCruise needs the Co-Pilot360 Assist 2.0 hardware, so it counts.
PACKAGE_HINTS: dict[str, tuple[str, ...]] = {
    "Co-Pilot360 Assist 2.0": ("copilot360 assist 2 0", "co pilot 360 assist 2 0", "copilot 360 assist 2 0",
                               "bluecruise", "blue cruise"),
    "Co-Pilot360 Assist": ("copilot360 assist", "co pilot 360 assist", "copilot 360 assist"),
    "Safety Package II": ("adaptive cruise",),
    "Driver Alert Package II": ("adaptive cruise",),
    "Adaptive Cruise Control (ACC)": ("adaptive cruise", "acc"),
    "Adaptive Cruise Control with Lane Centering": ("lane centering", "adaptive cruise"),
    "Smart Cruise Control (SCC)": ("smart cruise", "adaptive cruise"),
    "Honda Sensing": ("adaptive cruise",),
}


@dataclass
class Support:
    family: str
    make: str
    model: str
    years: list[int] = field(default_factory=list)
    # [[first_year, last_year, package], ...]: the 2022 Maverick needs a different
    # package from the 2023-24 one, so a package is tied to its years.
    packages: list[list] = field(default_factory=list)

    def packages_for(self, year: int) -> list[str]:
        return [pkg for lo, hi, pkg in self.packages if lo <= year <= hi]


def normalize(text: str) -> str:
    """Lowercase, punctuation to spaces: 'F-150 Co-Pilot360' -> 'f 150 co pilot360'."""
    text = re.sub(r"[^a-z0-9]+", " ", text.lower()).strip()
    # 'f150' and 'f 150' are both common; collapse so one alias form matches.
    return re.sub(r"\bf (150)\b", r"f150", text)


def _has_word(haystack: str, needle: str) -> bool:
    return re.search(rf"(?<![a-z0-9]){re.escape(normalize(needle))}(?![a-z0-9])", haystack) is not None


def _years(spec: str) -> list[int]:
    """'2020-21' -> [2020, 2021]; '2022' -> [2022]; '2017-25' -> 2017..2025."""
    m = re.fullmatch(r"(\d{4})(?:-(\d{2,4}))?", spec)
    if not m:
        return []
    start = int(m.group(1))
    if not m.group(2):
        return [start]
    end = m.group(2)
    end_year = int(end) if len(end) == 4 else int(str(start)[:2] + end)
    return list(range(start, end_year + 1))


def parse_cars_md(text: str) -> dict[str, Support]:
    """Pull the truck families' supported years and packages out of CARS.md's table."""
    out: dict[str, Support] = {}
    for line in text.splitlines():
        cells = line.split("|")
        if len(cells) < 5 or not line.startswith("|"):
            continue
        make = re.sub(r"\[<sup>.*?</sup>\]\(.*?\)", "", cells[1]).strip()
        model_cell, package = cells[2].strip(), cells[3].strip()
        m = re.fullmatch(r"(.+?) (\d{4}(?:-\d{2,4})?)", model_cell)
        if not m:
            continue
        model, years = m.group(1), _years(m.group(2))
        if not years:
            continue
        for fam in FAMILIES:
            if make != fam.make or not model.startswith(fam.cars_md_model):
                continue
            if any(model.startswith(x) for x in fam.exclude_rows):
                continue
            s = out.setdefault(fam.key, Support(fam.key, fam.make, fam.cars_md_model))
            s.years = sorted(set(s.years) | set(years))
            row = [years[0], years[-1], package]
            if row not in s.packages:
                s.packages.append(row)
    return out


def fetch_cars_md() -> str:
    with urllib.request.urlopen(CARS_MD_URL, timeout=30) as resp:
        return resp.read().decode("utf-8")


def save_support(support: dict[str, Support], path: Path) -> None:
    path.write_text(json.dumps({k: vars(v) for k, v in sorted(support.items())}, indent=2) + "\n")


def load_support(path: Path | None = None) -> dict[str, Support]:
    if path and path.exists():
        raw = json.loads(path.read_text())
    else:
        raw = json.loads(resources.files("truckhunt").joinpath("supported_trucks.json").read_text())
    return {k: Support(**v) for k, v in raw.items()}


@dataclass
class Match:
    family: str
    year: int
    # 'likely': the package is standard ('All'); 'mentioned': the listing names it
    # or a hint for it; 'check': nothing in the text says either way.
    package_status: str
    packages: list[str]


def find_year(text: str) -> int | None:
    m = re.search(r"(?<!\d)(19[89]\d|20[0-4]\d)(?!\d)", text)
    return int(m.group(1)) if m else None


def match_listing(title: str, support: dict[str, Support], *, make: str = "", model: str = "",
                  year: int | None = None, extra_text: str = "") -> Match | None:
    """Return the supported family a listing belongs to, or None.

    `make`/`model`/`year` are structured fields when a site has them (KSL);
    otherwise everything comes from the title (Facebook).
    """
    name = normalize(" ".join(x for x in (make, model, title) if x))
    year = year or find_year(title)
    if not year:
        return None
    for fam in FAMILIES:
        s = support.get(fam.key)
        if not s or year not in s.years:
            continue
        if not any(_has_word(name, a) for a in fam.aliases):
            continue
        has_make = any(_has_word(name, a) for a in fam.make_aliases)
        if fam.needs_make and not has_make:
            continue
        if make and not has_make:
            continue  # a structured make that isn't this family's (e.g. a Mercury Maverick)
        if any(_has_word(name, x) for x in fam.exclude):
            continue
        packages = s.packages_for(year)
        return Match(fam.key, year, package_status(packages, title + " " + extra_text), packages)
    return None


def package_status(packages: list[str], text: str) -> str:
    if any(p.strip().lower() == "all" for p in packages):
        return "likely"
    norm = normalize(text)
    words = [w for p in packages for w in (p, *PACKAGE_HINTS.get(p, ()))]
    if any(_has_word(norm, w) for w in words):
        return "mentioned"
    return "check"
