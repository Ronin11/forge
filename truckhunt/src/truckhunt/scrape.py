"""Drive a real Chromium through the search pages, like a person browsing.

What gets this past the sites' bot checks is being ordinary: a full browser
(not bare HTTP requests), one persistent profile with your own Facebook
login and cookies, a visible window by default, a few dozen page loads per
run with random pauses between them, and stopping when a site pushes back
instead of retrying. When a check page appears in a visible window, it
waits for you to clear it by hand.
"""

from __future__ import annotations

import json
import random
import re
import sys
import time
from pathlib import Path
from typing import Callable
from urllib.parse import quote, quote_plus

from playwright.sync_api import BrowserContext, Page, Response, sync_playwright
from playwright.sync_api import Error as PlaywrightError

from .extract import Listing, fb_from_json, json_blobs, ksl_from_json, merge_listings, parse_card, parse_miles
from .models import FAMILY_BY_KEY, Support

Log = Callable[[str], None]

BLOCK_MARKERS = (
    "access to this page has been denied", "px-captcha", "verify you are human",
    "just a moment", "pardon our interruption", "unusual traffic", "are you a robot",
)
CHALLENGE_WAIT = 180  # seconds to wait for a person to clear a check page


class Blocked(Exception):
    pass


def search_url(source: str, cfg: dict, family: str, support: Support) -> str:
    fam = FAMILY_BY_KEY[family]
    s = cfg["search"]
    if source == "ksl":
        k = cfg["ksl"]
        return k["url_template"].format(
            make=quote(fam.make), model=quote(fam.cars_md_model), year_from=support.years[0],
            year_to=support.years[-1], min_price=s["min_price"], max_price=s["max_price"],
            max_miles=s["max_miles"], zip=k["zip"], radius=k["radius_miles"], query=quote_plus(fam.query))
    f = cfg["facebook"]
    loc = f["location"].strip("/")
    return f["url_template"].format(
        location=f"{loc}/" if loc else "", query=quote_plus(fam.query), min_price=s["min_price"],
        max_price=s["max_price"], days=f["days_since_listed"])


class Scraper:
    def __init__(self, cfg: dict, log: Log = print, dump_dir: Path | None = None):
        self.cfg = cfg
        self.b = cfg["browser"]
        self.log = log
        self.dump_dir = dump_dir
        self.blocked: set[str] = set()
        self._responses: list[str] = []
        self._pw = None
        self.ctx: BrowserContext | None = None
        self.page: Page | None = None

    # ---- lifecycle -------------------------------------------------------

    def __enter__(self) -> "Scraper":
        self._pw = sync_playwright().start()
        profile = self.cfg["data_dir"] / "profile"
        profile.mkdir(parents=True, exist_ok=True)
        self.ctx = self._pw.chromium.launch_persistent_context(
            str(profile),
            headless=self.b["headless"],
            viewport={"width": 1366, "height": 900},
            locale="en-US",
            timezone_id="America/Denver",
            # Without this Chromium announces itself as automated (navigator.webdriver).
            args=["--disable-blink-features=AutomationControlled"],
            channel=self.b.get("channel") or None,
            executable_path=self.b.get("executable_path") or None,
        )
        self.page = self.ctx.pages[0] if self.ctx.pages else self.ctx.new_page()
        self.page.on("response", self._on_response)
        return self

    def _fresh_page(self) -> None:
        """Swap in a new tab. After a failed load Chromium is still navigating the old
        one to its error page, and that navigation cuts off the next goto."""
        old, self.page = self.page, self.ctx.new_page()
        self.page.on("response", self._on_response)
        try:
            old.close()
        except PlaywrightError:
            pass

    def __exit__(self, *exc) -> None:
        if self.ctx:
            self.ctx.close()
        if self._pw:
            self._pw.stop()

    def _on_response(self, resp: Response) -> None:
        # Facebook's GraphQL replies are sometimes typed text/javascript or text/html.
        if "json" not in resp.headers.get("content-type", "") and "graphql" not in resp.url:
            return
        try:
            self._responses.append(resp.text())
        except PlaywrightError:
            pass  # body gone (redirect, navigation away)

    # ---- browsing --------------------------------------------------------

    def pause(self, lo: float | None = None, hi: float | None = None) -> None:
        time.sleep(random.uniform(lo if lo is not None else self.b["min_delay"],
                                  hi if hi is not None else self.b["max_delay"]))

    def goto(self, url: str, source: str) -> None:
        self._responses = []
        try:
            self.page.goto(url, wait_until="domcontentloaded", timeout=60_000)
        except PlaywrightError:
            self._fresh_page()
            raise
        try:
            self.page.wait_for_load_state("networkidle", timeout=15_000)
        except PlaywrightError:
            pass  # Facebook never goes idle; what's loaded by now is enough
        self._check_blocked(source)

    def _block_reason(self, source: str) -> str | None:
        url = self.page.url
        if source == "fb":
            if "/login" in url or "/checkpoint" in url:
                return "Facebook wants you to log in or confirm it's you"
            if self.page.locator('form[action*="login"] input[name="email"]').count():
                return "Facebook isn't logged in"
        try:
            text = (self.page.title() + " " + self.page.inner_text("body", timeout=5_000)[:3000]).lower()
        except PlaywrightError:
            return None
        hit = next((m for m in BLOCK_MARKERS if m in text), None)
        return f"bot check page ({hit!r})" if hit else None

    def _check_blocked(self, source: str) -> None:
        reason = self._block_reason(source)
        if not reason:
            return
        if self.b["headless"]:
            raise Blocked(f"{reason}. Run `truckhunt login`, or set browser.headless = false.")
        self.log(f"  {reason}. Clear it in the browser window; waiting up to {CHALLENGE_WAIT}s...")
        deadline = time.time() + CHALLENGE_WAIT
        while time.time() < deadline:
            time.sleep(3)
            if not self._block_reason(source):
                self.log("  cleared, carrying on")
                self.page.wait_for_load_state("domcontentloaded")
                return
        raise Blocked(reason)

    def scroll(self, times: int) -> None:
        for _ in range(times):
            self.page.mouse.wheel(0, random.randint(1800, 3200))
            self.pause(2, 5)

    # ---- reading ---------------------------------------------------------

    def _collect(self, source: str) -> list[Listing]:
        cards = self.page.eval_on_selector_all(
            "a[href]", "els => els.map(e => [e.href, e.innerText || ''])")
        scripts = self.page.eval_on_selector_all(
            'script[type="application/json"], script#__NEXT_DATA__, script[type="application/ld+json"]',
            "els => els.map(e => e.textContent || '')")
        from_cards = [c for href, text in cards if (c := parse_card(source, href, text))]
        reader = fb_from_json if source == "fb" else ksl_from_json
        from_json = [item for body in [*scripts, *self._responses] for blob in json_blobs(body)
                     for item in reader(blob)]
        if self.dump_dir:
            self._dump(source, cards, scripts)
        # JSON first: it has the structured fields; cards fill what it lacks.
        return merge_listings([from_json, from_cards])

    def _dump(self, source: str, cards: list, scripts: list[str]) -> None:
        self.dump_dir.mkdir(parents=True, exist_ok=True)
        stem = self.dump_dir / f"{source}-{int(time.time() * 1000)}"
        stem.with_suffix(".html").write_text(self.page.content())
        stem.with_suffix(".json").write_text(json.dumps(
            {"url": self.page.url, "cards": cards, "scripts": scripts, "responses": self._responses}))
        self.log(f"  dumped page to {stem}.*")

    def search(self, source: str, family: str, support: Support) -> list[Listing]:
        if source in self.blocked:
            return []
        url = search_url(source, self.cfg, family, support)
        self.log(f"[{source}] {family}: {url}")
        try:
            self.goto(url, source)
            if source == "fb":
                self.scroll(self.b["max_scrolls"])
            found = self._collect(source)
        except Blocked as e:
            self.log(f"  stopped {source} for this run: {e}")
            self.blocked.add(source)
            return []
        except PlaywrightError as e:
            self.log(f"  page failed: {e.message.splitlines()[0]}")
            return []
        self.log(f"  {len(found)} listings on the page")
        self.pause()
        return found

    def details(self, item: Listing) -> Listing:
        """Open one listing and fill its description (and miles, if the card lacked them)."""
        if item.source in self.blocked:
            return item
        try:
            self.goto(item.url, item.source)
            body = self.page.inner_text("body", timeout=10_000)
        except Blocked as e:
            self.log(f"  stopped {item.source} details for this run: {e}")
            self.blocked.add(item.source)
            return item
        except PlaywrightError as e:
            self.log(f"  detail page failed: {e.message.splitlines()[0]}")
            return item
        for found in self._collect(item.source):
            if found.id == item.id:
                item.merge(found)
        # The visible page text is the fallback that never depends on markup. Only a
        # fallback: it also holds the titles of other listings the page suggests.
        if not item.description:
            item.description = body[:20000]
        if item.miles is None:
            m = re.search(r"(?:driven|mileage|odometer)[^\n]{0,40}", body, re.I)
            item.miles = parse_miles(m.group(0)) if m else None
        self.pause()
        return item


def open_login(cfg: dict) -> None:
    """Open the profile's browser on both sites so you can log in and clear any checks."""
    cfg = {**cfg, "browser": {**cfg["browser"], "headless": False}}
    with Scraper(cfg) as s:
        s.page.goto("https://www.facebook.com/marketplace/")
        s.ctx.new_page().goto("https://cars.ksl.com/")
        print("Log in to Facebook (and set your Marketplace location and radius) in the window that opened,\n"
              "then come back here and press Enter. Your session stays in the browser profile.")
        try:
            input()
        except EOFError:
            pass
    print("Saved.", file=sys.stderr)
