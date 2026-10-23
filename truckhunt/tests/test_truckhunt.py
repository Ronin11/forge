"""Offline tests. The JSON and card shapes here are written to match what the
sites are known to send, not captured from them; `truckhunt run --dump` saves
real pages to turn into fixtures when a parser needs fixing."""

import json
import time
import unittest
from pathlib import Path

from truckhunt.analyze import compare
from truckhunt.extract import (Listing, fb_from_json, json_blobs, ksl_from_json, merge_listings,
                               parse_card, parse_miles, parse_price)
from truckhunt.models import load_support, match_listing, parse_cars_md
from truckhunt.store import Store

CARS_MD_ROWS = """\
|Make|Model|Supported Package|ACC|
|---|---|---|:---:|
|Chevrolet|Silverado 1500 2020-21|Safety Package II|openpilot available[<sup>1</sup>](#footnotes)|0 mph|
|Ford|F-150 2021-23|Co-Pilot360 Assist 2.0|openpilot available|0 mph|
|Ford|F-150 Hybrid 2021-23|Co-Pilot360 Assist 2.0|openpilot available|0 mph|
|Ford|F-150 Lightning 2022-23|Co-Pilot360 Assist 2.0|openpilot available|0 mph|
|Ford|Maverick 2022|LARIAT Luxury|openpilot|0 mph|
|Ford|Maverick 2023-24|Co-Pilot360 Assist|openpilot|0 mph|
|Honda|Ridgeline 2017-25|Honda Sensing|openpilot|26 mph|
|Audi[<sup>12</sup>](#footnotes)|A3 2014-19|ACC|openpilot|0 mph|
"""


class CarsMd(unittest.TestCase):
    def test_parses_truck_rows_only(self):
        s = parse_cars_md(CARS_MD_ROWS)
        self.assertEqual(set(s), {"silverado", "f150", "maverick", "ridgeline"})
        self.assertEqual(s["f150"].years, [2021, 2022, 2023])  # Lightning row left out
        self.assertEqual(s["ridgeline"].years[0], 2017)
        self.assertEqual(s["ridgeline"].years[-1], 2025)
        self.assertEqual(s["maverick"].packages_for(2022), ["LARIAT Luxury"])
        self.assertEqual(s["maverick"].packages_for(2023), ["Co-Pilot360 Assist"])

    def test_bundled_snapshot_loads(self):
        s = load_support()
        self.assertIn("ram1500", s)
        self.assertEqual(s["r1t"].packages_for(2023), ["All"])


class Matching(unittest.TestCase):
    support = load_support()

    def fam(self, title, **kw):
        m = match_listing(title, self.support, **kw)
        return m.family if m else None

    def test_titles(self):
        cases = {
            "2021 Ford F150 XLT SuperCrew 4x4": "f150",
            "2022 ford f-150 lariat": "f150",
            "2023 F-150 Platinum PowerBoost": "f150",
            "2020 Ford F-150 XLT": None,                  # year not supported
            "2022 Ford F-150 Lightning Lariat": None,      # EV, not on the list
            "2020 Chevy Silverado 1500 LT Trail Boss": "silverado",
            "2020 Chevrolet Silverado 2500HD LTZ": None,
            "2021 Chevrolet Silverado 3500 HD": None,
            "2021 GMC Sierra 1500 AT4": "sierra",
            "2021 Sierra 1500 SLT": None,                  # 'sierra' alone is too vague
            "2020 Ram 1500 Big Horn Crew Cab": "ram1500",
            "2021 Dodge Ram 1500 Laramie": "ram1500",
            "2019 Ram 1500 Classic Tradesman": None,
            "2020 Ram 2500 Cummins": None,
            "2018 Ram 1500 Sport": None,
            "2023 Rivian R1T Adventure": "r1t",
            "2019 Honda Ridgeline RTL-E": "ridgeline",
            "2023 Hyundai Santa Cruz SEL": "santacruz",
            "2024 Ford Ranger XLT": "ranger",
            "2023 Ford Ranger XLT": None,
            "Ford F150 good truck": None,                  # no year
        }
        for title, want in cases.items():
            with self.subTest(title=title):
                self.assertEqual(self.fam(title), want)

    def test_structured_fields(self):
        self.assertEqual(self.fam("XLT", make="Ford", model="Ranger", year=2024), "ranger")
        # A structured make that isn't Ford rules out the Maverick alias.
        self.assertIsNone(self.fam("1999 Mercury Maverick", make="Mercury", model="Maverick", year=2022))

    def test_package_status(self):
        m = match_listing("2022 Ford Maverick XLT", self.support)
        self.assertEqual((m.package_status, m.packages), ("check", ["LARIAT Luxury"]))
        m = match_listing("2022 Ford Maverick Lariat", self.support, extra_text="Has the Lariat Luxury package")
        self.assertEqual(m.package_status, "mentioned")
        # Co-Pilot360 Assist is the 2023-24 package; on a 2022 it isn't enough.
        m = match_listing("2022 Ford Maverick", self.support, extra_text="Co-Pilot 360 Assist")
        self.assertEqual(m.package_status, "check")
        m = match_listing("2022 Ford F-150 XLT", self.support, extra_text="BlueCruise ready, 302A")
        self.assertEqual(m.package_status, "mentioned")
        self.assertEqual(match_listing("2023 Rivian R1T", self.support).package_status, "likely")


class Parsing(unittest.TestCase):
    def test_price_and_miles(self):
        self.assertEqual(parse_price("$24,500"), 24500)
        self.assertIsNone(parse_price("$1"))
        self.assertIsNone(parse_price("Free"))
        self.assertEqual(parse_miles("80K miles"), 80000)
        self.assertEqual(parse_miles("123,456 mi"), 123456)
        self.assertEqual(parse_miles("Mileage: 45,000"), 45000)
        self.assertEqual(parse_miles("62.5K miles · Dealership"), 62500)
        self.assertIsNone(parse_miles("Clean title"))

    def test_fb_card(self):
        item = parse_card("fb", "https://www.facebook.com/marketplace/item/1234567890/?ref=search",
                          "$31,900\n2021 Ford F150 XLT\nOrem, UT\n64K miles")
        self.assertEqual((item.id, item.price, item.title, item.location, item.miles),
                         ("1234567890", 31900, "2021 Ford F150 XLT", "Orem, UT", 64000))
        self.assertEqual(item.url, "https://www.facebook.com/marketplace/item/1234567890/")
        self.assertIsNone(parse_card("fb", "https://www.facebook.com/groups/1/", "x"))

    def test_ksl_card(self):
        item = parse_card("ksl", "https://cars.ksl.com/listing/9876543",
                          "2020 Ram 1500 Laramie\n$38,995\n41,200 Miles\nLayton, UT")
        self.assertEqual((item.id, item.price, item.miles, item.title), ("9876543", 38995, 41200,
                                                                         "2020 Ram 1500 Laramie"))

    def test_fb_json(self):
        node = {"data": {"marketplace_search": {"feed_units": {"edges": [{"node": {"listing": {
            "id": "555", "marketplace_listing_title": "2022 Ford Maverick Lariat",
            "listing_price": {"amount": "29500.00", "formatted_amount": "$29,500"},
            "location": {"reverse_geocode": {"city": "Provo", "state": "UT"}},
            "custom_sub_titles_with_rendering_flags": [{"subtitle": "22K miles"}],
        }}}]}}}}
        body = "for (;;);" + json.dumps(node) + "\n" + json.dumps({"extensions": {}})
        items = [it for blob in json_blobs(body) for it in fb_from_json(blob)]
        self.assertEqual(len(items), 1)
        it = items[0]
        self.assertEqual((it.id, it.price, it.miles, it.location), ("555", 29500, 22000, "Provo, UT"))

    def test_ksl_json(self):
        page = {"props": {"pageProps": {"results": [
            {"id": 111, "makeYear": 2021, "make": "Chevrolet", "model": "Silverado 1500", "trim": "LT",
             "price": 36900, "mileage": 51000, "city": "Ogden", "state": "UT"},
            {"id": 7, "name": "Dealer of the month"},  # not a listing: no price
        ]}}}
        items = ksl_from_json(page)
        self.assertEqual(len(items), 1)
        self.assertEqual((items[0].title, items[0].miles, items[0].location),
                         ("2021 Chevrolet Silverado 1500 LT", 51000, "Ogden, UT"))

    def test_merge_fills_gaps(self):
        a = Listing("fb", "1", "u", title="2021 Ford F150", price=30000)
        b = Listing("fb", "1", "u", title="other", miles=5000, location="Lehi, UT")
        [m] = merge_listings([[a], [b]])
        self.assertEqual((m.title, m.price, m.miles, m.location), ("2021 Ford F150", 30000, 5000, "Lehi, UT"))


class StoreAndDeals(unittest.TestCase):
    def setUp(self):
        import tempfile
        self.tmp = tempfile.TemporaryDirectory()
        self.store = Store(Path(self.tmp.name) / "t.db")
        self.support = load_support()

    def tearDown(self):
        self.store.db.close()
        self.tmp.cleanup()

    def add(self, lid, price, title="2021 Ford F150 XLT", now=None):
        it = Listing("fb", lid, f"https://x/{lid}", title=title, price=price)
        return self.store.upsert(it, match_listing(title, self.support), now=now)

    def test_new_then_known_and_price_drop(self):
        t0 = time.time() - 100
        self.assertTrue(self.add("1", 35000, now=t0))
        self.assertFalse(self.add("1", 35000, now=t0 + 10))
        self.add("1", 32000, now=t0 + 20)
        drops = self.store.price_drops(since=t0 + 15)
        self.assertEqual([(r["id"], old) for r, old in drops], [("1", 35000)])
        self.assertEqual(self.store.all()[0]["price"], 32000)

    def test_mentioned_status_survives_card_resighting(self):
        it = Listing("fb", "9", "u", title="2022 Ford Maverick")
        self.store.upsert(it, match_listing(it.title, self.support, extra_text="lariat luxury"))
        self.store.upsert(it, match_listing(it.title, self.support))
        self.assertEqual(self.store.all()[0]["package_status"], "mentioned")

    def test_compare(self):
        for i, p in enumerate([30000, 32000, 34000, 36000]):
            self.add(str(i), p)
        self.add("cheap", 25000)
        cmp = compare(self.store.all(), min_samples=3)
        c = cmp[("fb", "cheap")]
        self.assertEqual(c.median, 33000)
        self.assertAlmostEqual(c.pct_below, 1 - 25000 / 33000)
        self.assertIn("below median $33,000", c.label)

    def test_compare_needs_samples(self):
        self.add("a", 30000)
        self.add("b", 31000)
        self.assertIsNone(compare(self.store.all(), min_samples=3)[("fb", "a")].pct_below)


if __name__ == "__main__":
    unittest.main()
