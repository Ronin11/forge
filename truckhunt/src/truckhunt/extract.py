"""Turn what a search page shows into Listing rows.

Both sites change their markup and JSON often, so nothing here depends on one
exact shape. Two independent readers run on every page and their results merge:

- cards: every link to a listing, read as the text a person sees on its card
  ("$24,500 / 2021 Ford F150 XLT / Provo, UT / 80K miles");
- json: every JSON blob the page loaded, walked for objects that look like a
  listing (an id plus a price plus a title or year/make/model).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any, Iterator


@dataclass
class Listing:
    source: str                 # 'ksl' or 'fb'
    id: str
    url: str
    title: str = ""
    price: int | None = None
    miles: int | None = None
    year: int | None = None
    make: str = ""
    model: str = ""
    location: str = ""
    description: str = ""
    extra: dict[str, Any] = field(default_factory=dict)

    def merge(self, other: "Listing") -> None:
        for name in ("title", "price", "miles", "year", "make", "model", "location", "description"):
            if not getattr(self, name) and getattr(other, name):
                setattr(self, name, getattr(other, name))


URL_PATTERNS = {
    "fb": re.compile(r"/marketplace/item/(\d+)"),
    "ksl": re.compile(r"/listing/(\d+)"),
}


def listing_url(source: str, listing_id: str) -> str:
    if source == "fb":
        return f"https://www.facebook.com/marketplace/item/{listing_id}/"
    return f"https://cars.ksl.com/listing/{listing_id}"


def parse_price(text: str) -> int | None:
    """'$24,500' -> 24500. 'Free' and '$1' placeholders -> None."""
    m = re.search(r"\$\s*([\d,]+)", text)
    if not m:
        return None
    value = int(m.group(1).replace(",", ""))
    return value if value >= 100 else None


def parse_miles(text: str) -> int | None:
    """'80K miles' -> 80000; '123,456 mi' -> 123456; 'Mileage: 45,000' -> 45000."""
    m = re.search(r"([\d.,]+)\s*([kK])?\s*(?:miles|mi\b)", text, re.I) or \
        re.search(r"mileage\D{0,3}([\d.,]+)\s*([kK])?", text, re.I)
    if not m:
        return None
    try:
        value = float(m.group(1).replace(",", ""))
    except ValueError:
        return None
    return int(value * 1000) if m.group(2) else int(value)


_LOCATION = re.compile(r"^[A-Z][A-Za-z .'-]+, [A-Z]{2}$")


def parse_card(source: str, href: str, text: str) -> Listing | None:
    """Read one search-result card from its link target and visible text."""
    m = URL_PATTERNS[source].search(href)
    if not m:
        return None
    lines = [ln.strip() for ln in text.splitlines() if ln.strip()]
    url = href.split("?")[0] if href.startswith("http") else listing_url(source, m.group(1))
    item = Listing(source, m.group(1), url)
    for ln in lines:
        if item.price is None and ln.lstrip().startswith("$"):
            item.price = parse_price(ln)
        elif item.miles is None and re.search(r"\b(miles|mi)\b", ln, re.I) and parse_miles(ln):
            item.miles = parse_miles(ln)
        elif not item.location and _LOCATION.match(ln):
            item.location = ln
        elif not item.title and re.search(r"\b(19|20)\d{2}\b", ln) and not ln.startswith("$"):
            item.title = ln
    if not item.title:
        # A card with no year anywhere: take the longest line that isn't the price.
        rest = [ln for ln in lines if not ln.startswith("$") and ln != item.location]
        item.title = max(rest, key=len, default="")
    return item


# ---- JSON walking --------------------------------------------------------

def iter_dicts(node: Any) -> Iterator[dict]:
    stack = [node]
    while stack:
        cur = stack.pop()
        if isinstance(cur, dict):
            yield cur
            stack.extend(cur.values())
        elif isinstance(cur, list):
            stack.extend(cur)


def _first(d: dict, *keys: str) -> Any:
    for k in keys:
        if k in d and d[k] not in (None, "", [], {}):
            return d[k]
    return None


def _as_int(v: Any) -> int | None:
    if isinstance(v, bool):
        return None
    if isinstance(v, (int, float)):
        return int(v)
    if isinstance(v, str):
        digits = re.sub(r"[^\d.]", "", v)
        try:
            return int(float(digits)) if digits else None
        except ValueError:
            return None
    if isinstance(v, dict):  # {'amount': '24500.00'} / {'value': 80000, 'unit': 'MILES'}
        return _as_int(_first(v, "amount", "value", "price"))
    return None


def _text(v: Any) -> str:
    if isinstance(v, str):
        return v
    if isinstance(v, dict):
        return str(_first(v, "text", "name", "display_name", "value") or "")
    return ""


def fb_from_json(blob: Any) -> list[Listing]:
    """Marketplace listing nodes, as in its GraphQL and in the page's embedded JSON."""
    out = []
    for d in iter_dicts(blob):
        if "marketplace_listing_title" not in d or not d.get("id"):
            continue
        item = Listing("fb", str(d["id"]), listing_url("fb", str(d["id"])),
                       title=_text(d.get("marketplace_listing_title")))
        price = d.get("listing_price") or {}
        item.price = _as_int(price.get("amount")) if isinstance(price, dict) else _as_int(price)
        if item.price is not None and item.price < 100:
            item.price = None
        geo = (d.get("location") or {}).get("reverse_geocode") or {}
        if geo.get("city"):
            item.location = ", ".join(x for x in (geo.get("city"), geo.get("state")) if x)
        for sub in d.get("custom_sub_titles_with_rendering_flags") or []:
            miles = parse_miles(_text(sub.get("subtitle")) if isinstance(sub, dict) else "")
            if miles:
                item.miles = miles
        odo = d.get("vehicle_odometer_data")
        if isinstance(odo, dict) and _as_int(odo.get("value")):
            item.miles = _as_int(odo.get("value"))
        item.make = _text(d.get("vehicle_make_display_name"))
        item.model = _text(d.get("vehicle_model_display_name"))
        desc = d.get("redacted_description") or d.get("description")
        item.description = _text(desc)
        out.append(item)
    return out


def ksl_from_json(blob: Any) -> list[Listing]:
    """KSL Cars listing objects, from its search API or the page's __NEXT_DATA__."""
    out = []
    for d in iter_dicts(blob):
        lid = _first(d, "id", "listingId", "listing_id")
        price = _as_int(_first(d, "price", "listPrice", "askingPrice"))
        year = _as_int(_first(d, "makeYear", "year", "modelYear"))
        make = _text(_first(d, "make", "makeName"))
        model = _text(_first(d, "model", "modelName"))
        title = _text(_first(d, "title", "name"))
        if not lid or price is None or not (title or (year and make)):
            continue
        if year and not 1980 <= year <= 2100:
            continue
        item = Listing("ksl", str(lid), listing_url("ksl", str(lid)), title=title or
                       " ".join(str(x) for x in (year, make, model, _text(d.get("trim"))) if x),
                       price=price if price >= 100 else None, year=year, make=make, model=model,
                       miles=_as_int(_first(d, "mileage", "miles", "odometer")))
        city, state = _text(_first(d, "city", "cityName")), _text(_first(d, "state", "stateCode"))
        item.location = ", ".join(x for x in (city, state) if x)
        item.description = _text(_first(d, "description", "sellerDescription"))
        out.append(item)
    return out


def json_blobs(text: str) -> Iterator[Any]:
    """Every JSON value in a response body. Facebook sends several JSON documents
    per response, one per line, sometimes behind a 'for (;;);' guard."""
    text = text.strip()
    if text.startswith("for (;;);"):
        text = text[len("for (;;);"):]
    try:
        yield json.loads(text)
        return
    except ValueError:
        pass
    for line in text.splitlines():
        line = line.strip()
        if line.startswith(("{", "[")):
            try:
                yield json.loads(line)
            except ValueError:
                continue


def merge_listings(groups: list[list[Listing]]) -> list[Listing]:
    by_id: dict[tuple[str, str], Listing] = {}
    for group in groups:
        for item in group:
            key = (item.source, item.id)
            if key in by_id:
                by_id[key].merge(item)
            else:
                by_id[key] = item
    return list(by_id.values())
