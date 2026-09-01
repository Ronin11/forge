package main

import (
	"context"
	"fmt"
	"net/http"
	"net/url"

	"forge/internal/core/stats"
)

// runRetro emits the reflection data pack (DESIGN.md §9.4). The --json flag
// is accepted for symmetry with every other command but changes nothing: the
// pack is the product, so retro always writes pretty-printed JSON to stdout.
func runRetro(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("retro")
	since := fs.String("since", "7d", "window: <N>h hours or <N>d days (e.g. 1d, 24h)")
	asJSON := fs.Bool("json", true, "accepted and ignored: retro always emits JSON")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge retro: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.retro")
	if code >= 0 {
		return code
	}
	if !*asJSON {
		fmt.Fprintln(c.stderr, "forge retro: output is always JSON; --json=false is ignored")
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("retro", err)
	}
	v := url.Values{}
	v.Set("since", *since)
	var pack stats.RetroPack
	if err := cl.do(ctx, http.MethodGet, "/api/v1/retro?"+v.Encode(), nil, &pack); err != nil {
		return c.fail("retro", err)
	}
	c.printJSON(pack)
	return 0
}
