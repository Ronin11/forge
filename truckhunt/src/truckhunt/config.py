from __future__ import annotations

import tomllib
from importlib import resources
from pathlib import Path
from typing import Any

DEFAULT_PATH = Path("~/.truckhunt/config.toml").expanduser()


def _merge(base: dict, over: dict) -> dict:
    out = dict(base)
    for k, v in over.items():
        out[k] = _merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def load(path: Path | None) -> dict[str, Any]:
    """The example config's values, overridden by the user's file if there is one."""
    cfg = tomllib.loads(resources.files("truckhunt").joinpath("config.example.toml").read_text())
    path = path or DEFAULT_PATH
    if path.exists():
        cfg = _merge(cfg, tomllib.loads(path.read_text()))
    cfg["data_dir"] = Path(cfg["data_dir"]).expanduser()
    return cfg
