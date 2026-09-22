# Nate's list

Decisions and actions only the operator can take. Forge keeps everything
else moving; this page is what it is waiting on, kept short and current.
Cross one off by deleting it, add one with a line. The weekly engineering
job does not read this; a person does.

- [ ] **A real customer for the intake interview.** The stand-in
  plumber (task 370) is withdrawn. When someone real is set up in person,
  file the interview with their contact name and the five things they do
  by hand: `forge add ~/Projects/forge --workflow intake "<their brief>"`.
  It is the front door to the first customer automation (docs/JOBS.md
  step 7).
- [ ] **A hostname for the game and the portal.** Nucleosynthesis at
  `play.<domain>` on the equitizr box behind Caddy with the same
  on-landing deploy; the portal at `portal.<domain>` proxied over the
  tailnet to wherever the store lives. Name the domain and both targets
  get filed.
- [ ] **The dev.home day.** Reinstall the VM on a supported OS, Tailscale
  on the Proxmox host as a subnet router, an API token in a `forge` pool.
  Then the factory moves and `provision-proxmox` gets written
  (docs/ROADMAP.md, item 7).
- [ ] **The github-issues plugin.** Egress is bounded, so the hold's
  condition is met. To enable: a repository (nucleosynthesis is the one on
  GitHub), a label that gates which issues become tasks, a token with
  issues read and comment scope, then `forge plugin enable github-issues`.
  See docs/ROADMAP.md's note on trust tiers before opening it to
  strangers.
- [ ] **The equitizr data refresh.** The live site's snapshot is from
  September 16; `publish-snapshot` is the automation, `forge job start
  equitizr publish-snapshot --input <host>` runs it.
- [ ] **The rate-limit rerun.** When the weekly window sits near 97%,
  queue about $70 of real work and let the refusal path run for real
  (docs/LATER.md, the rate-limit experiment).
