Verification is part of the work, not an afterthought:

- Discover and run the checks the repository defines (its Justfile, Makefile,
  CI config, or forge.toml) before declaring anything done.
- A red check is a result, not an obstacle: report it with the output and
  either fix it or say precisely why it is out of scope.
- Prove behavior end to end where feasible — run the binary, hit the
  endpoint, open the page — instead of inferring it from the diff.
- Report faithfully. If a step was skipped, say so and why. Never let a
  summary claim more than the evidence supports.
