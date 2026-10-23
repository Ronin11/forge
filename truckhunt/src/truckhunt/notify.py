from __future__ import annotations

import os
import smtplib
from email.message import EmailMessage
from typing import Mapping, Sequence

from .analyze import Comparison

PACKAGE_NOTE = {
    "likely": "package: standard on this truck",
    "mentioned": "package: listing mentions it",
    "check": "package: CHECK - needs {packages}",
}


def describe(row: Mapping, cmp: Comparison | None, deal_pct: float) -> str:
    price = f"${row['price']:,}" if row["price"] else "no price"
    miles = f"{row['miles']:,} mi" if row["miles"] else "miles ?"
    deal = "  ** DEAL **" if cmp and cmp.pct_below is not None and cmp.pct_below >= deal_pct else ""
    lines = [f"{row['title'] or row['family']} - {price}, {miles}, {row['location'] or '?'} [{row['source']}]{deal}",
             f"  {row['url']}",
             "  " + PACKAGE_NOTE.get(row["package_status"], "").format(packages=row["packages"])]
    if cmp and cmp.label:
        lines.append(f"  {cmp.label}")
    return "\n".join(lines)


def build_message(new: Sequence[Mapping], drops: Sequence[tuple[Mapping, int]],
                  comparisons: dict, deal_pct: float) -> str:
    parts = []
    if new:
        parts.append(f"{len(new)} new comma-supported truck listing(s):\n")
        parts += [describe(r, comparisons.get((r["source"], r["id"])), deal_pct) + "\n" for r in new]
    if drops:
        parts.append(f"{len(drops)} price drop(s):\n")
        for r, old in drops:
            parts.append(f"  was ${old:,}\n" + describe(r, comparisons.get((r["source"], r["id"])), deal_pct) + "\n")
    return "\n".join(parts)


def send_email(cfg: dict, subject: str, body: str) -> None:
    e = cfg["email"]
    password = os.environ.get(e["password_env"], "")
    if not password:
        raise RuntimeError(f"email is enabled but ${e['password_env']} is not set")
    msg = EmailMessage()
    msg["Subject"], msg["From"], msg["To"] = subject, e["from"] or e["username"], e["to"]
    msg.set_content(body)
    with smtplib.SMTP(e["smtp_host"], e["smtp_port"], timeout=30) as smtp:
        smtp.starttls()
        smtp.login(e["username"], password)
        smtp.send_message(msg)
