# truckhunt

Watches KSL Cars and Facebook Marketplace for used pickups that
[comma.ai / openpilot supports](https://github.com/commaai/openpilot/blob/master/docs/CARS.md),
remembers what it has seen, and tells you about new listings, price drops and
listings priced well under comparable trucks.

The supported pickups, from comma's list (refresh it with `truckhunt update-models`):

| Truck | Years | Needs |
|---|---|---|
| Chevrolet Silverado 1500 | 2020-21 | Safety Package II |
| GMC Sierra 1500 | 2020-21 | Driver Alert Package II |
| Ford F-150 / Hybrid (not Lightning) | 2021-23 | Co-Pilot360 Assist 2.0 |
| Ford Maverick / Hybrid | 2022 / 2023-24 | LARIAT Luxury / Co-Pilot360 Assist |
| Ford Ranger | 2024 | Adaptive Cruise Control with Lane Centering |
| Honda Ridgeline | 2017-25 | Honda Sensing |
| Hyundai Santa Cruz | 2022-24 | Smart Cruise Control |
| Ram 1500 (new body, not Classic) | 2019-24 | Adaptive Cruise Control |
| Rivian R1T | 2022-24 | standard |

The package matters as much as the year: plenty of 2021 F-150s lack
Co-Pilot360 Assist 2.0. Each listing gets a package status: `likely` (standard
on that truck), `mentioned` (the listing names the package or something that
implies it, such as BlueCruise), or `check` (nothing says either way; ask the
seller or decode the VIN).

## Setup

Run it on your own computer. The sites turn away datacenter and cloud IPs.

```sh
cd truckhunt
python3 -m venv .venv && . .venv/bin/activate
pip install -e .
playwright install chromium          # or set browser.channel = "chrome" to use your Chrome
mkdir -p ~/.truckhunt && cp src/truckhunt/config.example.toml ~/.truckhunt/config.toml
# edit price, miles, ZIP and radius in ~/.truckhunt/config.toml
truckhunt login                      # log in to Facebook in the window, set Marketplace location/radius
truckhunt run                        # first run: watch the window, solve any check it shows
```

## Use

```sh
truckhunt run                 # search, record, print/email new matches and drops, write listings.csv
truckhunt deals               # active listings ranked by how far under their median they are
truckhunt export all.csv      # every listing ever seen, sold ones included
truckhunt update-models       # pull comma's current supported list
```

`~/.truckhunt/listings.csv` is rewritten every run. It holds listings seen in
the last 14 days, with the median price for the same truck and year
(year ±1 when there are too few) and a `deal` flag for listings at least
`deals.pct_below` under it. The median compares asking prices only, not trim
or miles, so a deal flag is a reason to look, not a verdict.

For email alerts, fill in `[email]` and set the password in the environment
(`TRUCKHUNT_SMTP_PASSWORD`). For Gmail, that's an app password.

Schedule it a few times a day, not every few minutes. Once runs work in a
window, set `browser.headless = true` for cron:

```cron
17 7,12,18 * * * cd ~/truckhunt && .venv/bin/truckhunt run >> ~/.truckhunt/run.log 2>&1
```

## Getting past the bot checks

The scraper gets through by looking like a person browsing:

- a real browser (Chromium or your installed Chrome) with one saved profile,
  so cookies and your Facebook login carry over between runs;
- a visible window by default, which gets flagged less than headless;
- about 20 searches per run, random 6-15 second pauses, and a few scrolls on
  Facebook;
- when a site shows a check page, a visible window waits for you to solve it
  (up to 3 minutes). A headless run stops that site for the rest of the run
  and does not retry.

It doesn't rotate proxies or solve captchas automatically. At this volume
from a home connection, you shouldn't need either.

**Facebook account risk.** Marketplace needs a login, and automated browsing
breaks Facebook's terms. At a few runs a day the risk is low, but if it
flags you, it's your account that gets locked. A secondary account is
safer.

## When a site changes

Neither site publishes an API, and both change their pages. The parsers don't
depend on exact markup. On every page they read two things and combine them:
the visible text of each listing link (price, title, location, miles), and any
JSON the page loaded that looks like a listing. Even so, a redesign can break
them. When a run starts reporting `0 listings on the page`:

```sh
truckhunt run --sources ksl --dump dumps/
```

That saves each page's HTML, links and JSON under `dumps/`, ready to turn
into a test fixture and fix the parser. If KSL changes its search URLs, run
the search by hand and paste its URL into `ksl.url_template`.

## Tests

```sh
python -m unittest discover -s tests
```

The tests run offline against listing shapes written to match what the sites
send. They are not captured pages.
